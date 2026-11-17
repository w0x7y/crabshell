//! Programs a config runs, as Quickshell's `Process`: what they print,
//! whole or line by line, and how they ended.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io::{ErrorKind, PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe, Location};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::Instant;

use calloop::timer::TimeoutAction;
use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use rustix::process::{Pid, PidfdFlags, Signal as Kill, kill_process_group, pidfd_open};

use crate::lines::{Lines, decode, panic_message};
use crate::{Shared, activation, spawn};

/// The most of each output kept in [`Process::stdout`] and
/// [`Process::stderr`]. Lines still reach `on_line` past it.
pub(crate) const MAX_OUTPUT: usize = 1024 * 1024;

/// The most bytes left to write to a program's input before it is closed,
/// so a program that reads nothing can't grow the shell without end.
pub(crate) const MAX_INPUT: usize = 4 * 1024 * 1024;

/// What `cx.process` and `cx.exec_detached` need from the shell: the loop.
pub(crate) struct Processes {
  pub(crate) shared: Rc<Shared>,
  /// Every `cx.process`, so the shell can stop them all before it exits.
  live: RefCell<Vec<Weak<Inner>>>,
}

impl Processes {
  pub(crate) fn new(shared: Rc<Shared>) -> Processes {
    Processes {
      shared,
      live: RefCell::default(),
    }
  }
}

/// Stops every program `cx.process` started, as the shell does before it
/// exits: `exit` skips the drops that would, and each program has a
/// process group of its own, which nothing else signals.
pub(crate) fn stop_all(cx: &Cx) {
  let Some(processes) = cx.service::<Processes>() else {
    return;
  };
  let live: Vec<Rc<Inner>> = processes
    .live
    .borrow()
    .iter()
    .filter_map(Weak::upgrade)
    .collect();
  for inner in live {
    inner.stop();
  }
}

/// What `cx.process` and `cx.exec_detached` run: a [`Command`], or the
/// program and its arguments as a list, such as `["sleep", "30"]`, as
/// Quickshell's `command` is.
pub trait IntoCommand {
  /// The command, or `None` for an empty list.
  fn into_command(self) -> Option<Command>;
}

impl IntoCommand for Command {
  fn into_command(self) -> Option<Command> {
    Some(self)
  }
}

impl<S: AsRef<OsStr>> IntoCommand for Vec<S> {
  fn into_command(self) -> Option<Command> {
    let mut words = self.into_iter();
    let mut command = Command::new(words.next()?);
    command.args(words);
    Some(command)
  }
}

impl<S: AsRef<OsStr>, const N: usize> IntoCommand for [S; N] {
  fn into_command(self) -> Option<Command> {
    Vec::from(self).into_command()
  }
}

/// A program the shell runs, from `cx.process(command)`. A clone is cheap
/// and shares it.
///
/// It starts on the loop's next turn, so `on_line` and `with_stdin` set
/// right after `cx.process` apply to it. It runs in a process group of its
/// own, and that group gets `SIGTERM` when the context that started it
/// goes or the command starts again. Nothing polls: the shell wakes when
/// the program prints, takes input or ends.
#[derive(Clone)]
pub struct Process {
  inner: Rc<Inner>,
}

struct Inner {
  /// `None` when the shell provided no services.
  processes: Option<Rc<Shared>>,
  /// These live under the starting context's owner.
  running: Signal<bool>,
  status: Signal<Option<ExitStatus>>,
  stdout: Signal<String>,
  stderr: Signal<String>,
  /// Read by the effect that builds the command, so bumping it starts
  /// the command again.
  restarts: Signal<u64>,
  state: RefCell<State>,
}

