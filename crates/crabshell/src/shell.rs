//! The shell: the platform's event loop, the root context and its
//! services, and what `cx` can ask of the shell.

use std::cell::{Cell, RefCell};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::{Owner, ReadSignal, Runtime, Signal};
use crabshell_services::Sway;
use crabshell_ui::{Color, Cx, FloatingWindow, IntoProp, Output, Window};
use crabshell_wayland::{Event, EventLoop, LoopSignal, Platform, Toplevel, WindowId};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::level_filters::LevelFilter;

use crate::colors::{self, Handle};
use crate::control;
use crate::ipc::Ipc;
use crate::per_output::{self, Host, IntoPanels, PerOutput};
use crate::persist::{self, Persisted};
use crate::switch_log::SwitchLog;

/// The exit status when the root closure panicked: the config is broken.
pub const EXIT_ROOT_PANICKED: i32 = 1;

/// The exit status when the compositor can't be reached, or the connection
/// to it is lost, as at logout.
pub const EXIT_NO_COMPOSITOR: i32 = 2;

/// Starts the shell: connects to the compositor, calls `root` with the
/// root context, and runs the event loop. It never returns.
///
/// `root` sets the shell up, usually with [`ShellExt::per_output`]. Errors
/// are logged, and the process exits with:
///
/// - [`EXIT_ROOT_PANICKED`] (1) if `root` panics: the config is broken;
/// - [`EXIT_NO_COMPOSITOR`] (2) if there is no compositor to connect to,
///   it lacks a protocol the shell needs, or the connection is lost later,
///   as at logout.
///
/// Logs go to stderr through `tracing`, at the level `CRABSHELL_LOG` names
/// (`error`, `warn`, `info`, `debug` or `trace`; `info` if unset or empty;
/// any other value is warned about). The time from this call to the first
/// committed frame is logged at `info`, and so is the time from each of
/// Sway's workspace events to the commit that shows it.
///
/// Under the supervisor, `CRABSHELL_CONTROL` names its socket. The shell
/// says `ready` there once each window has drawn, and on `save` writes its
/// [`persist`](ShellExt::persist)ed values to
/// `$XDG_RUNTIME_DIR/crabshell/state.json` and exits with 0. It starts from
/// that file, if there is one, and removes it.
///
/// [`ipc`](ShellExt::ipc) handlers are served at
/// `$XDG_RUNTIME_DIR/crabshell/ipc-<display>.sock`, named for
/// `WAYLAND_DISPLAY`.
pub fn run(root: impl FnOnce(&Cx)) -> ! {
  let started = Instant::now();
  init_logging();
  let event_loop = match EventLoop::connect() {
    Ok(event_loop) => event_loop,
    Err(error) => {
      tracing::error!(%error, "the shell couldn't start");
      std::process::exit(EXIT_NO_COMPOSITOR);
    }
  };
  let file = persist::state_file();
  let saved = file.as_deref().map(persist::load).unwrap_or_default();
  let ipc = crate::ipc::socket_path(
    std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
    std::env::var_os("WAYLAND_DISPLAY").as_deref(),
  );
  let mut shell = match Shell::build(event_loop, saved, ipc, root) {
    Ok(shell) => shell,
    Err(error) => {
      tracing::error!(%error, "the shell couldn't start");
      std::process::exit(EXIT_ROOT_PANICKED);
    }
  };
  shell.started = started;
  if let Some(stream) = control::connect() {
    shell.follow_supervisor(stream, file);
  }
  shell.run()
}

/// Logs to stderr at the level `CRABSHELL_LOG` names. Does nothing if the
/// program set up `tracing` itself. A stderr that fails, such as a closed
/// pipe, drops log lines instead of panicking.
fn init_logging() {
  let (level, problem) = log_level(std::env::var("CRABSHELL_LOG").ok().as_deref());
  let _ = tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .with_max_level(level)
    .log_internal_errors(false)
    .try_init();
  if let Some(problem) = problem {
    tracing::warn!("{problem}");
  }
}

/// The level `CRABSHELL_LOG` names, and what is wrong with it, if anything.
/// Unset or empty is `info`.
pub(crate) fn log_level(value: Option<&str>) -> (LevelFilter, Option<String>) {
  match value.map(str::trim).filter(|value| !value.is_empty()) {
    None => (LevelFilter::INFO, None),
    Some(value) => match value.parse::<LevelFilter>() {
      Ok(level) => (level, None),
      Err(_) => (
        LevelFilter::INFO,
        Some(format!(
          "CRABSHELL_LOG={value:?} isn't a level (error, warn, info, debug, trace or off); logging at info"
        )),
      ),
    },
  }
}

