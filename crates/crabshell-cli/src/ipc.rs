//! `crabshell ipc`: calls the running shell's `cx.ipc` handlers through
//! its socket, `$XDG_RUNTIME_DIR/crabshell/ipc-<display>.sock`. The shell's
//! side is in the `crabshell` crate; the tests hold both to one format.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// How long the whole exchange may take.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// The longest reply read: the shell's 1 MiB of text, escaped.
const MAX_REPLY: u64 = 8 << 20;

/// What `crabshell ipc` asks the shell.
#[derive(Debug, PartialEq)]
pub(crate) enum Ask {
  List,
  Call {
    target: String,
    function: String,
    args: Vec<String>,
  },
}

/// `<runtime_dir>/crabshell/ipc-<display>.sock`, where `<display>` is the
/// last part of `WAYLAND_DISPLAY`, or `wayland-0`. `None` without an
/// absolute runtime dir. The shell finds it the same way.
pub(crate) fn socket_path(runtime_dir: Option<&OsStr>, display: Option<&OsStr>) -> Option<PathBuf> {
  let dir = Path::new(runtime_dir?);
  if !dir.is_absolute() {
    return None;
  }
  let display = display
    .and_then(|display| Path::new(display).file_name())
    .unwrap_or(OsStr::new("wayland-0"));
  let mut name = OsString::from("ipc-");
  name.push(display);
  name.push(".sock");
  Some(dir.join("crabshell").join(name))
}

/// The request line for `ask`.
pub(crate) fn request(ask: &Ask) -> String {
  let value = match ask {
    Ask::List => serde_json::json!({ "list": true }),
    Ask::Call {
      target,
      function,
      args,
    } => serde_json::json!({ "target": target, "function": function, "args": args }),
  };
  format!("{value}\n")
}

/// The text of the shell's reply, `{"ok":…}`, or why it failed.
pub(crate) fn read_reply(reply: &[u8]) -> Result<String, String> {
  if reply.is_empty() {
    return Err(String::from("the shell hung up without answering"));
  }
  let line = reply.split(|&byte| byte == b'\n').next().unwrap_or(reply);
  let value: serde_json::Value = serde_json::from_slice(line)
    .map_err(|error| format!("the shell's reply doesn't parse: {error}"))?;
  match (&value["ok"], &value["error"]) {
    (serde_json::Value::String(text), serde_json::Value::Null) => Ok(text.clone()),
    (serde_json::Value::Null, serde_json::Value::String(text)) => Err(text.clone()),
    _ => Err(format!(
      "the shell's reply isn't one crabshell knows: {value}"
    )),
  }
}

/// `text` as printed: with a newline added if it lacks one, and nothing
/// if it is empty.
pub(crate) fn shown(text: &str) -> String {
  let mut shown = text.to_owned();
  if !shown.is_empty() && !shown.ends_with('\n') {
    shown.push('\n');
  }
  shown
}

/// Sends `line` to the shell at `path` and returns its reply's text, all
/// within `timeout`.
pub(crate) fn ask(path: &Path, line: String, timeout: Duration) -> Result<String, String> {
  let (sender, receiver) = std::sync::mpsc::channel();
  let at = path.to_owned();
  // On a thread, so a connect stuck on a full backlog is timed too.
  std::thread::spawn(move || sender.send(exchange(&at, &line)));
  receiver.recv_timeout(timeout).unwrap_or_else(|_| {
    Err(format!(
      "the shell at {} didn't answer within {} s",
      path.display(),
      timeout.as_secs_f64()
    ))
  })
}

fn exchange(path: &Path, line: &str) -> Result<String, String> {
  let mut stream = UnixStream::connect(path).map_err(|error| match error.kind() {
    ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
      format!("no shell is listening at {}", path.display())
    }
    _ => format!("can't reach the shell at {}: {error}", path.display()),
  })?;
  // A shell that refused the connection has said why already; it's read
  // below even if this fails.
  let _ = stream.write_all(line.as_bytes());
  // One line: read to its end, not to the connection's, which a shell
  // closing with input unread resets after the reply arrived.
  let mut reply = Vec::new();
  std::io::BufReader::new(stream.take(MAX_REPLY))
    .read_until(b'\n', &mut reply)
    .map_err(|error| format!("can't read the shell's reply: {error}"))?;
  read_reply(&reply)
}

/// Runs `crabshell ipc`: prints the reply's text, or fails with why.
pub(crate) fn run(ask_for: &Ask) -> Result<ExitCode, String> {
  let path = socket_path(
    std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
    std::env::var_os("WAYLAND_DISPLAY").as_deref(),
  )
  .ok_or("XDG_RUNTIME_DIR isn't set, so no shell is listening")?;
  let text = ask(&path, request(ask_for), TIMEOUT)?;
  // A closed stdout, as in `| head`, isn't the shell's failure.
  let _ = std::io::stdout().write_all(shown(&text).as_bytes());
  Ok(ExitCode::SUCCESS)
}