#[derive(Default)]
struct State {
  /// The command to start at the loop's next turn, and the timer that
  /// will.
  pending: Option<Command>,
  starter: Option<RegistrationToken>,
  run: Option<Run>,
  keep_stdin: bool,
  /// Written before the program started, for its input.
  input: Vec<u8>,
  /// The config closed the input: it closes once what was written is
  /// sent.
  closing: bool,
  /// A run ended, failed to start or was killed before it started, and
  /// nothing is to start: writes and closes have no run to wait for.
  ran: bool,
  on_line: Option<OnLine>,
  /// Which method set `on_line` and where, for the log if it panics.
  at: Option<(&'static str, &'static Location<'static>)>,
  /// What the run printed on its standard output and wasn't passed on
  /// yet. Split at a newline until a handler says otherwise, so one set
  /// late still gets the start of its first line.
  lines: Lines,
  /// A next-turn delivery is on its way, and passes on all that waits.
  later: bool,
  /// From [`Process::stopped`]: nothing starts until a restart.
  stopped: bool,
}

/// What a config does with each line.
type OnLine = Rc<dyn Fn(&str)>;

/// The program running now.
struct Run {
  child: Child,
  program: String,
  exit: Option<RegistrationToken>,
  out: Output,
  err: Output,
  stdin: Option<PipeWriter>,
  outbox: Vec<u8>,
  writer: Option<RegistrationToken>,
}

/// One of a program's outputs, read as it comes.
#[derive(Default)]
struct Output {
  stream: Option<(PipeReader, RegistrationToken)>,
  /// The start of a character a read cut off.
  partial: Vec<u8>,
  /// How much is in the signal, and whether the cap was logged.
  kept: usize,
  capped: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Which {
  Out,
  Err,
}

/// `cx.process(command)`.
#[track_caller]
pub(crate) fn process<C: IntoCommand>(cx: &Cx, command: impl Fn() -> C + 'static) -> Process {
  let at = Location::caller();
  let service = cx.service::<Processes>();
  let processes = service
    .as_ref()
    .map(|processes| Rc::clone(&processes.shared));
  if processes.is_none() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.process() needs the services the shell provides; nothing runs");
    });
  }
  let inner = Rc::new(Inner {
    processes,
    // Running from the start: the effect below starts it at the next
    // flush.
    running: cx.signal(true),
    status: cx.signal(None),
    stdout: cx.signal(String::new()),
    stderr: cx.signal(String::new()),
    restarts: cx.signal(0),
    state: RefCell::default(),
  });
  // Dropped with the starting context's owner, which stops the program.
  cx.owner().signal(StopOnDrop(Rc::clone(&inner)));
  if let Some(service) = service {
    let mut live = service.live.borrow_mut();
    live.retain(|inner| inner.strong_count() > 0);
    live.push(Rc::downgrade(&inner));
  }
  let weak = Rc::downgrade(&inner);
  let restarts = inner.restarts;
  // The command is the config's: built in an effect, so it is built again
  // when a signal it read changes. A panic stops the run before and is
  // logged; the next change tries again.
  cx.effect(move || {
    restarts.get();
    let Some(inner) = weak.upgrade().filter(|inner| !inner.state.borrow().stopped) else {
      return;
    };
    let built = panic::catch_unwind(AssertUnwindSafe(|| command().into_command()));
    match built {
      Ok(next) => untrack(|| inner.start(next)),
      Err(payload) => {
        let message = panic_message(payload.as_ref());
        tracing::error!("cx.process (called at {at}) panicked building its command: {message}");
        untrack(|| inner.cancel());
      }
    }
  });
  Process { inner }
}

/// `cx.exec_detached(command)`.
pub(crate) fn exec_detached(cx: &Cx, command: impl IntoCommand) {
  let Some(processes) = cx.service::<Processes>() else {
    tracing::warn!("cx.exec_detached() needs the services the shell provides; nothing runs");
    return;
  };
  let Some(mut command) = command.into_command() else {
    return tracing::warn!("cx.exec_detached() got an empty command; nothing runs");
  };
  let shared = Rc::clone(&processes.shared);
  activation::with_token(&processes.shared, None, move |token| {
    if let Some(token) = &token {
      activation::set_env(&mut command, token);
    }
    // Logged there.
    let _ = spawn::spawn(&shared, &mut command);
  });
}

/// Stops the program when the starting context's owner drops it.
struct StopOnDrop(Rc<Inner>);

impl Drop for StopOnDrop {
  fn drop(&mut self) {
    self.0.stop();
  }
}

