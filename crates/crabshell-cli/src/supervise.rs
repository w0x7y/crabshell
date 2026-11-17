//! `crabshell run`: builds the config, starts the shell and starts it again
//! when it crashes, falling back to `last-good` when a new build keeps
//! crashing. An edit to the config rebuilds it and swaps the shell. One
//! thread, on a calloop loop that sleeps until the shell exits or talks,
//! a file changes or a timer is due.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use calloop::futures::Scheduler;
use calloop::generic::Generic;
use calloop::signals::{Signal, Signals};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Dispatcher, EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use rustix::process::{Pid, PidfdFlags};

use crate::build::{self, Built};
use crate::policy::{Binary, Exit, HEALTHY_AFTER, Next, Policy};
use crate::watch::Watcher;
use crate::{log, notify};

/// The variable that names the control socket for the shell.
const CONTROL: &str = "CRABSHELL_CONTROL";

/// The signals that stop `crabshell run`, and the shell with it.
const STOP_SIGNALS: [Signal; 2] = [Signal::SIGINT, Signal::SIGTERM];

/// How long the files must be quiet after an edit before a rebuild, so a
/// save that writes several files builds once.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// How long the old shell gets to save and exit before it is killed.
const SAVE_LIMIT: Duration = Duration::from_secs(5);

/// The notification's title when the config doesn't build.
const DIDNT_BUILD: &str = "Your Crabshell config didn't build";

/// The longest line read from the shell; a longer one is cut.
const MAX_LINE: usize = 256;

/// What the supervisor knows while it runs.
struct Supervisor {
  handle: LoopHandle<'static, Supervisor>,
  /// Runs the notifications' D-Bus calls on the loop.
  scheduler: Scheduler<()>,
  /// The stop signals, which `start` unblocks around each spawn.
  signals: Dispatcher<'static, Signals, Supervisor>,
  /// The program the config built; `None` if the first build failed and
  /// `last-good` runs instead.
  build: Option<PathBuf>,
  config: PathBuf,
  target: PathBuf,
  last_good: PathBuf,
  socket: PathBuf,
  policy: Policy,
  /// The config's file watches; `None` if inotify failed.
  watcher: Option<Watcher>,
  /// The running shell and when it started.
  shell: Option<(Child, Instant)>,
  /// The running shell's end of the control socket, once it connects.
  control: Option<UnixStream>,
  /// The timer that marks the running shell healthy.
  healthy_timer: Option<RegistrationToken>,
  /// The timer that starts the shell again after a crash.
  restart_timer: Option<RegistrationToken>,
  /// The timer that rebuilds once the files are quiet.
  debounce_timer: Option<RegistrationToken>,
  /// A new build waits for the old shell to end; the timer kills it if
  /// it takes too long.
  reloading: Option<RegistrationToken>,
  /// When the old shell ended in a reload, until the new one is ready.
  reload_gap: Option<Instant>,
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

  // Before the first build, so an edit saved while it runs is seen.
  let watcher = Watcher::new(config);

  // After the socket, so a session problem shows before a long build.
  let last_good = target.with_file_name("last-good");
  let (build, binary, problem) = match timed_build(config, target) {
    Ok(built) => (Some(built.program), Binary::Build, None),
    Err(problem) if last_good.is_file() => {
      log(&format!(
        "the config didn't build: {problem}; starting last-good"
      ));
      (None, Binary::LastGood, Some(problem))
    }
    Err(problem) => return Err(format!("the config didn't build: {problem}")),
  };

