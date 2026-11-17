//! Starting programs the shell doesn't wait on, such as an app a launcher
//! opens.

use std::cell::Cell;
use std::ffi::OsString;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use calloop::Interest;
use rustix::process::{Pid, PidfdFlags};

use crate::Shared;

/// Starts `command` apart from the shell and returns its process id. It
/// reads nothing, its output goes nowhere, and it leads a process group
/// of its own, so a signal to the shell's group misses it. When it exits,
/// a pidfd in the loop wakes the shell to reap it, so it leaves no zombie
/// and nothing polls. A failure to start is logged with the program's
/// name.
pub(crate) fn spawn(shared: &Rc<Shared>, command: &mut Command) -> io::Result<u32> {
  let program = command.get_program().to_string_lossy().into_owned();
  untune(command);
  let child = command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0)
    .spawn()
    .inspect_err(|error| tracing::warn!(%error, program, "couldn't start a program"))?;
  let pid = child.id();
  reap(shared, child, program);
  Ok(pid)
}

/// glibc's tunable that keeps its threshold for using `mmap` where it is.
/// By default the threshold rises to the size of each large block freed,
/// so after a wallpaper is decoded the next one's buffers come from the
/// heap and stay there: tens of MiB for a 4K image. Kept in step with
/// crabshell-cli's `supervise::TUNABLE`, which a test there checks.
const TUNABLE: &str = "glibc.malloc.mmap_threshold=131072";

/// Where a shell `crabshell::run` starts again leaves the time it first
/// started, so its first frame is counted from then. Not passed on to
/// the programs the shell starts.
#[doc(hidden)]
pub const FIRST_START: &str = "CRABSHELL_STARTED";

/// `GLIBC_TUNABLES` for the shell: `old` with the shell's fixed `mmap`
/// threshold added, or `None` if it sets that threshold already.
/// `crabshell::run` starts the shell again with it when the supervisor
/// didn't, and [`spawn`] keeps it from the programs the shell starts.
#[doc(hidden)]
pub fn shell_tunables(old: Option<OsString>) -> Option<OsString> {
  match old {
    Some(old)
      if old
        .to_string_lossy()
        .contains("glibc.malloc.mmap_threshold") =>
    {
      None
    }
    Some(mut old) if !old.is_empty() => {
      old.push(":");
      old.push(TUNABLE);
      Some(old)
    }
    _ => Some(TUNABLE.into()),
  }
}

/// Takes what is the shell's own out of what `command` inherits, so the
/// programs it starts run as they would anywhere else: the supervisor's
/// socket, which a crabshell started from a launched terminal would
/// otherwise take for its own, the shell's glibc tunable, the time it
/// first started, and the activation token the shell was started with,
/// unless `command` was given one of its own.
pub(crate) fn untune(command: &mut Command) {
  untune_from(command, std::env::var_os("GLIBC_TUNABLES"));
}

/// [`untune`], given the shell's `GLIBC_TUNABLES`.
fn untune_from(command: &mut Command, ours: Option<OsString>) {
  command.env_remove("CRABSHELL_CONTROL");
  command.env_remove(FIRST_START);
  // A token is spent on the window it was given for, the shell's here.
  for name in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
    if !command.get_envs().any(|(set, _)| set == name) {
      command.env_remove(name);
    }
  }
  if ours.is_some_and(|value| value == TUNABLE) {
    command.env_remove("GLIBC_TUNABLES");
  }
}

/// Waits for `child` to end without blocking, through a pidfd in the loop
/// that goes once it is reaped, so it leaves no zombie and nothing polls.
pub(crate) fn reap(shared: &Rc<Shared>, mut child: Child, program: String) {
  // It can't name another process: the child isn't reaped until it turns
  // readable.
  let pidfd = match rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty()) {
    Ok(pidfd) => pidfd,
    Err(error) => {
      tracing::warn!(%error, program, "can't watch a program, so it stays a zombie once it exits");
      return;
    }
  };
  let token = Rc::new(Cell::new(None));
  let reap = {
    let (shared, token) = (Rc::clone(shared), Rc::clone(&token));
    Box::new(move || {
      // Readable means it exited, so this returns at once.
      match child.wait() {
        Ok(status) => tracing::debug!(program, %status, "a program ended"),
        Err(error) => tracing::warn!(%error, program, "couldn't reap a program"),
      }
      if let (Some(token), Some(event_loop)) = (token.take(), shared.event_loop()) {
        event_loop.remove(token);
      }
    })
  };
  if let Some(event_loop) = shared.event_loop() {
    token.set(event_loop.insert_fd(pidfd, Interest::READ, reap));
  }
}

#[cfg(test)]
mod tests {
  use std::process::Command;
  use std::rc::Rc;
  use std::time::{Duration, Instant};