/// Why the shell couldn't start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
  /// The root closure panicked; nothing would be shown.
  RootPanicked(String),
}

impl std::fmt::Display for Error {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Error::RootPanicked(message) => write!(f, "the root closure panicked: {message}"),
    }
  }
}

impl std::error::Error for Error {}

/// A shell on an event loop of its own, which the caller drives. [`run`]
/// is the usual way to start one; this is for tests and for programs that
/// run the loop themselves. Dropping it frees everything it made: every
/// reactive node under the root context, its windows and its connection.
pub struct Shell {
  event_loop: EventLoop,
  pub(crate) started: Instant,
  first_frame: bool,
  switches: SwitchLog,
  /// The supervisor's socket, until the shell has said it is ready.
  control: Option<UnixStream>,
  cx: Cx,
}

impl Shell {
  /// Sets up the shell on `event_loop` and calls `root` with the root
  /// context, which hands out the services through
  /// [`ServicesExt`](crabshell_services::ServicesExt). Nothing is shown
  /// before the first [`dispatch`](Self::dispatch).
  ///
  /// # Errors
  ///
  /// If `root` panics. The panic is logged too.
  pub fn new(event_loop: EventLoop, root: impl FnOnce(&Cx)) -> Result<Shell, Error> {
    Shell::build(event_loop, serde_json::Map::new(), None, root)
  }

