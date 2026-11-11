//! What `run` exits with when it can't reach a compositor, with stderr a
//! pipe nobody reads. The test runs this test binary again as the shell.

use std::process::{Command, Stdio};

const CHILD: &str = "CRABSHELL_EXIT_TEST_CHILD";

#[test]
fn with_no_compositor_run_exits_2_even_when_stderr_is_a_closed_pipe() {
  if std::env::var_os(CHILD).is_some() {
    crabshell::run(|_| {});
  }
  let runtime_dir = std::env::temp_dir().join(format!("crabshell-exit-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&runtime_dir);
  std::fs::create_dir_all(&runtime_dir).expect("create an empty runtime directory");
  // A pipe whose read end is closed before the shell starts: every log
  // line it writes fails.
  let (reader, writer) = std::io::pipe().expect("a pipe");
  drop(reader);
  let status = Command::new(std::env::current_exe().expect("the test binary"))
    .args([
      "--exact",
      "with_no_compositor_run_exits_2_even_when_stderr_is_a_closed_pipe",
      "--nocapture",
    ])
    .env(CHILD, "1")
    .env("XDG_RUNTIME_DIR", &runtime_dir)
    .env("WAYLAND_DISPLAY", "no-compositor-here")
    .env_remove("WAYLAND_SOCKET")
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::from(writer))
    .status()
    .expect("the test binary runs");
  let _ = std::fs::remove_dir_all(&runtime_dir);
  assert_eq!(
    status.code(),
    Some(crabshell::EXIT_NO_COMPOSITOR),
    "{status}: a panic writing a log line would give 101"
  );
}