impl Process {
  /// `true` from `cx.process` or a restart until the program ends or
  /// can't start.
  pub fn running(&self) -> ReadSignal<bool> {
    self.inner.running.read_only()
  }

  /// How the last run ended: `None` while it runs, or if it couldn't
  /// start.
  pub fn exit_status(&self) -> ReadSignal<Option<ExitStatus>> {
    self.inner.status.read_only()
  }

  /// Everything it printed on its standard output this run, up to 1 MiB,
  /// decoded as UTF-8 with bad bytes replaced. Complete once
  /// [`exit_status`](Self::exit_status) is set.
  pub fn stdout(&self) -> ReadSignal<String> {
    self.inner.stdout.read_only()
  }

  /// Like [`stdout`](Self::stdout), for its standard error.
  pub fn stderr(&self) -> ReadSignal<String> {
    self.inner.stderr.read_only()
  }

  /// Calls `on_line` with each line of its standard output, without its
  /// `\n`, decoded as UTF-8 with bad bytes replaced, as it comes. What is
  /// left when it ends counts as a last line. A line over 1 MiB is
  /// dropped and logged.
  ///
  /// It runs on the main thread. If it panics, that is logged with the
  /// program and where `on_line` was called, and it isn't called again.
  #[track_caller]
  pub fn on_line(self, on_line: impl Fn(&str) + 'static) -> Process {
    self.handle("on_line", "\n", Rc::new(on_line))
  }

  /// Like [`on_line`](Self::on_line), with lines that end at `marker`
  /// instead of `\n`, such as `"\0"` for `find -print0`, as Quickshell's
  /// `SplitParser`. An empty `marker` passes on each read as it comes, up
  /// to 64 KiB, keeping a character the read cut off for the next.
  ///
  /// It replaces `on_line`, and everything not passed on yet is split at
  /// the new marker: set from a handler, from the line after the one it
  /// was given; set elsewhere, on the loop's next turn.
  #[track_caller]
  pub fn on_split(self, marker: &str, on_split: impl Fn(&str) + 'static) -> Process {
    self.handle("on_split", marker, Rc::new(on_split))
  }

  #[track_caller]
  fn handle(self, name: &'static str, marker: &str, handler: OnLine) -> Process {
    let mut state = self.inner.state.borrow_mut();
    state.on_line = Some(handler);
    state.at = Some((name, Location::caller()));
    state.lines.set_marker(marker);
    let waiting = state.lines.waiting();
    drop(state);
    if waiting {
      self.inner.deliver_later();
    }
    self
  }

  /// Gives the program an input that [`write`](Self::write) feeds, open
  /// until [`close_stdin`](Self::close_stdin). Without it the program's
  /// input is empty. It applies from the next start, so call it right
  /// after `cx.process`.
  pub fn with_stdin(self) -> Process {
    self.inner.state.borrow_mut().keep_stdin = true;
    self
  }

  /// Sends `data` to the program's input, as it takes it; written before
  /// the program started, it waits for the start. Without
  /// [`with_stdin`](Self::with_stdin), or once the input is closed, `data`
  /// is dropped and that is logged. If more than 4 MiB wait because the
  /// program reads nothing, its input is closed.
  pub fn write(&self, data: impl AsRef<[u8]>) {
    self.inner.write(data.as_ref());
  }

  /// Closes the program's input once what was written is sent, so it
  /// reads the end of it.
  pub fn close_stdin(&self) {
    {
      let mut state = self.inner.state.borrow_mut();
      // Nothing runs or is to start: the next run's input starts open.
      if state.run.is_none() && state.ran {
        return;
      }
      state.closing = true;
    }
    self.inner.flush();
  }

  /// Sends `SIGTERM` to the program's process group. Its exit status
  /// comes when it ends.
  /// Before it started, it doesn't start.
  pub fn kill(&self) {
    if let Some(run) = &self.inner.state.borrow().run {
      return terminate(&run.child);
    }
    self.inner.cancel();
  }