  /// [`new`](Self::new), with `cx.persist` values starting from `saved`,
  /// and `cx.ipc` handlers served at `ipc`.
  pub(crate) fn build(
    mut event_loop: EventLoop,
    saved: serde_json::Map<String, serde_json::Value>,
    ipc: Option<std::path::PathBuf>,
    root: impl FnOnce(&Cx),
  ) -> Result<Shell, Error> {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner().named("root"));
    // Nothing starts here: each service connects when root first asks.
    crabshell_services::provide(&cx, &event_loop.handle());
    let state = Rc::new(RefCell::new(PerOutput::new()));
    cx.provide(Service {
      persisted: Rc::new(Persisted::new(cx.owner(), saved)),
      per_output: Rc::clone(&state),
      idle: idle_inserter(&event_loop),
      handle: {
        let weak = event_loop.handle().downgrade();
        Rc::new(move || weak.upgrade())
      },
      signal: event_loop.signal(),
      ipc: Ipc::new(&event_loop.handle(), ipc),
      root: cx.owner(),
      toplevels: Cell::new(None),
    });
    event_loop
      .platform()
      .on_event(move |platform, event| match event {
        Event::OutputAdded(output) => {
          per_output::added(&state, &mut Shown(platform), &output.name);
        }
        Event::OutputRemoved(output) => {
          // Another output of the same name may be announced
          // already, before this one's removal arrived.
          let announced: Vec<String> = platform
            .outputs()
            .map(|output| output.name.clone())
            .collect();
          per_output::removed(&state, &mut Shown(platform), &output.name, &announced);
        }
        Event::WindowClosed(window) => {
          per_output::closed(&state, &mut Shown(platform), *window);
        }
      });
    let mut shell = Shell {
      event_loop,
      started: Instant::now(),
      first_frame: false,
      switches: SwitchLog::default(),
      control: None,
      cx,
    };
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| root(&shell.cx))) {
      let message = panic_message(payload.as_ref());
      tracing::error!("the root closure panicked: {message}");
      // Dropping the shell frees whatever root made before it panicked.
      drop(shell);
      return Err(Error::RootPanicked(message));
    }
    shell.started = Instant::now();
    Ok(shell)
  }

  /// Runs one iteration of the event loop, waiting up to `timeout`
  /// (forever if `None`) for something to happen.
  ///
  /// # Errors
  ///
  /// If the connection to the compositor failed.
  pub fn dispatch(&mut self, timeout: Option<Duration>) -> Result<(), crabshell_wayland::Error> {
    let commits = self.event_loop.platform().stats().commits;
    self.event_loop.dispatch(timeout)?;
    if !self.first_frame && self.event_loop.platform().stats().commits > 0 {
      self.first_frame = true;
      let elapsed = self.started.elapsed();
      tracing::info!(
        ms = elapsed.as_secs_f64() * 1000.0,
        "first frame committed {:.1} ms after the shell started",
        elapsed.as_secs_f64() * 1000.0
      );
    }
    self.log_switch(commits);
    self.tell_ready();
    Ok(())
  }

  /// Follows the supervisor on `stream`: tells it when the shell is
  /// ready, and on `save` writes the persisted values to `file` and exits
  /// with 0.
  pub(crate) fn follow_supervisor(&mut self, stream: UnixStream, file: Option<std::path::PathBuf>) {
    match stream.try_clone() {
      Ok(writer) => self.control = Some(writer),
      Err(error) => tracing::warn!(%error, "can't answer the supervisor"),
    }
    let service = self.cx.service::<Service>();
    let persisted = service
      .as_ref()
      .map(|service| Rc::clone(&service.persisted));
    let cx = self.cx.clone();
    control::listen(&self.event_loop.handle(), stream, move || {
      if let Some(service) = &service {
        service.ipc.remove_socket();
      }
      // The next shell starts its own; these would run on beside them.
      crabshell_services::stop_programs(&cx);
      let values = persisted.as_ref().map(|persisted| persisted.save());
      if let (Some(values), Some(file)) = (values, &file)
        && let Err(error) = persist::write(file, &values)
      {
        tracing::error!(path = %file.display(), %error, "can't save the persisted values");
      }
      tracing::info!("the supervisor asked for a reload; exiting");
      std::process::exit(0);
    });
  }

  /// Says `ready` to the supervisor once every panel shown has drawn a
  /// frame. A shell that shows no panel never says it.
  fn tell_ready(&mut self) {
    if self.control.is_none() {
      return;
    }
    let platform = self.event_loop.platform();
    let panels = platform
      .windows()
      .filter(|(_, window)| !window.is_popup())
      .filter_map(|(id, _)| platform.window_info(id))
      .map(|info| (info.shown, info.commits));
    if control::ready(panels)
      && let Some(mut stream) = self.control.take()
    {
      control::send_ready(&mut stream);
    }
  }

  /// Logs how long a workspace switch took to reach the screen, once a
  /// dispatch made the commit that shows it. `commits` is the count from
  /// before this dispatch. A config that never asked for `cx.sway()` has
  /// no switches, and this doesn't connect to Sway to look.
  fn log_switch(&mut self, commits: u64) {
    let changed_at = Sway::started(&self.cx).and_then(|sway| sway.changed_at());
    let platform = self.event_loop.platform();
    let after = platform.stats().commits;
    let drawing = || {
      platform.windows().any(|(id, window)| {
        window.has_damage() && platform.window_info(id).is_some_and(|info| info.configured)
      })
    };
    if let Some(event) = self
      .switches
      .after_dispatch(changed_at, commits, after, drawing)
    {
      let ms = event.elapsed().as_secs_f64() * 1000.0;
      tracing::info!(ms, "workspace switch on screen in {ms:.1} ms");
    }
  }

  /// Runs the event loop until the connection is lost, then logs it and
  /// exits with [`EXIT_NO_COMPOSITOR`].
  pub fn run(mut self) -> ! {
    loop {
      if let Err(error) = self.dispatch(None) {
        if let Some(service) = self.cx.service::<Service>() {
          service.ipc.remove_socket();
        }
        crabshell_services::stop_programs(&self.cx);
        tracing::error!(%error, "the shell stops");
        std::process::exit(EXIT_NO_COMPOSITOR);
      }
    }
  }

  /// The platform: its outputs, windows and counters. Its
  /// [`on_event`](Platform::on_event) listener is `per_output`'s: setting
  /// another replaces it, and `per_output` stops hearing of outputs.
  pub fn platform(&mut self) -> &mut Platform {
    self.event_loop.platform()
  }

  /// The root context, the one `root` was given.
  pub fn cx(&self) -> &Cx {
    &self.cx
  }
}

impl Drop for Shell {
  fn drop(&mut self) {
    // The services map holds per_output's state, whose builders hold
    // contexts, which hold the services map: clearing the builders
    // breaks that loop. Then everything under the root context goes.
    if let Some(service) = self.cx.service::<Service>() {
      let cleared = std::mem::replace(&mut *service.per_output.borrow_mut(), PerOutput::new());
      drop(cleared);
    }
    self.cx.owner().dispose();
  }
}

