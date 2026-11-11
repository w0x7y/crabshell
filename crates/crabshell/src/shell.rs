//! The shell: the platform's event loop, the root context and its
//! services, and what `cx` can ask of the shell.

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe, Location};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_services::Sway;
use crabshell_ui::{Cx, Output, Window};
use crabshell_wayland::{Event, EventLoop, LoopSignal, Platform, WindowId};
use tracing::level_filters::LevelFilter;

use crate::per_output::{self, Host, IntoPanels, PerOutput};
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
    let mut shell = match Shell::new(event_loop, root) {
        Ok(shell) => shell,
        Err(error) => {
            tracing::error!(%error, "the shell couldn't start");
            std::process::exit(EXIT_ROOT_PANICKED);
        }
    };
    shell.started = started;
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
    pub fn new(mut event_loop: EventLoop, root: impl FnOnce(&Cx)) -> Result<Shell, Error> {
        let runtime = Runtime::current();
        let cx = Cx::new(runtime.new_owner().named("root"));
        // Nothing starts here: each service connects when root first asks.
        crabshell_services::provide(&cx, &event_loop.handle());
        let state = Rc::new(RefCell::new(PerOutput::new()));
        cx.provide(Service {
            per_output: Rc::clone(&state),
            idle: idle_inserter(&event_loop),
            signal: event_loop.signal(),
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
        Ok(())
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
            let cleared =
                std::mem::replace(&mut *service.per_output.borrow_mut(), PerOutput::new());
            drop(cleared);
        }
        self.cx.owner().dispose();
    }
}

/// What the shell keeps in the root context's services, so `cx` can reach
/// the platform from anywhere.
struct Service {
    per_output: Rc<RefCell<PerOutput<WindowId>>>,
    /// Puts a request in an idle callback, which gets the platform, while
    /// the loop is there. It holds the loop weakly, so a context kept
    /// somewhere doesn't keep the loop and its connection.
    idle: Idle,
    /// Wakes the loop so an idle callback runs soon, even if the request
    /// came from an effect, after this iteration's sources.
    signal: LoopSignal,
}

impl Service {
    /// Runs `f` with the platform, after the loop's current sources. A
    /// panic in it is logged; it doesn't end the shell.
    fn with_platform(&self, what: &'static str, f: impl FnOnce(&mut Platform) + 'static) {
        schedule(&self.idle, &self.signal, what, f);
    }
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

    /// Looks icon names up in `theme`, such as `"Adwaita"`, instead of
    /// `hicolor`. Every window is repainted.
    fn set_icon_theme(&self, theme: impl Into<String>);
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

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        String::from("(panic payload is not a string)")
    }
}
