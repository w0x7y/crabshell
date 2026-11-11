//! The shell: the platform's event loop, the root context and its
//! services, and what `cx` can ask of the shell.

use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use crabshell_wayland::{EventLoop, LoopSignal, Platform};
use tracing::level_filters::LevelFilter;

/// The exit status when the root closure panicked: the config is broken.
pub const EXIT_ROOT_PANICKED: i32 = 1;

/// The exit status when the compositor can't be reached, or the connection
/// to it is lost, as at logout.
pub const EXIT_NO_COMPOSITOR: i32 = 2;

/// Starts the shell: connects to the compositor, calls `root` with the
/// root context, and runs the event loop. It never returns.
///
/// `root` sets the shell up. Errors are logged, and the process exits with:
///
/// - [`EXIT_ROOT_PANICKED`] (1) if `root` panics: the config is broken;
/// - [`EXIT_NO_COMPOSITOR`] (2) if there is no compositor to connect to,
///   it lacks a protocol the shell needs, or the connection is lost later,
///   as at logout.
///
/// Logs go to stderr through `tracing`, at the level `CRABSHELL_LOG` names
/// (`error`, `warn`, `info`, `debug` or `trace`; `info` if unset or empty;
/// any other value is warned about). The time from this call to the first
/// committed frame is logged at `info`.
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
    cx: Cx,
}

impl Shell {
    /// Sets up the shell on `event_loop` and calls `root` with the root
    /// context. Nothing is shown before the first [`dispatch`](Self::dispatch).
    ///
    /// # Errors
    ///
    /// If `root` panics. The panic is logged too.
    pub fn new(event_loop: EventLoop, root: impl FnOnce(&Cx)) -> Result<Shell, Error> {
        let runtime = Runtime::current();
        let cx = Cx::new(runtime.new_owner().named("root"));
        cx.provide(Service {
            idle: idle_inserter(&event_loop),
            signal: event_loop.signal(),
        });
        let mut shell = Shell {
            event_loop,
            started: Instant::now(),
            first_frame: false,
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
        Ok(())
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

    /// The platform: its outputs, windows and counters.
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
        // Everything under the root context goes.
        self.cx.owner().dispose();
    }
}

/// What the shell keeps in the root context's services, so `cx` can reach
/// the platform from anywhere.
struct Service {
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

/// What a context can ask of the shell. In the prelude, so its methods
/// work with no other import.
pub trait ShellExt {
    /// Looks icon names up in `theme`, such as `"Adwaita"`, instead of
    /// `hicolor`. Every window is repainted.
    fn set_icon_theme(&self, theme: impl Into<String>);
}

impl ShellExt for Cx {
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