/// What the shell keeps in the root context's services, so `cx` can reach
/// the platform from anywhere.
struct Service {
  /// What `cx.persist` hands out, and saves on reload.
  persisted: Rc<Persisted>,
  per_output: Rc<RefCell<PerOutput<WindowId>>>,
  /// Puts a request in an idle callback, which gets the platform, while
  /// the loop is there. It holds the loop weakly, so a context kept
  /// somewhere doesn't keep the loop and its connection.
  idle: Idle,
  /// Wakes the loop so an idle callback runs soon, even if the request
  /// came from an effect, after this iteration's sources.
  signal: LoopSignal,
  /// The loop's handle, for sources added from a context, held weakly as
  /// `idle` holds it.
  handle: Handle<Platform>,
  /// What `cx.ipc` registers, and the socket it is called through.
  ipc: Rc<Ipc<Platform>>,
  /// The root context's owner, which outlasts every other.
  root: Owner,
  /// `cx.toplevels()`, once a config asked.
  toplevels: Cell<Option<Toplevels>>,
}

impl Service {
  /// Runs `f` with the platform, after the loop's current sources. A
  /// panic in it is logged; it doesn't end the shell.
  fn with_platform(&self, what: &'static str, f: impl FnOnce(&mut Platform) + 'static) {
    schedule(&self.idle, &self.signal, what, f);
  }
}

/// Every program's windows, from [`cx.toplevels()`](ShellExt::toplevels),
/// the shell's own floating windows included. It is `Copy`, so closures
/// capture it as they do a signal.
#[derive(Clone, Copy)]
pub struct Toplevels {
  list: ReadSignal<Vec<Toplevel>>,
  /// `None` on a context the shell didn't set up.
  requests: Option<ReadSignal<Requests>>,
}

impl Toplevels {
  /// Every window, in the order the compositor announced them. Empty
  /// until the compositor answers, and without the protocol.
  pub fn list(&self) -> ReadSignal<Vec<Toplevel>> {
    self.list
  }

  /// Asks the compositor to focus window `id`. Here and below, an id
  /// that isn't listed is ignored.
  pub fn activate(&self, id: u64) {
    self.send("toplevels.activate", move |platform| {
      platform.activate_toplevel(id);
    });
  }

  /// Asks window `id` to close. Its program may ask the user first.
  pub fn close(&self, id: u64) {
    self.send("toplevels.close", move |platform| {
      platform.close_toplevel(id)
    });
  }

  /// Asks the compositor to maximize window `id`, or to stop. Sway
  /// ignores it.
  pub fn set_maximized(&self, id: u64, maximized: bool) {
    self.send("toplevels.set_maximized", move |platform| {
      platform.set_toplevel_maximized(id, maximized);
    });
  }

  /// Asks the compositor to minimize window `id`, or to stop. Sway
  /// ignores it.
  pub fn set_minimized(&self, id: u64, minimized: bool) {
    self.send("toplevels.set_minimized", move |platform| {
      platform.set_toplevel_minimized(id, minimized);
    });
  }

  /// Asks the compositor to make window `id` fullscreen, on an output of
  /// its choosing, or to stop.
  pub fn set_fullscreen(&self, id: u64, fullscreen: bool) {
    self.send("toplevels.set_fullscreen", move |platform| {
      platform.set_toplevel_fullscreen(id, fullscreen);
    });
  }

  fn send(&self, what: &'static str, f: impl FnOnce(&mut Platform) + 'static) {
    // A handle kept after its shell went does nothing.
    if let Some(requests) = self.requests {
      requests.try_with(|requests| schedule(&requests.idle, &requests.signal, what, f));
    }
  }
}

/// How a [`Toplevels`] reaches the platform: the shell's loop.
struct Requests {
  idle: Idle,
  signal: LoopSignal,
}

/// Inserts an idle callback into the loop, if it is still there.
type Idle = Rc<dyn Fn(Box<dyn FnOnce(&mut Platform)>)>;