  /// Stops the program if it runs and starts the command again, as when
  /// a signal the command read changed. A [`stopped`](Self::stopped) one
  /// starts.
  pub fn restart(&self) {
    self.inner.state.borrow_mut().stopped = false;
    self.inner.restarts.try_update(|count| *count += 1);
  }

  /// Doesn't start the program until [`restart`](Self::restart), as
  /// Quickshell's `Process` with `running: false`, so a program run on
  /// demand, such as a button's, is one process restarted per click.
  /// Until then the command isn't built, a signal it reads starts
  /// nothing, [`running`](Self::running) is `false`, and writes are
  /// dropped. Call it right after `cx.process`; later, it stops a run
  /// about to start, but not one already running.
  pub fn stopped(self) -> Process {
    self.inner.state.borrow_mut().stopped = true;
    if self.inner.state.borrow().run.is_none() {
      self.inner.cancel();
    }
    self
  }
}

/// Sends `SIGTERM` to `child`'s process group.
fn terminate(child: &Child) {
  // The group is the child's own, from `process_group(0)`.
  if let Err(error) = kill_process_group(Pid::from_child(child), Kill::TERM) {
    tracing::debug!(%error, "couldn't stop a program; it may have ended");
  }
}

impl Inner {
  fn shared(&self) -> Option<&Rc<Shared>> {
    self.processes.as_ref()
  }

  fn event_loop(&self) -> Option<Box<dyn crate::EventLoop>> {
    self.shared()?.event_loop()
  }

  /// Stops the program running now, if one is, and starts `command` at
  /// the loop's next turn. An empty command is logged and runs nothing.
  fn start(self: &Rc<Self>, command: Option<Command>) {
    self.stop_run();
    let Some(command) = command else {
      tracing::warn!("cx.process() got an empty command; nothing runs");
      set(self.running, false);
      return;
    };
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    let mut state = self.state.borrow_mut();
    state.pending = Some(command);
    state.ran = false;
    if state.starter.is_none() {
      let inner = Rc::clone(self);
      state.starter = event_loop.insert_timer(
        Instant::now(),
        Box::new(move || {
          inner.spawn_pending();
          TimeoutAction::Drop
        }),
      );
    }
    drop(state);
    set(self.running, true);
    set(self.status, None);
    set(self.stdout, String::new());
    set(self.stderr, String::new());
  }

  fn spawn_pending(self: &Rc<Self>) {
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    let (command, keep_stdin, input) = {
      let mut state = self.state.borrow_mut();
      state.starter = None;
      let input = std::mem::take(&mut state.input);
      (state.pending.take(), state.keep_stdin, input)
    };
    let Some(mut command) = command else {
      return;
    };
    let program = command.get_program().to_string_lossy().into_owned();
    let spawned = spawn_piped(&mut command, keep_stdin);
    // The command holds the program's ends of the pipes. They must close
    // here, or its outputs would never end.
    drop(command);
    let (child, out, err, stdin) = match spawned {
      Ok(spawned) => spawned,
      Err(error) => {
        tracing::warn!(program, %error, "couldn't start the program");
        let mut state = self.state.borrow_mut();
        state.ran = true;
        state.closing = false;
        drop(state);
        set(self.running, false);
        return;
      }
    };
    let mut run = Run {
      child,
      program,
      exit: None,
      out: Output::default(),
      err: Output::default(),
      stdin,
      outbox: input,
      writer: None,
    };
    self.state.borrow_mut().lines.reset();
    for (which, stream) in [(Which::Out, out), (Which::Err, err)] {
      let Ok(fd) = stream.as_fd().try_clone_to_owned() else {
        continue;
      };
      let inner = Rc::clone(self);
      let read = Box::new(move || inner.on_output(which));
      if let Some(token) = event_loop.insert_fd(fd, Interest::READ, read) {
        run.output(which).stream = Some((stream, token));
      }
    }
    match pidfd_open(Pid::from_child(&run.child), PidfdFlags::NONBLOCK) {
      Ok(fd) => {
        let inner = Rc::clone(self);
        let ended = Box::new(move || inner.on_exit());
        run.exit = event_loop.insert_fd(fd, Interest::READ, ended);
      }
      Err(error) => tracing::warn!(%error, "couldn't follow a program to its end"),
    }
    self.state.borrow_mut().run = Some(run);
    // Whatever was written before it started.
    self.flush();
  }

