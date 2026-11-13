//! `crabshell run`: builds the config, starts the shell and starts it again
//! when it crashes, falling back to `last-good` when a new build keeps
//! crashing. One thread, on a calloop loop that sleeps until the shell
//! exits, talks or a timer is due.

use std::io::{ErrorKind, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Instant;

use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Dispatcher, EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use rustix::process::{Pid, PidfdFlags};

use crate::build;
use crate::policy::{Binary, Exit, HEALTHY_AFTER, Next, Policy};

/// The variable that names the control socket for the shell.
const CONTROL: &str = "CRABSHELL_CONTROL";

/// The signals that stop `crabshell run`, and the shell with it.
const STOP_SIGNALS: [Signal; 2] = [Signal::SIGINT, Signal::SIGTERM];

/// The longest line read from the shell; a longer one is cut.
const MAX_LINE: usize = 256;

/// What the supervisor knows while it runs.
struct Supervisor {
  handle: LoopHandle<'static, Supervisor>,
  /// The stop signals, which `start` unblocks around each spawn.
  signals: Dispatcher<'static, Signals, Supervisor>,
  /// The program the config built; `None` if the first build failed and
  /// `last-good` runs instead.
  build: Option<PathBuf>,
  last_good: PathBuf,
  socket: PathBuf,
  policy: Policy,
  /// The running shell and when it started.
  shell: Option<(Child, Instant)>,
  /// The timer that marks the running shell healthy.
  healthy_timer: Option<RegistrationToken>,
  /// SIGINT or SIGTERM came: the shell's exit ends `run`.
  stopping: bool,
  /// Set when `run` is done.
  result: Option<Result<(), String>>,
}

impl Drop for Supervisor {
  /// `run` is ending with the shell still up, as after a loop error: the
  /// shell goes too, rather than run on unsupervised.
  fn drop(&mut self) {
    if let Some((mut child, _)) = self.shell.take() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

/// Removes the control socket when `run` ends, however it ends.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

/// Runs the config in `config`, built into `target`, until the shell exits
/// on purpose, loses its compositor, or the supervisor is told to stop.
pub(crate) fn run(config: &Path, target: &Path) -> Result<(), String> {
  let runtime = std::env::var_os("XDG_RUNTIME_DIR")
    .map(PathBuf::from)
    .filter(|dir| dir.is_absolute())
    .ok_or("XDG_RUNTIME_DIR isn't set, so there is no session to run a shell in")?
    .join("crabshell");
  private_dir(&runtime)?;
  let socket = runtime.join(format!("control-{}.sock", std::process::id()));
  // One left by an earlier supervisor with this process id.
  let _ = std::fs::remove_file(&socket);
  let listener = UnixListener::bind(&socket)
    .map_err(|error| format!("can't listen on {}: {error}", socket.display()))?;
  let _socket_file = SocketFile(socket.clone());
  listener
    .set_nonblocking(true)
    .map_err(|error| format!("can't listen on {}: {error}", socket.display()))?;

  // After the socket, so a session problem shows before a long build.
  let last_good = target.with_file_name("last-good");
  let (build, binary) = match build::build(config, target) {
    Ok(program) => (Some(program), Binary::Build),
    Err(problem) if last_good.is_file() => {
      log(&format!("{problem}; starting last-good"));
      (None, Binary::LastGood)
    }
    Err(problem) => return Err(problem),
  };

  let mut event_loop: EventLoop<'static, Supervisor> =
    EventLoop::try_new().map_err(|error| format!("can't make the event loop: {error}"))?;
  let handle = event_loop.handle();
  let accepting = handle.clone();
  handle
    .insert_source(
      Generic::new(listener, Interest::READ, Mode::Level),
      move |_, listener, _| {
        loop {
          match listener.accept() {
            Ok((stream, _)) => listen(&accepting, stream),
            Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(PostAction::Continue),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
              log(&format!("can't accept the shell's connection: {error}"));
              return Ok(PostAction::Continue);
            }
          }
        }
      },
    )
    .map_err(|error| format!("can't listen for the shell: {}", error.error))?;
  let signals =
    Signals::new(&STOP_SIGNALS).map_err(|error| format!("can't watch for signals: {error}"))?;
  let signals = Dispatcher::new(signals, |_, _, supervisor: &mut Supervisor| {
    supervisor.stop()
  });
  handle
    .register_dispatcher(signals.clone())
    .map_err(|error| format!("can't watch for signals: {error}"))?;

  let mut supervisor = Supervisor {
    handle,
    signals,
    build,
    last_good,
    socket,
    policy: Policy::new(binary),
    shell: None,
    healthy_timer: None,
    stopping: false,
    result: None,
  };
  supervisor.start();
  loop {
    if let Some(result) = supervisor.result.take() {
      return result;
    }
    event_loop
      .dispatch(None, &mut supervisor)
      .map_err(|error| format!("the event loop failed: {error}"))?;
  }
}

impl Supervisor {
  /// Starts the binary the policy picks, and watches for its exit.
  fn start(&mut self) {
    let program = match self.policy.binary {
      Binary::Build => self
        .build
        .clone()
        .expect("the policy picks the build only when there is one"),
      Binary::LastGood => self.last_good.clone(),
    };
    let now = Instant::now();
    self.policy.started(now);
    // A child inherits the blocked signals signalfd needs, so they are
    // unblocked while it starts; one arriving just then ends the
    // supervisor as if it had no handler.
    if let Err(error) = self.signals.as_source_mut().remove_signals(&STOP_SIGNALS) {
      log(&format!("can't unblock signals for the shell: {error}"));
    }
    let spawned = Command::new(&program)
      .env(CONTROL, &self.socket)
      .stdin(Stdio::null())
      .spawn();
    if let Err(error) = self.signals.as_source_mut().add_signals(&STOP_SIGNALS) {
      log(&format!("can't watch for signals again: {error}"));
    }
    let mut child = match spawned {
      Ok(child) => child,
      Err(error) => {
        return self.ended(
          Exit::Crashed,
          &format!("{} didn't start: {error}", program.display()),
        );
      }
    };
    // A pidfd turns readable when the process exits, so the loop sleeps
    // until then. It can't name another process: the child isn't reaped
    // until `exited`.
    let watched = rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty())
      .map_err(|error| error.to_string())
      .and_then(|pidfd| {
        self
          .handle
          .insert_source(
            Generic::new(pidfd, Interest::READ, Mode::Level),
            |_, _, supervisor| {
              supervisor.exited();
              Ok(PostAction::Remove)
            },
          )
          .map_err(|error| error.error.to_string())
      });
    if let Err(error) = watched {
      let _ = child.kill();
      let _ = child.wait();
      self.result = Some(Err(format!(
        "can't watch the shell ({error}); crabshell run needs Linux 5.3 or later"
      )));
      return;
    }
    self.shell = Some((child, now));
    self.healthy_timer = self
      .handle
      .insert_source(Timer::from_duration(HEALTHY_AFTER), |_, _, supervisor| {
        supervisor.healthy();
        TimeoutAction::Drop
      })
      .map_err(|error| log(&format!("can't time the shell: {}", error.error)))
      .ok();
  }

  /// The shell has run for [`HEALTHY_AFTER`]: a build is kept as
  /// `last-good`.
  fn healthy(&mut self) {
    self.healthy_timer = None;
    self.policy.healthy();
    if self.policy.binary != Binary::Build {
      return;
    }
    let build = self.build.as_ref().expect("the build is running");
    match keep(build, &self.last_good) {
      Ok(()) => log(&format!(
        "the shell ran {} s, so its build is kept as {}",
        HEALTHY_AFTER.as_secs(),
        self.last_good.display()
      )),
      Err(error) => log(&format!(
        "can't keep the build as {}: {error}",
        self.last_good.display()
      )),
    }
  }

  /// The shell's pidfd turned readable: it exited.
  fn exited(&mut self) {
    let Some((mut child, _)) = self.shell.take() else {
      return;
    };
    if let Some(timer) = self.healthy_timer.take() {
      self.handle.remove(timer);
    }
    match child.wait() {
      Ok(status) => self.ended(
        Exit::from_code(status.code()),
        &format!("the shell ended ({status})"),
      ),
      Err(error) => self.ended(
        Exit::Crashed,
        &format!("can't read how the shell ended: {error}"),
      ),
    }
  }

  /// The shell ended as `exit`, described by `how`: stop, or start it
  /// again when the policy says.
  fn ended(&mut self, exit: Exit, how: &str) {
    if self.stopping {
      self.result = Some(Ok(()));
      return;
    }
    let was = self.policy.binary;
    match self
      .policy
      .exited(Instant::now(), exit, self.last_good.is_file())
    {
      Next::Stop => {
        log(&format!("{how}; stopping"));
        self.result = Some(Ok(()));
      }
      Next::Restart { after, binary } => {
        if binary != was {
          log(&format!(
            "{how}; the new build keeps crashing, so last-good starts in {} s",
            after.as_secs()
          ));
        } else {
          log(&format!(
            "{how}; starting it again in {} s",
            after.as_secs()
          ));
        }
        let inserted =
          self
            .handle
            .insert_source(Timer::from_duration(after), |_, _, supervisor| {
              if !supervisor.stopping {
                supervisor.start();
              }
              TimeoutAction::Drop
            });
        if let Err(error) = inserted {
          self.result = Some(Err(format!("can't wait to restart: {}", error.error)));
        }
      }
    }
  }

  /// The shell said `ready`.
  fn ready(&self) {
    if let Some((_, started)) = &self.shell {
      log(&format!(
        "the shell is ready, {} ms after it started",
        started.elapsed().as_millis()
      ));
    }
  }

  /// SIGINT or SIGTERM: ends the shell, then `run`.
  fn stop(&mut self) {
    self.stopping = true;
    match &self.shell {
      Some((child, _)) => {
        let _ =
          rustix::process::kill_process(Pid::from_child(child), rustix::process::Signal::TERM);
      }
      // Waiting to restart: nothing to end.
      None => self.result = Some(Ok(())),
    }
  }
}

/// Reads the shell's lines from `stream` on the loop.
fn listen(handle: &LoopHandle<'static, Supervisor>, stream: UnixStream) {
  if let Err(error) = stream.set_nonblocking(true) {
    return log(&format!("can't read from the shell: {error}"));
  }
  let mut line = Vec::new();
  let inserted = handle.insert_source(
    Generic::new(stream, Interest::READ, Mode::Level),
    move |_, stream, supervisor| {
      let mut chunk = [0_u8; 64];
      loop {
        // Through a shared reference: calloop lends the source read-only.
        match (&**stream).read(&mut chunk) {
          Ok(0) => return Ok(PostAction::Remove),
          Ok(read) => {
            for &byte in &chunk[..read] {
              if byte != b'\n' {
                if line.len() < MAX_LINE {
                  line.push(byte);
                }
                continue;
              }
              match line.as_slice() {
                b"ready" => supervisor.ready(),
                other => log(&format!(
                  "the shell sent something unknown: {}",
                  String::from_utf8_lossy(other)
                )),
              }
              line.clear();
            }
          }
          Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(PostAction::Continue),
          Err(error) if error.kind() == ErrorKind::Interrupted => {}
          Err(_) => return Ok(PostAction::Remove),
        }
      }
    },
  );
  if let Err(error) = inserted {
    log(&format!("can't read from the shell: {}", error.error));
  }
}

/// Creates `dir` for this user only. One others can reach is made private;
/// one that is a symlink is refused.
fn private_dir(dir: &Path) -> Result<(), String> {
  let failed = |error: std::io::Error| format!("can't use {}: {error}", dir.display());
  std::fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(dir)
    .map_err(failed)?;
  let meta = std::fs::symlink_metadata(dir).map_err(failed)?;
  if !meta.is_dir() {
    return Err(format!("{} isn't a directory", dir.display()));
  }
  if meta.permissions().mode() & 0o077 != 0 {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(failed)?;
  }
  Ok(())
}

/// Copies `build` to `last_good` through a file beside it, so a crash
/// mid-copy never leaves half a program there.
fn keep(build: &Path, last_good: &Path) -> std::io::Result<()> {
  let partial = last_good.with_file_name("last-good.partial");
  std::fs::copy(build, &partial)?;
  std::fs::rename(&partial, last_good)
}

/// Logs `message` to stderr. A closed stderr is ignored rather than a
/// panic, which would leave the shell unsupervised.
fn log(message: &str) {
  use std::io::Write;
  let _ = writeln!(std::io::stderr(), "crabshell: {message}");
}