/// An [`Idle`] for `event_loop`'s loop, holding it weakly. (calloop
/// doesn't let the weak handle's type be named, so a closure keeps it.)
fn idle_inserter(event_loop: &EventLoop) -> Idle {
  let weak = event_loop.handle().downgrade();
  Rc::new(move |f: Box<dyn FnOnce(&mut Platform)>| {
    if let Some(handle) = weak.upgrade() {
      handle.insert_idle(f);
    }
  })
}

/// Runs `f` with the platform in an idle callback, if the loop is still
/// there, catching a panic in it.
fn schedule(
  idle: &Idle,
  signal: &LoopSignal,
  what: &'static str,
  f: impl FnOnce(&mut Platform) + 'static,
) {
  idle(Box::new(move |platform: &mut Platform| {
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| f(platform))) {
      tracing::error!("{what} panicked: {}", panic_message(payload.as_ref()));
    }
  }));
  signal.wakeup();
}

/// The platform as `per_output` uses it.
struct Shown<'a>(&'a mut Platform);

impl Host for Shown<'_> {
  type Id = WindowId;

  fn add_window(&mut self, window: Window) -> WindowId {
    self.0.add_window(window)
  }

  fn remove_window(&mut self, id: WindowId) {
    self.0.remove_window(id);
  }
}

/// What a context can ask of the shell. In the prelude, so `cx.per_output`
/// works with no other import.
pub trait ShellExt {
  /// Calls `f` for every output there is now and every one that appears
  /// later, and shows the windows it returns on it.
  ///
  /// Each output's call runs under a child owner of this context: when
  /// the output goes, that owner is disposed, so its windows and
  /// everything `f` created are dropped. If the compositor closes one of
  /// them, the rest go too, and nothing is built for that output until it
  /// is announced again. When this context's own owner is disposed, the
  /// call ends and every window it built goes. A panic in `f` is logged
  /// with its path, such as `per_output[DP-1] (called at src/main.rs:4)`,
  /// and the other outputs carry on.
  ///
  /// Call it from the root closure, or from inside another `per_output`'s
  /// `f`. Called from an effect, it would start another set of windows
  /// each time the effect runs.
  #[track_caller]
  fn per_output<W: IntoPanels>(&self, f: impl Fn(&Cx, &Output) -> W + 'static);

  /// Shows `window`, a floating window, for as long as this context
  /// lives: when its owner is disposed, the window goes. Its `visible`
  /// shows and hides it meanwhile.
  fn floating(&self, window: FloatingWindow);

  /// Whether the user has been idle for `timeout`: `true` once no key,
  /// pointer or touch input came for that long, and `false` again as soon
  /// as one does. The compositor says when, so nothing wakes the shell
  /// meanwhile, and windows that inhibit idle, such as a playing video or
  /// a bar's `inhibit_idle`, hold it off. The watch ends when this
  /// context's owner goes. Without `ext-idle-notify-v1` it stays `false`,
  /// and that is logged.
  fn idle_monitor(&self, timeout: Duration) -> ReadSignal<bool>;

  /// The main colors of the image file at `path`, most common first, as
  /// for a theme taken from a wallpaper: up to 2^`depth` of them, with
  /// `depth` clamped to 0..=8. The file is read as [`Image`] reads it,
  /// halved until its longer side is at most 128 pixels, and cut by
  /// median cut; see [`crabshell_render::quantize_colors`].
  ///
  /// It starts empty and fills once the colors are found, on a thread of
  /// its own, so a large image doesn't hold up the shell. A closure path
  /// is followed: when it changes, the colors are found again and
  /// replace the old ones, and an older path's never land after a newer
  /// one's. A file that can't be read is logged with its path and gives
  /// an empty list. Nothing is set once this context's owner goes.
  ///
  /// [`Image`]: crabshell_ui::Image
  #[track_caller]
  fn quantize_colors(&self, path: impl IntoProp<PathBuf>, depth: u8) -> ReadSignal<Vec<Color>>;
  /// Every program's windows, with their titles, app ids, outputs and
  /// states, and requests to focus, close, maximize, minimize and
  /// fullscreen them. Every caller shares one list, started on the first
  /// call, which lives as long as the shell and wakes it only when the
  /// compositor says a window changed. Without
  /// `wlr-foreign-toplevel-management` it stays empty, and that is logged.
  fn toplevels(&self) -> Toplevels;

  /// Looks icon names up in `theme`, such as `"Adwaita"`, instead of
  /// `hicolor`. Every window is repainted.
  fn set_icon_theme(&self, theme: impl Into<String>);

  /// A signal whose value survives a reload. `key` names it: asking again
  /// with the same key gives the same signal, which lives as long as the
  /// shell, not this context, so it outlasts an output unplugged and
  /// plugged back in. On a reload the shell saves it, and the next shell
  /// starts from the saved value. If that no longer fits `T`, as after an
  /// edit changed the type, `default` is used and that is logged once.
  fn persist<T>(&self, key: &str, default: T) -> Signal<T>
  where
    T: Serialize + DeserializeOwned + 'static;

  /// Lets the command line call `handler` as `crabshell ipc <target>
  /// <function> [args...]`, for as long as this context lives. It gets the
  /// arguments, and its `Ok` text is printed or its `Err` text reported
  /// as an error. `crabshell ipc` alone lists every target and function.
  ///
  /// Names are 1 to 64 ASCII letters, digits, `_`, `-` and `.`; others are
  /// refused and logged. Registering the same pair again replaces the
  /// older handler. It runs on the loop's thread, so the signals it sets
  /// show at once, and a slow one holds the whole shell up. A panic in it
  /// is logged, such as `ipc launcher.toggle (registered at
  /// src/main.rs:12:8) panicked`, and disables it. The socket is opened when
  /// the first handler registers.
  #[track_caller]
  fn ipc(
    &self,
    target: &str,
    function: &str,
    handler: impl Fn(&[String]) -> Result<String, String> + 'static,
  );
}

impl ShellExt for Cx {
  #[track_caller]
  fn per_output<W: IntoPanels>(&self, f: impl Fn(&Cx, &Output) -> W + 'static) {
    let at = Location::caller();
    let Some(service) = self.service::<Service>() else {
      tracing::error!(
        "cx.per_output at {at} needs the shell crabshell::run starts; nothing is built"
      );
      return;
    };
    // When this context's owner goes, the builder's windows are removed
    // soon after, without waiting for an output to come or go.
    let on_dead = {
      let state = Rc::clone(&service.per_output);
      let (idle, signal) = (Rc::clone(&service.idle), service.signal.clone());
      Box::new(move || {
        schedule(&idle, &signal, "per_output's cleanup", move |platform| {
          per_output::sweep(&state, &mut Shown(platform));
        });
      })
    };
    let builder = service.per_output.borrow_mut().register(
      self.clone(),
      Rc::new(move |cx: &Cx, output: &Output| f(cx, output).into_panels()),
      at,
      on_dead,
    );
    // The outputs there already are built for once the loop runs; the
    // ones announced later, when they are.
    let state = Rc::clone(&service.per_output);
    service.with_platform("per_output", move |platform| {
      let outputs: Vec<String> = platform
        .outputs()
        .map(|output| output.name.clone())
        .collect();
      per_output::existing(&state, &mut Shown(platform), builder, &outputs);
    });
  }

