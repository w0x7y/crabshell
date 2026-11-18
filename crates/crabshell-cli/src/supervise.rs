//! `crabshell run`: builds the config, starts the shell and starts it again
//! when it crashes, falling back to `last-good` when a new build keeps
//! crashing. An edit to the config, or `cx.reload()`, rebuilds it and
//! swaps the shell: the old one saves and runs on beside the new one
//! until that one is ready, so no frame goes without a bar. One thread,
//! on a calloop loop that sleeps until a shell exits or talks, a file
//! changes or a timer is due.

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

/// The variable set to `1` on a shell started beside the old one of a
/// reload, which waits for `alone` before it takes what only one process
/// can hold.
const BESIDE: &str = "CRABSHELL_BESIDE";

/// The signals that stop `crabshell run`, and the shell with it.
const STOP_SIGNALS: [Signal; 2] = [Signal::SIGINT, Signal::SIGTERM];

/// How long the files must be quiet after an edit before a rebuild, so a
/// save that writes several files builds once.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// How long the old shell gets to save, and to exit once told, before
/// it is killed.
const SAVE_LIMIT: Duration = Duration::from_secs(5);

/// The notification's title when the config doesn't build.
const DIDNT_BUILD: &str = "Your Crabshell config didn't build";

/// How long a new shell started beside the old one gets to say `ready`
/// before it is killed and the old one stays.
const READY_LIMIT: Duration = Duration::from_secs(10);

/// The notification's title when a new shell ends before it is ready.
const DIDNT_START: &str = "Your Crabshell config didn't start";

/// Why the supervisor ends a new shell that runs beside the old one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ending {
  /// It wasn't ready within [`READY_LIMIT`].
  Late,
  /// A newer build starts in its place.
  Replaced,
}