  /// Reads what the program printed on one output.
  fn on_output(&self, which: Which) {
    let mut chunk = [0; 64 * 1024];
    let mut state = self.state.borrow_mut();
    let State { run, lines, .. } = &mut *state;
    let Some(run) = run.as_mut() else {
      return;
    };
    let Some((stream, _)) = run.output(which).stream.as_mut() else {
      return;
    };
    let read = match stream.read(&mut chunk) {
      Ok(read) => read,
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(_) => 0,
    };
    let text = run.take(which, &chunk[..read]);
    if which == Which::Out {
      lines.append(&chunk[..read]);
    }
    let program = run.program.clone();
    if read == 0 {
      // It closed this output; its exit is still to come.
      let token = run.output(which).stream.take().map(|(_, token)| token);
      if let (Some(token), Some(event_loop)) = (token, self.event_loop()) {
        event_loop.remove(token);
      }
    }
    drop(state);
    self.show(which, text);
    self.deliver(false, &program);
  }

  /// The program ended: reads what is left of its outputs, then shows how
  /// it ended.
  fn on_exit(&self) {
    let mut state = self.state.borrow_mut();
    let Some(run) = state.run.as_mut() else {
      return;
    };
    let status = match run.child.try_wait() {
      Ok(Some(status)) => Some(status),
      Ok(None) => return,
      // Reaped behind the shell's back: its end is all there is to know.
      Err(error) => {
        tracing::warn!(%error, program = run.program, "lost track of the program");
        None
      }
    };
    let mut run = state.run.take().expect("invariant: matched just above");
    // The next run's input starts open.
    state.closing = false;
    state.ran = true;
    drop(state);
    // What it printed before it ended is in the sockets now. A program it
    // started may hold them open, so read only what is there.
    // A program it started that keeps printing would keep this going, so
    // it stops after what the signal keeps.
    let mut chunk = [0; 64 * 1024];
    for which in [Which::Out, Which::Err] {
      let mut budget = MAX_OUTPUT;
      while let Some((stream, _)) = run.output(which).stream.as_mut()
        && budget > 0
      {
        let Ok(read @ 1..) = stream.read(&mut chunk) else {
          break;
        };
        budget = budget.saturating_sub(read);
        let text = run.take(which, &chunk[..read]);
        if which == Which::Out {
          self.state.borrow_mut().lines.append(&chunk[..read]);
        }
        self.show(which, text);
        self.deliver(false, &run.program);
      }
      let text = run.take(which, &[]);
      self.show(which, text);
    }
    self.deliver(true, &run.program);
    self.remove_sources(&mut run);
    set(self.status, status);
    set(self.running, false);
  }

  /// Adds `text` to the output's signal.
  fn show(&self, which: Which, text: String) {
    if text.is_empty() {
      return;
    }
    let signal = match which {
      Which::Out => self.stdout,
      Which::Err => self.stderr,
    };
    signal.try_update(|all| all.push_str(&text));
  }

  /// Passes on what waits at the loop's next turn, as a marker set outside
  /// a handler asks.
  fn deliver_later(self: &Rc<Self>) {
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    if std::mem::replace(&mut self.state.borrow_mut().later, true) {
      return;
    }
    let inner = Rc::clone(self);
    let later = move || {
      inner.state.borrow_mut().later = false;
      let program = inner
        .state
        .borrow()
        .run
        .as_ref()
        .map(|run| run.program.clone());
      if let Some(program) = program {
        inner.deliver(false, &program);
      }
      TimeoutAction::Drop
    };
    event_loop.insert_timer(Instant::now(), Box::new(later));
  }