  fn floating(&self, window: FloatingWindow) {
    let Some(service) = self.service::<Service>() else {
      return tracing::error!(
        "cx.floating needs the shell crabshell::run starts; nothing is shown"
      );
    };
    // Building under a context that is gone panics; that is logged here.
    let Ok(window) = panic::catch_unwind(AssertUnwindSafe(|| Window::floating(self, window)))
    else {
      return tracing::warn!(
        "cx.floating was called with a context that is gone; nothing is shown"
      );
    };
    let id = Rc::new(Cell::new(None));
    // Removed soon after this context's owner goes, which also frees it.
    let (idle, signal, held) = (
      Rc::clone(&service.idle),
      service.signal.clone(),
      Rc::clone(&id),
    );
    let gone = OnDispose(Some(Box::new(move || {
      schedule(&idle, &signal, "cx.floating's cleanup", move |platform| {
        if let Some(id) = held.take() {
          platform.remove_window(id);
        }
      });
    })));
    if panic::catch_unwind(AssertUnwindSafe(|| self.owner().signal(gone))).is_err() {
      return tracing::warn!(
        "cx.floating was called with a context that is gone; nothing is shown"
      );
    }
    service.with_platform("cx.floating", move |platform| {
      id.set(Some(platform.add_window(window)));
    });
  }