  use super::spawn;
  use crate::Shared;
  use crate::testing::{Harness, capture_logs, offline};

  fn shared(shell: &Harness) -> Rc<Shared> {
    Rc::new(Shared::new(
      shell.cx.owner(),
      &shell.event_loop.handle(),
      offline(),
    ))
  }

  #[test]
  fn a_program_runs_apart_and_is_reaped_when_it_exits() {
    let mut shell = Harness::new(offline());
    let shared = shared(&shell);
    let pid = spawn(&shared, Command::new("sleep").arg("0.3")).expect("sleep starts");

    let proc = format!("/proc/{pid}");
    let stat = std::fs::read_to_string(format!("{proc}/stat")).expect("it runs");
    // After the name in parentheses: the state, the parent, the group.
    let fields: Vec<&str> = stat
      .rsplit_once(')')
      .expect("a stat line")
      .1
      .split_whitespace()
      .collect();
    assert_eq!(fields[2], pid.to_string(), "its own process group: {stat}");
    for fd in 0..3 {
      let target = std::fs::read_link(format!("{proc}/fd/{fd}")).expect("an fd");
      assert_eq!(target, std::path::Path::new("/dev/null"), "fd {fd}");
    }

    // One wakeup for its exit, one for the end of the run: no polling.
    let wakeups = shell.run_until(Instant::now() + Duration::from_millis(800));
    assert!(wakeups <= 2, "{wakeups} wakeups");
    assert!(
      !std::path::Path::new(&proc).exists(),
      "reaped, not a zombie"
    );
    // Its pidfd left the loop, or it would wake the loop over and over.
    let wakeups = shell.run_until(Instant::now() + Duration::from_millis(200));
    assert_eq!(wakeups, 1);
  }

  #[test]
  fn a_program_that_cant_start_is_logged_and_returned() {
    let shell = Harness::new(offline());
    let shared = shared(&shell);
    let logs = capture_logs(|| {
      let started = spawn(&shared, &mut Command::new("/nonexistent/crabshell-program"));
      assert!(started.is_err());
    });
    assert!(logs.contains("couldn't start"), "{logs}");
    assert!(logs.contains("/nonexistent/crabshell-program"), "{logs}");
  }

  #[test]
  fn the_shell_gets_a_fixed_mmap_threshold_added_to_the_users_tunables() {
    use super::{TUNABLE, shell_tunables};
    let tunables = |old: Option<&str>| shell_tunables(old.map(Into::into));
    assert_eq!(tunables(None), Some(TUNABLE.into()));
    assert_eq!(tunables(Some("")), Some(TUNABLE.into()));
    assert_eq!(
      tunables(Some("glibc.malloc.arena_max=2")),
      Some(format!("glibc.malloc.arena_max=2:{TUNABLE}").into())
    );
    let own = "glibc.malloc.mmap_threshold=65536";
    assert_eq!(tunables(Some(own)), None, "the user's own wins");
  }

  #[test]
  fn the_shells_own_glibc_tunable_isnt_passed_on() {
    use std::ffi::OsStr;
    let removed = |ours: Option<&str>| {
      let mut command = Command::new("true");
      super::untune_from(&mut command, ours.map(Into::into));
      command
        .get_envs()
        .any(|(name, value)| name == OsStr::new("GLIBC_TUNABLES") && value.is_none())
    };
    assert!(removed(Some(super::TUNABLE)));
    assert!(!removed(Some("glibc.malloc.arena_max=2")), "the user's own");
    assert!(!removed(None));
    let mut command = Command::new("true");
    super::untune_from(&mut command, None);
    let control = command
      .get_envs()
      .any(|(name, value)| name == OsStr::new("CRABSHELL_CONTROL") && value.is_none());
    assert!(control, "the supervisor's socket isn't passed on");
    let started = command
      .get_envs()
      .any(|(name, value)| name == OsStr::new(super::FIRST_START) && value.is_none());
    assert!(started, "the shell's first start isn't passed on");
  }

  #[test]
  fn the_shells_activation_token_isnt_passed_on_but_one_set_is() {
    use std::ffi::OsStr;
    let token = |command: &Command, name: &str| {
      let set = command.get_envs().find(|(set, _)| *set == OsStr::new(name));
      set.map(|(_, value)| value.map(OsStr::to_owned))
    };
    let mut command = Command::new("true");
    super::untune_from(&mut command, None);
    for name in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
      assert_eq!(token(&command, name), Some(None), "{name} removed");
    }
    let mut command = Command::new("true");
    crate::activation::set_env(&mut command, "ours");
    super::untune_from(&mut command, None);
    for name in ["XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID"] {
      assert_eq!(
        token(&command, name),
        Some(Some("ours".into())),
        "{name} kept"
      );
    }
  }
}