  /// Passes each whole line waiting to the handler set at the time, one at
  /// a time with nothing borrowed, so one that sets another marker has the
  /// rest split at it. Once the program is `done`, what is left goes last.
  fn deliver(&self, done: bool, program: &str) {
    loop {
      let (line, on_line, at) = {
        let mut state = self.state.borrow_mut();
        let line = match state.lines.next(&program) {
          Some(line) => line,
          None if done => match state.lines.rest(&program) {
            Some(line) => line,
            None => return,
          },
          None => return,
        };
        (line, state.on_line.clone(), state.at)
      };
      let Some(on_line) = on_line else {
        continue;
      };
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_line(&line))) {
        let (name, at) = at.map_or(("on_line", String::new()), |(name, at)| {
          (name, format!(" (called at {at})"))
        });
        let message = panic_message(payload.as_ref());
        tracing::error!("process {program} {name}{at} panicked: {message}");
        let mut state = self.state.borrow_mut();
        // Unless it set a new one meanwhile.
        if state
          .on_line
          .as_ref()
          .is_some_and(|now| Rc::ptr_eq(now, &on_line))
        {
          state.on_line = None;
        }
      }
    }
  }

  fn write(self: &Rc<Self>, data: &[u8]) {
    let mut state = self.state.borrow_mut();
    if state.closing {
      return tracing::warn!("the program's input is closed, so a write was dropped");
    }
    // Not started yet, so kept for the start.
    let waiting = state.keep_stdin && state.run.is_none() && !state.ran;
    match state.run.as_mut() {
      Some(run) if run.stdin.is_some() => run.outbox.extend_from_slice(data),
      _ if waiting => {
        if state.input.len() + data.len() > MAX_INPUT {
          return tracing::warn!("over 4 MiB written before the program started; dropped");
        }
        return state.input.extend_from_slice(data);
      }
      _ => return tracing::warn!("the program takes no input now, so a write was dropped"),
    }
    drop(state);
    self.flush();
  }

  /// Writes what the program's input takes, and waits for room for the
  /// rest.
  fn flush(self: &Rc<Self>) {
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    let mut state = self.state.borrow_mut();
    let closing = state.closing;
    let Some(run) = state.run.as_mut() else {
      return;
    };
    let Some(stdin) = run.stdin.as_mut() else {
      return;
    };
    let mut broken = false;
    while !run.outbox.is_empty() {
      match stdin.write(&run.outbox) {
        Ok(written) => {
          run.outbox.drain(..written);
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(_) => {
          broken = true;
          break;
        }
      }
    }
    if run.outbox.len() > MAX_INPUT {
      let program = &run.program;
      tracing::warn!(
        program,
        "the program reads nothing it is sent; closing its input"
      );
      broken = true;
    }
    if broken {
      drop(state);
      return self.drop_stdin();
    }
    if run.outbox.is_empty() {
      if let Some(writer) = run.writer.take() {
        event_loop.remove(writer);
      }
      if closing {
        // All sent: the program reads the end.
        run.stdin = None;
      }
    } else if run.writer.is_none()
      && let Ok(fd) = stdin.as_fd().try_clone_to_owned()
    {
      let inner = Rc::clone(self);
      run.writer = event_loop.insert_fd(fd, Interest::WRITE, Box::new(move || inner.flush()));
    }
  }

  /// Closes the program's input now, dropping what wasn't sent.
  fn drop_stdin(&self) {
    let writer = {
      let mut state = self.state.borrow_mut();
      state.input.clear();
      let Some(run) = state.run.as_mut() else {
        return;
      };
      run.stdin = None;
      run.outbox.clear();
      run.writer.take()
    };
    if let (Some(writer), Some(event_loop)) = (writer, self.event_loop()) {
      event_loop.remove(writer);
    }
  }

  /// Stops the program running now, if one is: `SIGTERM` to its group,
  /// its outputs closed, and it is left to be reaped.
  fn stop_run(&self) {
    let run = {
      let mut state = self.state.borrow_mut();
      let run = state.run.take();
      // The next run's input starts open.
      if run.is_some() {
        state.closing = false;
      }
      run
    };
    let Some(mut run) = run else {
      return;
    };
    // Its lines aren't passed on after it.
    self.state.borrow_mut().lines.reset();
    terminate(&run.child);
    self.remove_sources(&mut run);
    if let Some(shared) = &self.processes {
      spawn::reap(shared, run.child, run.program);
    }
  }

  /// Stops everything, for good, as the starting context went.
  fn stop(&self) {
    let (starter, on_line) = {
      let mut state = self.state.borrow_mut();
      state.pending = None;
      (state.starter.take(), state.on_line.take())
    };
    // Dropped outside the borrow: it may hold this process, and its drop
    // may use it.
    drop(on_line);
    if let (Some(starter), Some(event_loop)) = (starter, self.event_loop()) {
      event_loop.remove(starter);
    }
    self.stop_run();
  }

  /// Stops the run and drops a start still to come, as when the command
  /// panicked or the config killed it before it started.
  fn cancel(&self) {
    let starter = {
      let mut state = self.state.borrow_mut();
      state.pending = None;
      state.ran = true;
      state.closing = false;
      state.starter.take()
    };
    if let (Some(starter), Some(event_loop)) = (starter, self.event_loop()) {
      event_loop.remove(starter);
    }
    self.stop_run();
    let ran = self.state.borrow().run.is_none();
    if ran {
      set(self.running, false);
    }
  }

  fn remove_sources(&self, run: &mut Run) {
    let tokens = [
      run.exit.take(),
      run.writer.take(),
      run.out.stream.take().map(|(_, token)| token),
      run.err.stream.take().map(|(_, token)| token),
    ];
    run.stdin = None;
    if let Some(event_loop) = self.event_loop() {
      for token in tokens.into_iter().flatten() {
        event_loop.remove(token);
      }
    }
  }
}