  fn idle_monitor(&self, timeout: Duration) -> ReadSignal<bool> {
    let idle = self.signal(false);
    let Some(service) = self.service::<Service>() else {
      tracing::error!("cx.idle_monitor needs the shell crabshell::run starts; it stays false");
      return idle.read_only();
    };
    let watch = Rc::new(Cell::new(None));
    // Ended soon after this context's owner goes, which also frees it.
    let (queue, signal, held) = (
      Rc::clone(&service.idle),
      service.signal.clone(),
      Rc::clone(&watch),
    );
    let gone = OnDispose(Some(Box::new(move || {
      schedule(
        &queue,
        &signal,
        "cx.idle_monitor's cleanup",
        move |platform| {
          if let Some(watch) = held.take() {
            platform.unwatch_idle(watch);
          }
        },
      );
    })));
    // The signal above was made, so the owner is there.
    self.owner().signal(gone);
    service.with_platform("cx.idle_monitor", move |platform| {
      watch.set(platform.watch_idle(timeout, move |now| {
        idle.try_set(now);
      }));
    });
    idle.read_only()
  }

  #[track_caller]
  fn quantize_colors(&self, path: impl IntoProp<PathBuf>, depth: u8) -> ReadSignal<Vec<Color>> {
    let Some(service) = self.service::<Service>() else {
      tracing::error!("cx.quantize_colors needs the shell crabshell::run starts; it stays empty");
      return self.signal(Vec::new()).read_only();
    };
    colors::follow(
      self,
      Rc::clone(&service.handle),
      path.into_prop(),
      colors::quantizer(depth),
    )
  }

  fn toplevels(&self) -> Toplevels {
    let Some(service) = self.service::<Service>() else {
      tracing::error!("cx.toplevels needs the shell crabshell::run starts; the list stays empty");
      return Toplevels {
        list: self.signal(Vec::new()).read_only(),
        requests: None,
      };
    };
    if let Some(toplevels) = service.toplevels.get() {
      return toplevels;
    }
    // Under the root owner, so every caller can share them.
    let list = service.root.signal(Vec::new());
    let requests = service.root.signal(Requests {
      idle: Rc::clone(&service.idle),
      signal: service.signal.clone(),
    });
    let toplevels = Toplevels {
      list: list.read_only(),
      requests: Some(requests.read_only()),
    };
    service.toplevels.set(Some(toplevels));
    service.with_platform("cx.toplevels", move |platform| {
      platform.watch_toplevels(move |now| {
        // An unchanged list isn't set, so nothing that reads it reruns.
        if list.try_with(|old| old.as_slice() != now) == Some(true) {
          list.try_set(now.to_vec());
        }
      });
    });
    toplevels
  }

  fn persist<T>(&self, key: &str, default: T) -> Signal<T>
  where
    T: Serialize + DeserializeOwned + 'static,
  {
    let Some(service) = self.service::<Service>() else {
      tracing::error!(
        key,
        "cx.persist needs the shell crabshell::run starts; the value isn't kept"
      );
      return self.signal(default);
    };
    // One that isn't kept is under this context, so it goes with it.
    service
      .persisted
      .signal(key, default)
      .unwrap_or_else(|default| self.signal(default))
  }

  #[track_caller]
  fn ipc(
    &self,
    target: &str,
    function: &str,
    handler: impl Fn(&[String]) -> Result<String, String> + 'static,
  ) {
    let at = Location::caller();
    let Some(service) = self.service::<Service>() else {
      return tracing::error!(
        "cx.ipc at {at} needs the shell crabshell::run starts; nothing is registered"
      );
    };
    service
      .ipc
      .register(self, target, function, Box::new(handler), at);
  }

  fn set_icon_theme(&self, theme: impl Into<String>) {
    let theme = theme.into();
    let Some(service) = self.service::<Service>() else {
      tracing::error!(
        theme,
        "cx.set_icon_theme needs the shell crabshell::run starts"
      );
      return;
    };
    service.with_platform("set_icon_theme", move |platform| {
      platform.set_icon_theme(theme);
    });
  }
}

/// Runs its closure when dropped, as when the owner keeping it is
/// disposed.
pub(crate) struct OnDispose(pub(crate) Option<Box<dyn FnOnce()>>);

impl Drop for OnDispose {
  fn drop(&mut self) {
    if let Some(f) = self.0.take() {
      f();
    }
  }
}

pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    (*message).to_owned()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    String::from("(panic payload is not a string)")
  }
}
