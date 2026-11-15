//! polkit's helper, which checks a user's answers with PAM, as root. From
//! polkit 127 systemd starts it for each connection to polkit's socket,
//! which reads the user's name as its first line; before, it is a setuid
//! program that takes the name as its argument. Either way the request's
//! cookie comes next as a line, then it prints PAM's prompts and messages
//! a line each, reads an answer line for each prompt, and ends with
//! `SUCCESS` or `FAILURE`. On success it has already told polkit itself.

use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command};
use std::rc::Rc;

use calloop::RegistrationToken;

use super::{MAX_TEXT, cut};
use crate::lines::Lines;
use crate::{Shared, spawn};

/// A response on its way to the helper. It has no `Debug`, so it can't be
/// logged, and its bytes are zeroed when it goes.
pub(super) struct Secret(pub(super) Vec<u8>);

impl Drop for Secret {
  fn drop(&mut self) {
    // Its spare room too, which may hold bytes typed and deleted.
    crate::pam::wipe(&mut self.0);
  }
}

/// One line the helper printed.
#[derive(Debug, PartialEq)]
pub(super) enum Line {
  /// PAM asks something: the question, and whether the answer may show.
  Prompt {
    message: String,
    visible: bool,
  },
  Error(String),
  Info(String),
  Success,
  Failure,
  Unknown,
}

/// What `line` says, with its text cut to 4 KiB.
pub(super) fn parse(line: &str) -> Line {
  let (word, text) = line.split_once(' ').unwrap_or((line, ""));
  let text = || cut(text, MAX_TEXT);
  match word {
    "PAM_PROMPT_ECHO_OFF" => Line::Prompt {
      message: text(),
      visible: false,
    },
    "PAM_PROMPT_ECHO_ON" => Line::Prompt {
      message: text(),
      visible: true,
    },
    "PAM_ERROR_MSG" => Line::Error(text()),
    "PAM_TEXT_INFO" => Line::Info(text()),
    _ if line == "SUCCESS" => Line::Success,
    "FAILURE" => Line::Failure,
    _ => Line::Unknown,
  }
}

/// One run of the helper.
pub(super) struct Helper {
  /// The setuid helper, or `None` when systemd started it on the socket.
  child: Option<Child>,
  stdin: File,
  stdout: File,
  /// The loop's watch on `stdout`.
  pub(super) token: Option<RegistrationToken>,
  lines: Lines,
  /// Which run this is, so a callback for one gone does nothing.
  pub(super) run: u64,
}

impl Helper {
  /// Starts a run for `user` through polkit's `socket`, or, when nothing
  /// listens there, by spawning `program`, with its errors in the shell's
  /// log. Either way its input and output are non-blocking.
  pub(super) fn start(program: &Path, socket: &Path, user: &str, run: u64) -> io::Result<Helper> {
    let (child, stdin, stdout) = match connect(socket, user) {
      Some(stream) => {
        let stdout = stream.try_clone()?;
        (None, OwnedFd::from(stream), OwnedFd::from(stdout))
      }
      None => {
        let (stdout, child_out) = std::io::pipe()?;
        let (child_in, stdin) = std::io::pipe()?;
        let mut command = Command::new(program);
        spawn::untune(&mut command);
        // The command holds the helper's ends, which close with it.
        let child = command
          .arg(user)
          .stdin(child_in)
          .stdout(child_out)
          .spawn()?;
        (Some(child), OwnedFd::from(stdin), OwnedFd::from(stdout))
      }
    };
    rustix::io::ioctl_fionbio(&stdout, true)?;
    rustix::io::ioctl_fionbio(&stdin, true)?;
    Ok(Helper {
      child,
      stdin: File::from(stdin),
      stdout: File::from(stdout),
      token: None,
      lines: Lines::default(),
      run,
    })
  }

