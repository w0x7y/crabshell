//! polkit's setuid helper, which checks a user's answers with PAM: it
//! takes the user's name as its argument and the request's cookie as its
//! first line of input, prints PAM's prompts and messages a line each,
//! reads an answer line for each prompt, and ends with `SUCCESS` or
//! `FAILURE`. On success it has already told polkit itself.

use std::io::{self, ErrorKind, PipeReader, PipeWriter, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
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
    self.0.fill(0);
    // So the zeroing isn't optimized away as a store nobody reads.
    std::hint::black_box(&self.0);
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
  child: Child,
  stdin: PipeWriter,
  stdout: PipeReader,
  /// The loop's watch on `stdout`.
  pub(super) token: Option<RegistrationToken>,
  lines: Lines,
  /// Which run this is, so a callback for one gone does nothing.
  pub(super) run: u64,
}

impl Helper {
  /// Starts `program` for `user`, with its input and output through
  /// non-blocking pipes and its errors in the shell's log.
  pub(super) fn start(program: &Path, user: &str, run: u64) -> io::Result<Helper> {
    let (stdout, child_out) = std::io::pipe()?;
    let (child_in, stdin) = std::io::pipe()?;
    rustix::io::ioctl_fionbio(&stdout, true)?;
    rustix::io::ioctl_fionbio(&stdin, true)?;
    let mut command = Command::new(program);
    spawn::untune(&mut command);
    // The command holds the helper's ends, which close with it.
    let child = command
      .arg(user)
      .stdin(child_in)
      .stdout(child_out)
      .spawn()?;
    Ok(Helper {
      child,
      stdin,
      stdout,
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
  /// reads the end and exits. `kill` also sends it `SIGKILL`. It is
  /// reaped once it is gone.
  pub(super) fn stop(self, shared: &Rc<Shared>, kill: bool) {
    let Helper {
      mut child, token, ..
    } = self;
    if let (Some(token), Some(event_loop)) = (token, shared.event_loop()) {
      event_loop.remove(token);
    }
    if kill && let Err(error) = child.kill() {
      tracing::debug!(%error, "couldn't stop polkit's helper; it may have ended");
    }
    spawn::reap(shared, child, String::from("polkit's helper"));
  }
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