  let mut event_loop: EventLoop<'static, Supervisor> =
    EventLoop::try_new().map_err(|error| format!("can't make the event loop: {error}"))?;
  let handle = event_loop.handle();
  let accepting = handle.clone();
  handle
    .insert_source(
      Generic::new(listener, Interest::READ, Mode::Level),
      move |_, listener, supervisor| {
        loop {
          match listener.accept() {
            Ok((stream, _)) => listen(&accepting, supervisor, stream),
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

  let (executor, scheduler) =
    calloop::futures::executor().map_err(|error| format!("can't make the executor: {error}"))?;
  handle
    .insert_source(executor, |(), _, _| {})
    .map_err(|error| format!("can't make the executor: {}", error.error))?;
  let watcher = match watcher {
    Ok(watcher) => watcher
      .fd()
      .map_err(|error| error.to_string())
      .and_then(|fd| {
        // A copy of the descriptor wakes the loop; the watcher itself
        // stays in the state, where reading it needs `&mut`.
        handle
          .insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            |_, _, supervisor| {
              supervisor.files_changed();
              Ok(PostAction::Continue)
            },
          )
          .map_err(|error| error.error.to_string())
      })
      .map(|_| watcher),
    Err(error) => Err(error.to_string()),
  };
  let watcher = watcher
    .map_err(|error| {
      log(&format!(
        "can't watch the config ({error}); edits won't reload it"
      ))
    })
    .ok();

  let mut supervisor = Supervisor {
    handle,
    scheduler,
    signals,
    build,
    config: config.to_owned(),
    target: target.to_owned(),
    last_good,
    socket,
    policy: Policy::new(binary),
    watcher,
    shell: None,
    control: None,
    healthy_timer: None,
    restart_timer: None,
    debounce_timer: None,
    reloading: None,
    reload_gap: None,
    stopping: false,
    result: None,
  };
  if let Some(problem) = problem {
    notify::send(
      &supervisor.handle,
      &supervisor.scheduler,
      DIDNT_BUILD,
      &format!("{problem}; the last config that worked runs instead"),
    );
  }
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
      .env(
        "GLIBC_TUNABLES",
        tunables(std::env::var_os("GLIBC_TUNABLES")),
      )
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
  }

  /// The shell has run for [`HEALTHY_AFTER`] since it said `ready`: a
  /// build is kept as `last-good`.
  fn healthy(&mut self) {
    self.healthy_timer = None;
    self.policy.healthy();
    let Some((child, _)) = &self.shell else {
      return;
    };
    if self.policy.binary != Binary::Build {
      return;
    }
    // The program that ran, which a newer build at the same path may
    // have replaced on disk.
    let ran = PathBuf::from(format!("/proc/{}/exe", child.id()));
    match keep(&ran, &self.last_good) {
      Ok(()) => log(&format!(
        "the shell ran {} s after it said ready, so its build is kept as {}",
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
    self.control = None;
    if let Some(timer) = self.reloading.take() {
      self.handle.remove(timer);
      let status = child
        .wait()
        .map_or_else(|error| error.to_string(), |status| status.to_string());
      log(&format!(
        "the old shell ended ({status}); starting the new build"
      ));
      if self.stopping {
        self.result = Some(Ok(()));
      } else {
        self.reload_gap = Some(Instant::now());
        self.start();
      }
      return;
    }
    // A new build that ended before `ready` didn't reload.
    self.reload_gap = None;
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
          notify::send(
            &self.handle,
            &self.scheduler,
            "Your Crabshell config keeps crashing",
            "It crashed three times in its first minute, so the last config that worked runs instead.",
          );
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
              supervisor.restart_timer = None;
              if !supervisor.stopping {
                supervisor.start();
              }
              TimeoutAction::Drop
            });
        match inserted {
          Ok(timer) => self.restart_timer = Some(timer),
          Err(error) => self.result = Some(Err(format!("can't wait to restart: {}", error.error))),
        }
      }
    }
  }

  /// The shell said `ready`: it counts as healthy [`HEALTHY_AFTER`] from
  /// now.
  fn ready(&mut self) {
    let Some((_, started)) = &self.shell else {
      return;
    };
    log(&format!(
      "the shell is ready, {} ms after it started",
      started.elapsed().as_millis()
    ));
    if let Some(ended) = self.reload_gap.take() {
      log(&format!(
        "reloaded with {} ms between the old shell's exit and the new one's ready",
        ended.elapsed().as_millis()
      ));
    }
    let Some(due) = self.policy.ready(Instant::now()) else {
      return;
    };
    self.healthy_timer = self
      .handle
      .insert_source(Timer::from_deadline(due), |_, _, supervisor| {
        supervisor.healthy();
        TimeoutAction::Drop
      })
      .map_err(|error| log(&format!("can't time the shell: {}", error.error)))
      .ok();
  }

  /// The watcher woke the loop: rebuild once the files are quiet for
  /// [`DEBOUNCE`].
  fn files_changed(&mut self) {
    let Some(watcher) = self.watcher.as_mut() else {
      return;
    };
    if !watcher.changed() {
      return;
    }
    if let Some(timer) = self.debounce_timer.take() {
      self.handle.remove(timer);
    }
    self.debounce_timer = self
      .handle
      .insert_source(Timer::from_duration(DEBOUNCE), |_, _, supervisor| {
        supervisor.debounce_timer = None;
        supervisor.rebuild();
        TimeoutAction::Drop
      })
      .map_err(|error| {
        log(&format!(
          "can't wait for the edits to settle: {}",
          error.error
        ))
      })
      .ok();
  }

  /// Builds the edited config. A failed build leaves the shell running and
  /// says why in a notification; a new program replaces the shell.
  fn rebuild(&mut self) {
    if self.stopping {
      return;
    }
    log("the config changed; building it");
    match timed_build(&self.config, &self.target) {
      Err(problem) => {
        log(&format!(
          "the config didn't build: {problem}; the running shell stays"
        ));
        notify::send(&self.handle, &self.scheduler, DIDNT_BUILD, &problem);
      }
      Ok(built) if built.fresh && self.build.as_ref() == Some(&built.program) => {
        log("nothing in the build changed; the shell keeps running");
      }
      Ok(built) => {
        self.build = Some(built.program);
        self.policy.new_build();
        self.reload();
      }
    }
  }

  /// Swaps the running shell for the new build: asks it to save and exit,
  /// and starts the build when it has. One waiting to restart is
  /// replaced at once.
  fn reload(&mut self) {
    // Already asked: the newest build starts when the old shell ends, and
    // the kill stays due when it was.
    if self.reloading.is_some() {
      return;
    }
    let Some((child, _)) = &self.shell else {
      if let Some(timer) = self.restart_timer.take() {
        self.handle.remove(timer);
      }
      return self.start();
    };
    let pid = Pid::from_child(child);
    let asked = self
      .control
      .as_mut()
      .is_some_and(|stream| stream.write_all(b"save\n").is_ok());
    if asked {
      log("asking the shell to save and exit");
    } else {
      log("the shell isn't connected, so it ends without saving");
      let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let timer =
      self
        .handle
        .insert_source(Timer::from_duration(SAVE_LIMIT), move |_, _, supervisor| {
          log(&format!(
            "the old shell didn't end within {} s; killing it",
            SAVE_LIMIT.as_secs()
          ));
          if let Some((child, _)) = supervisor.shell.as_mut() {
            let _ = child.kill();
          }
          TimeoutAction::Drop
        });
    match timer {
      Ok(timer) => self.reloading = Some(timer),
      Err(error) => {
        log(&format!("can't time the reload: {}", error.error));
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
      }
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

/// Reads the shell's lines from `stream` on the loop, and keeps a copy to
/// write to it.
fn listen(
  handle: &LoopHandle<'static, Supervisor>,
  supervisor: &mut Supervisor,
  stream: UnixStream,
) {
  if let Err(error) = stream.set_nonblocking(true) {
    return log(&format!("can't read from the shell: {error}"));
  }
  match stream.try_clone() {
    Ok(copy) => supervisor.control = Some(copy),
    Err(error) => log(&format!("can't write to the shell: {error}")),
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

/// Copies `program` to `last_good` through a file beside it, so a crash
/// mid-copy never leaves half a program there.
fn keep(program: &Path, last_good: &Path) -> std::io::Result<()> {
  let partial = last_good.with_file_name("last-good.partial");
  std::fs::copy(program, &partial)?;
  std::fs::rename(&partial, last_good)
}

/// Builds the config, and logs how long it took.
fn timed_build(config: &Path, target: &Path) -> Result<Built, String> {
  let started = Instant::now();
  let built = build::build(config, target);
  log(&format!(
    "the build took {:.1} s",
    started.elapsed().as_secs_f64()
  ));
  built
}

/// glibc's tunable that keeps its threshold for using `mmap` where it is.
/// By default the threshold rises to the size of each large block freed,
/// so after a wallpaper is decoded the next one's buffers come from the
/// heap and stay there: tens of MiB for a 4K image. The shell's own
/// `spawn` takes it out of what it starts. Kept in step with
/// crabshell-services' `spawn::TUNABLE`.
pub(crate) const TUNABLE: &str = "glibc.malloc.mmap_threshold=131072";

/// `GLIBC_TUNABLES` for the shell: `old` with [`TUNABLE`] added, unless it
/// sets that threshold already.
pub(crate) fn tunables(old: Option<std::ffi::OsString>) -> std::ffi::OsString {
  match old {
    Some(old)
      if old
        .to_string_lossy()
        .contains("glibc.malloc.mmap_threshold") =>
    {
      old
    }
    Some(mut old) if !old.is_empty() => {
      old.push(":");
      old.push(TUNABLE);
      old
    }
    _ => TUNABLE.into(),
  }
}