/// The program's outputs, and its input if wanted, through pipes, whose
/// ends here are non-blocking. Pipes, not socket pairs: a program can
/// open `/dev/stdout` on a pipe.
#[allow(
  clippy::type_complexity,
  reason = "the four parts of one spawn, used once"
)]
fn spawn_piped(
  command: &mut Command,
  keep_stdin: bool,
) -> std::io::Result<(Child, PipeReader, PipeReader, Option<PipeWriter>)> {
  let (out, child_out) = std::io::pipe()?;
  let (err, child_err) = std::io::pipe()?;
  rustix::io::ioctl_fionbio(&out, true)?;
  rustix::io::ioctl_fionbio(&err, true)?;
  let (stdin, child_in) = if keep_stdin {
    let (theirs, ours) = std::io::pipe()?;
    rustix::io::ioctl_fionbio(&ours, true)?;
    (Some(ours), Stdio::from(theirs))
  } else {
    (None, Stdio::null())
  };
  spawn::untune(command);
  let child = command
    .stdout(child_out)
    .stderr(child_err)
    .stdin(child_in)
    .process_group(0)
    .spawn()?;
  Ok((child, out, err, stdin))
}

impl Run {
  fn output(&mut self, which: Which) -> &mut Output {
    match which {
      Which::Out => &mut self.out,
      Which::Err => &mut self.err,
    }
  }

  /// `bytes` read from one output: the text to add to its signal. Empty
  /// `bytes` means the output ended, which lets a cut-off character go.
  fn take(&mut self, which: Which, bytes: &[u8]) -> String {
    let program = self.program.clone();
    let output = self.output(which);
    output.partial.extend_from_slice(bytes);
    let mut text = if bytes.is_empty() {
      String::from_utf8_lossy(&std::mem::take(&mut output.partial)).into_owned()
    } else {
      decode(&mut output.partial)
    };
    // Once a character didn't fit, nothing after it is kept either.
    if output.capped {
      text.clear();
    }
    let room = MAX_OUTPUT.saturating_sub(output.kept);
    if text.len() > room {
      if !std::mem::replace(&mut output.capped, true) {
        tracing::warn!(program, "a program printed over 1 MiB; the rest isn't kept");
      }
      let mut end = room;
      while !text.is_char_boundary(end) {
        end -= 1;
      }
      text.truncate(end);
    }
    output.kept += text.len();
    text
  }
}

/// Sets `signal` unless it holds `value` or is gone. The read is
/// untracked, so an effect that starts a process doesn't run again for it.
fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