  /// Writes `bytes` as one line. The helper reads each line before it
  /// asks for the next, so a short one fits in the pipe; one that doesn't
  /// is an error rather than a wait.
  pub(super) fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
    self.stdin.write_all(bytes)?;
    self.stdin.write_all(b"\n")
  }

  /// A copy of its output's fd, for the loop to watch. `None`, logged,
  /// if it couldn't be copied.
  pub(super) fn stdout_fd(&self) -> Option<OwnedFd> {
    let fd = self.stdout.as_fd().try_clone_to_owned();
    fd.inspect_err(|error| tracing::warn!(%error, "couldn't copy polkit's helper's output"))
      .ok()
  }

  /// The whole lines it printed since the last read, and whether it
  /// closed its output. `None` while nothing is there.
  pub(super) fn read(&mut self) -> Option<(Vec<String>, bool)> {
    let mut chunk = [0; 4096];
    match self.stdout.read(&mut chunk) {
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => None,
      Ok(read @ 1..) => Some((self.lines.push(&chunk[..read], &"polkit's helper"), false)),
      _ => Some((self.lines.finish().into_iter().collect(), true)),
    }
  }

  /// Stops reading it and closes its input, so a helper still running
  /// reads the end and exits. `kill` also sends a spawned one `SIGKILL`;
  /// it is reaped once it is gone.
  pub(super) fn stop(self, shared: &Rc<Shared>, kill: bool) {
    let Helper { child, token, .. } = self;
    if let (Some(token), Some(event_loop)) = (token, shared.event_loop()) {
      event_loop.remove(token);
    }
    let Some(mut child) = child else {
      return;
    };
    if kill && let Err(error) = child.kill() {
      tracing::debug!(%error, "couldn't stop polkit's helper; it may have ended");
    }
    spawn::reap(shared, child, String::from("polkit's helper"));
  }
}

/// A connection to polkit's helper on `socket`, told `user`. `None` when
/// there is no socket, or, logged, when it can't be used.
fn connect(socket: &Path, user: &str) -> Option<UnixStream> {
  let mut stream = match UnixStream::connect(socket) {
    Ok(stream) => stream,
    Err(error) if error.kind() == ErrorKind::NotFound => return None,
    Err(error) => {
      let socket = socket.display();
      tracing::warn!(%error, %socket, "couldn't reach polkit's helper; spawning it instead");
      return None;
    }
  };
  let told = stream
    .write_all(user.as_bytes())
    .and_then(|()| stream.write_all(b"\n"));
  told
    .inspect_err(|error| tracing::warn!(%error, "couldn't tell polkit's helper the user"))
    .ok()
    .map(|()| stream)
}

#[cfg(test)]
mod tests {
  use super::{Line, parse};

  #[test]
  fn the_helpers_lines_are_read_as_pams_conversation() {
    let prompt = |message: &str, visible| Line::Prompt {
      message: message.to_owned(),
      visible,
    };
    assert_eq!(
      parse("PAM_PROMPT_ECHO_OFF Password: "),
      prompt("Password: ", false)
    );
    assert_eq!(parse("PAM_PROMPT_ECHO_ON Login:"), prompt("Login:", true));
    assert_eq!(parse("PAM_ERROR_MSG Sorry."), Line::Error("Sorry.".into()));
    assert_eq!(
      parse("PAM_TEXT_INFO Touch it"),
      Line::Info("Touch it".into())
    );
    assert_eq!(parse("SUCCESS"), Line::Success);
    assert_eq!(parse("SUCCESS and more"), Line::Unknown);
    assert_eq!(parse("FAILURE"), Line::Failure);
    assert_eq!(parse("PAM_PROMPT_ECHO_OFF"), prompt("", false));
    assert_eq!(parse("something else"), Line::Unknown);
    let long = format!("PAM_TEXT_INFO {}", "é".repeat(4096));
    let Line::Info(cut) = parse(&long) else {
      panic!("an info line");
    };
    assert_eq!(cut.len(), 4096);
  }
}