/// The shell a reload replaces, running beside the new one until that
/// one is ready.
struct Old {
  child: Child,
  started: Instant,
  control: Option<UnixStream>,
  /// When it said `saved`.
  saved: Instant,
  /// When it would have counted as healthy, had no reload come.
  healthy_due: Option<Instant>,
  /// It was told to exit, since the new shell is ready; the timer kills
  /// it if it takes too long.
  exiting: Option<Option<RegistrationToken>>,
  /// Its control connection closed after it was told to exit: its
  /// descriptors are closed, though the kernel may take tens of ms more
  /// to end the process.
  hung_up: bool,
}

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
  /// The running shell said `can-hand-off`: a reload asks it to save and
  /// run on, rather than to save and exit.
  can_hand_off: bool,
  /// The running shell was told `alone`.
  told_alone: bool,
  /// The shell being replaced, while the new one starts.
  old: Option<Old>,
  /// An edit or `cx.reload()` came while a new shell started beside the
  /// old one: another reload follows.
  reload_again: bool,
  /// The timer that marks the running shell healthy, and when.
  healthy_timer: Option<RegistrationToken>,
  healthy_due: Option<Instant>,
  /// The timer that ends a new shell beside the old one that isn't ready
  /// within [`READY_LIMIT`].
  ready_deadline: Option<RegistrationToken>,
  /// Why the new shell beside the old one is being ended: it was late,
  /// or a newer build replaces it.
  ending: Option<Ending>,
  /// The timer that starts the shell again after a crash.
  restart_timer: Option<RegistrationToken>,
  /// The timer that rebuilds once the files are quiet.
  debounce_timer: Option<RegistrationToken>,
  /// A new build waits for the old shell to save, or with `save`, to
  /// end; the timer kills it if it takes too long.
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
    let old = self.old.take().map(|old| old.child);
    for mut child in self
      .shell
      .take()
      .map(|(child, _)| child)
      .into_iter()
      .chain(old)
    {
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
    can_hand_off: false,
    told_alone: false,
    old: None,
    reload_again: false,
    healthy_timer: None,
    healthy_due: None,
    ready_deadline: None,
    ending: None,
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
    self.told_alone = false;
    // A child inherits the blocked signals signalfd needs, so they are
    // unblocked while it starts; one arriving just then ends the
    // supervisor as if it had no handler.
    if let Err(error) = self.signals.as_source_mut().remove_signals(&STOP_SIGNALS) {
      log(&format!("can't unblock signals for the shell: {error}"));
    }
    self.ending = None;
    let mut command = Command::new(&program);
    if self.old.is_some() {
      command.env(BESIDE, "1");
    }
    let spawned = command
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
        let how = format!("{} didn't start: {error}", program.display());
        // An old shell told to exit can't take over again.
        if self.old.as_ref().is_some_and(|old| old.exiting.is_none()) {
          return self.failed_reload(&how);
        }
        return self.ended(Exit::Crashed, &how);
      }
    };
    let pid = child.id();
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
            move |_, _, supervisor| {
              supervisor.exited(pid);
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
    self.healthy_due = None;
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

  /// Whether `pid`, a connection's shell, is the running shell.
  fn is_current(&self, pid: Option<u32>) -> bool {
    let running = self.shell.as_ref().map(|(child, _)| child.id());
    pid.is_some() && running == pid
  }

  /// Shell `pid`'s pidfd turned readable: it exited.
  fn exited(&mut self, pid: u32) {
    if self.old.as_ref().is_some_and(|old| old.child.id() == pid) {
      return self.old_exited();
    }
    let Some((mut child, _)) = self.shell.take_if(|(child, _)| child.id() == pid) else {
      return;
    };
    // Meant for this shell, the new one beside the old.
    let ending = self.ending.take();
    if let Some(timer) = self.healthy_timer.take() {
      self.handle.remove(timer);
    }
    self.healthy_due = None;
    self.control = None;
    self.can_hand_off = false;
    let waited = child.wait();
    let status = waited
      .as_ref()
      .map_or_else(|error| error.to_string(), |status| status.to_string());
    if let Some(timer) = self.reloading.take() {
      self.handle.remove(timer);
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
    if self.stopping {
      if self.old.is_none() {
        self.result = Some(Ok(()));
      }
      return;
    }
    if self.old.as_ref().is_some_and(|old| old.exiting.is_none()) {
      return match ending {
        Some(Ending::Replaced) => {
          // It read the saved state, so the old shell saves again.
          log("the new shell ended for a newer build; the old one saves again");
          self.restore_old();
          self.reload();
        }
        Some(Ending::Late) => self.failed_reload(&format!(
          "the new shell wasn't ready within {} s",
          READY_LIMIT.as_secs()
        )),
        None => self.failed_reload(&format!("the new shell ended ({status})")),
      };
    }
    if ending == Some(Ending::Replaced) {
      // The old shell went first: nothing is left to save, so the newest
      // build simply starts. Not a crash.
      if let Some(timer) = self.ready_deadline.take() {
        self.handle.remove(timer);
      }
      log("the new shell ended for a newer build, which starts now");
      return self.start();
    }
    // A new build that ended before `ready` didn't reload.
    self.reload_gap = None;
    match waited {
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

  /// The shell a reload replaced exited.
  fn old_exited(&mut self) {
    let Some(mut old) = self.old.take() else {
      return;
    };
    // The new shell is the only one now, however long it takes, unless
    // it is being replaced: then its kill stays due.
    if self.ending != Some(Ending::Replaced)
      && let Some(timer) = self.ready_deadline.take()
    {
      self.handle.remove(timer);
    }
    if let Some(Some(timer)) = old.exiting {
      self.handle.remove(timer);
    }
    let status = old
      .child
      .wait()
      .map_or_else(|error| error.to_string(), |status| status.to_string());
    log(&format!("the old shell ended ({status})"));
    self.tell_alone();
    if self.stopping {
      if self.shell.is_none() {
        self.result = Some(Ok(()));
      }
    } else if std::mem::take(&mut self.reload_again) {
      self.reload();
    }
  }

  /// The new shell ended, or didn't start, before it was ready, as `how`
  /// says: the old one runs on, as after a build that failed.
  fn failed_reload(&mut self, how: &str) {
    if !self.restore_old() {
      return;
    }
    log(&format!(
      "{how} before it was ready, so the old shell stays"
    ));
    notify::send(
      &self.handle,
      &self.scheduler,
      DIDNT_START,
      &format!("{how} before it was ready; the shell before it runs on"),
    );
    // Its next crash starts the newest build afresh, whose crashes then
    // count toward last-good.
    self.policy.new_build();
    if std::mem::take(&mut self.reload_again) {
      self.reload();
    }
  }

  /// Makes the old shell of a reload the running one again, as it was
  /// before it saved. Returns whether there was one.
  fn restore_old(&mut self) -> bool {
    let Some(old) = self.old.take() else {
      return false;
    };
    if let Some(timer) = self.ready_deadline.take() {
      self.handle.remove(timer);
    }
    self.shell = Some((old.child, old.started));
    self.control = old.control;
    self.can_hand_off = true;
    // It held everything all along.
    self.told_alone = true;
    if let Some(due) = old.healthy_due {
      self.time_healthy(due);
    }
    true
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

  /// Shell `pid` said `ready`: it counts as healthy [`HEALTHY_AFTER`]
  /// from now, and the shell it replaces is told to exit.
  fn ready(&mut self, pid: Option<u32>) {
    // One being ended doesn't replace the old shell.
    if !self.is_current(pid) || self.ending.is_some() {
      return;
    }
    if let Some(timer) = self.ready_deadline.take() {
      self.handle.remove(timer);
    }
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
    if let Some(old) = self.old.as_mut().filter(|old| old.exiting.is_none()) {
      log(&format!(
        "reloaded with no gap: the new shell was ready {} ms after the old one saved; \
         telling the old one to exit",
        old.saved.elapsed().as_millis()
      ));
      let told = old
        .control
        .as_mut()
        .is_some_and(|stream| stream.write_all(b"exit\n").is_ok());
      if !told {
        let pid = Pid::from_child(&old.child);
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
      }
      let timer = Timer::from_duration(SAVE_LIMIT);
      let timer = self.handle.insert_source(timer, |_, _, supervisor| {
        if let Some(old) = supervisor.old.as_mut() {
          log(&format!(
            "the old shell didn't exit within {} s; killing it",
            SAVE_LIMIT.as_secs()
          ));
          old.exiting = Some(None);
          let _ = old.child.kill();
        }
        TimeoutAction::Drop
      });
      old.exiting = Some(timer.ok());
    }
    let Some(due) = self.policy.ready(Instant::now()) else {
      return;
    };
    self.time_healthy(due);
  }

  /// Counts the running shell healthy at `due`.
  fn time_healthy(&mut self, due: Instant) {
    self.healthy_due = Some(due);
    self.healthy_timer = self
      .handle
      .insert_source(Timer::from_deadline(due), |_, _, supervisor| {
        supervisor.healthy();
        TimeoutAction::Drop
      })
      .map_err(|error| log(&format!("can't time the shell: {}", error.error)))
      .ok();
  }

  /// Tells the running shell it is alone, so it takes what only one
  /// process can hold: once the old shell of a reload is gone, or at once
  /// when there is none.
  fn tell_alone(&mut self) {
    let old_gone = self.old.as_ref().is_none_or(|old| old.hung_up);
    if self.told_alone || !self.can_hand_off || !old_gone {
      return;
    }
    let Some(stream) = self.control.as_mut() else {
      return;
    };
    self.told_alone = true;
    if let Err(error) = stream.write_all(b"alone\n") {
      log(&format!("can't tell the shell it runs alone: {error}"));
    }
  }

  /// Shell `pid` closed its control connection. The old shell of a
  /// reload, told to exit, has closed what it held, so the new one can
  /// take it: its bar's zone at once, without waiting for the kernel to
  /// end the process.
  fn hung_up(&mut self, pid: Option<u32>) {
    let old = self.old.as_mut().filter(|old| Some(old.child.id()) == pid);
    if let Some(old) = old.filter(|old| old.exiting.is_some()) {
      old.hung_up = true;
      self.tell_alone();
    }
  }

  /// Starts the newest build beside the old shell, and ends it if it
  /// isn't ready within [`READY_LIMIT`].
  fn start_beside(&mut self) {
    self.start();
    // A program that didn't start restored the old shell.
    if self.old.is_none() {
      return;
    }
    let timer = Timer::from_duration(READY_LIMIT);
    self.ready_deadline = self
      .handle
      .insert_source(timer, |_, _, supervisor| {
        supervisor.ready_deadline = None;
        if let Some((child, _)) = supervisor.shell.as_mut() {
          log(&format!(
            "the new shell wasn't ready within {} s; killing it",
            READY_LIMIT.as_secs()
          ));
          supervisor.ending = Some(Ending::Late);
          let _ = child.kill();
        }
        TimeoutAction::Drop
      })
      .map_err(|error| log(&format!("can't time the new shell: {}", error.error)))
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
        supervisor.rebuild(false);
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

  /// Shell `pid` asked for a reload, through `cx.reload()`.
  fn asked_reload(&mut self, pid: Option<u32>) {
    if self.is_current(pid) {
      self.rebuild(true);
    }
  }

  /// Builds the edited config, or with `asked`, the config the shell asked
  /// to reload. A failed build leaves the shell running and says why in a
  /// notification; a new program replaces the shell, and so does the
  /// same one when asked.
  fn rebuild(&mut self, asked: bool) {
    if self.stopping {
      return;
    }
    log(if asked {
      "the shell asked for a reload; building the config"
    } else {
      "the config changed; building it"
    });
    match timed_build(&self.config, &self.target) {
      Err(problem) => {
        log(&format!(
          "the config didn't build: {problem}; the running shell stays"
        ));
        notify::send(&self.handle, &self.scheduler, DIDNT_BUILD, &problem);
      }
      Ok(built) if built.fresh && self.build.as_ref() == Some(&built.program) => {
        if asked {
          log("nothing in the build changed; starting it again");
          self.reload();
        } else {
          log("nothing in the build changed; the shell keeps running");
        }
      }
      Ok(built) => {
        self.build = Some(built.program);
        self.policy.new_build();
        self.reload();
      }
    }
  }

  /// Swaps the running shell for the new build: asks it to save, starts
  /// the build beside it once it has, and tells it to exit once the new
  /// one is ready. A shell from before the handoff is asked to save and
  /// exit, and the build starts when it has. One waiting to restart is
  /// replaced at once.
  fn reload(&mut self) {
    // Already asked: the newest build starts when the old shell has saved
    // or ended, and the kill stays due when it was.
    if self.reloading.is_some() {
      return;
    }
    if let Some(old) = &self.old {
      if old.exiting.is_some() {
        self.reload_again = true;
      } else if let Some((child, _)) = &self.shell
        && self.ending.is_none()
      {
        // Not ready yet: the old shell saves again once it has gone, and
        // the newest build starts in its place.
        log("ending the new shell, which isn't ready yet, for the newer build");
        self.ending = Some(Ending::Replaced);
        let _ =
          rustix::process::kill_process(Pid::from_child(child), rustix::process::Signal::TERM);
        // In place of the ready deadline, which would call it late.
        if let Some(timer) = self.ready_deadline.take() {
          self.handle.remove(timer);
        }
        let timer = Timer::from_duration(SAVE_LIMIT);
        self.ready_deadline = self
          .handle
          .insert_source(timer, |_, _, supervisor| {
            supervisor.ready_deadline = None;
            if supervisor.ending == Some(Ending::Replaced)
              && let Some((child, _)) = supervisor.shell.as_mut()
            {
              let _ = child.kill();
            }
            TimeoutAction::Drop
          })
          .ok();
      } else if self.ending == Some(Ending::Late) {
        // Being killed for being late: the old shell stays, and then this
        // edit reloads it.
        self.reload_again = true;
      }
      return;
    }
    let Some((child, _)) = &self.shell else {
      if let Some(timer) = self.restart_timer.take() {
        self.handle.remove(timer);
      }
      return self.start();
    };
    let pid = Pid::from_child(child);
    let command: &[u8] = if self.can_hand_off {
      b"handoff\n"
    } else {
      b"save\n"
    };
    let asked = self
      .control
      .as_mut()
      .is_some_and(|stream| stream.write_all(command).is_ok());
    if asked && self.can_hand_off {
      log("asking the shell to save for a reload");
    } else if asked {
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

  /// The running shell said `saved`: the new build starts beside it.
  fn saved(&mut self, pid: Option<u32>) {
    if !self.is_current(pid) || !self.can_hand_off || self.stopping {
      return;
    }
    let Some(timer) = self.reloading.take() else {
      return;
    };
    self.handle.remove(timer);
    if let Some(timer) = self.healthy_timer.take() {
      self.handle.remove(timer);
    }
    let (child, started) = self.shell.take().expect("is_current saw the shell");
    log("the old shell saved; starting the new build beside it");
    self.old = Some(Old {
      child,
      started,
      control: self.control.take(),
      saved: Instant::now(),
      healthy_due: self.healthy_due.take(),
      exiting: None,
      hung_up: false,
    });
    self.can_hand_off = false;
    self.start_beside();
  }

  /// SIGINT or SIGTERM: ends the shells, then `run`.
  fn stop(&mut self) {
    self.stopping = true;
    if let Some(timer) = self.ready_deadline.take() {
      self.handle.remove(timer);
    }
    // A shell whose loop is stuck never acts on SIGTERM.
    let kill = Timer::from_duration(SAVE_LIMIT);
    let killing = self.handle.insert_source(kill, |_, _, supervisor| {
      log(&format!(
        "a shell didn't end within {} s of SIGTERM; killing it",
        SAVE_LIMIT.as_secs()
      ));
      let old = supervisor.old.as_mut().map(|old| &mut old.child);
      for child in supervisor
        .shell
        .as_mut()
        .map(|(child, _)| child)
        .into_iter()
        .chain(old)
      {
        let _ = child.kill();
      }
      TimeoutAction::Drop
    });
    if let Err(error) = killing {
      log(&format!("can't time the shells' end: {}", error.error));
    }
    let old = self.old.as_ref().map(|old| &old.child);
    let shells: Vec<&Child> = self
      .shell
      .iter()
      .map(|(child, _)| child)
      .chain(old)
      .collect();
    for child in &shells {
      let _ = rustix::process::kill_process(Pid::from_child(child), rustix::process::Signal::TERM);
    }
    // Waiting to restart: nothing to end.
    if shells.is_empty() {
      self.result = Some(Ok(()));
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
  // Which process connected, as the kernel saw it then, so a shell that
  // died meanwhile, or a program it started, is never taken for another.
  let pid = rustix::net::sockopt::socket_peercred(&stream)
    .ok()
    .and_then(|peer| u32::try_from(peer.pid.as_raw_nonzero().get()).ok());
  let pid = pid.filter(|&pid| supervisor.is_current(Some(pid)));
  match stream.try_clone() {
    Ok(copy) if pid.is_some() => supervisor.control = Some(copy),
    Ok(_) => {}
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
          Ok(0) => {
            supervisor.hung_up(pid);
            return Ok(PostAction::Remove);
          }
          Ok(read) => {
            for &byte in &chunk[..read] {
              if byte != b'\n' {
                if line.len() < MAX_LINE {
                  line.push(byte);
                }
                continue;
              }
              match line.as_slice() {
                b"ready" => supervisor.ready(pid),
                b"saved" => supervisor.saved(pid),
                b"reload" => supervisor.asked_reload(pid),
                b"can-hand-off" if supervisor.is_current(pid) => {
                  supervisor.can_hand_off = true;
                  supervisor.tell_alone();
                }
                b"can-hand-off" => {}
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
          Err(_) => {
            supervisor.hung_up(pid);
            return Ok(PostAction::Remove);
          }
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
/// `spawn` takes it out of what it starts. A shell started without the
/// supervisor starts itself again with crabshell-services'
/// `shell_tunables`, which a test keeps this in step with; set here, the
/// shell skips that.
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
