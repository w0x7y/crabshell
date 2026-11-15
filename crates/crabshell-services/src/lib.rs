//! The services a Crabshell bar reads: the clock, Sway's and Hyprland's
//! workspaces, audio, the tray, power, notifications, the installed
//! applications, PAM, greetd and a polkit agent.
//!
//! The shell calls [`provide`] once, with its root context and event loop.
//! After that, any context hands out services through [`ServicesExt`],
//! such as `cx.clock(Precision::Minute)`. Each service starts the first
//! time a config asks for it and is shared by every caller, so two outputs
//! never mean two connections. Its signals belong to the root context's
//! owner: bars on every output share them, and they go when the shell does.
//!
//! Asked for on a context the shell didn't set up, as in a ui test, a
//! service logs once and hands out a value that never changes.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod audio;
mod bluetooth;
mod clock;
mod dbus;
mod desktop_entries;
mod file;
mod greetd;
mod hyprland;
mod lines;
mod mpris;
mod network;
mod notifications;
mod pam;
mod polkit;
mod power;
mod process;
mod socket;
mod spawn;
mod sway;
#[cfg(test)]
mod testing;
mod tray;
mod watch;

use std::cell::{Cell, OnceCell, RefCell};
use std::future::{Future, poll_fn};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use calloop::channel::Channel;
use calloop::futures::{Executor, Scheduler};
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal, Signal};
use crabshell_ui::Cx;

pub use audio::{Audio, AudioNode, AudioStream, Sink};
pub use bluetooth::{Bluetooth, BluetoothAdapter, BluetoothDevice};
pub use clock::Precision;
pub use desktop_entries::{DesktopAction, DesktopEntries, DesktopEntry};
pub use file::FileView;
pub use greetd::{GreetMessage, GreetState, Greetd};
pub use hyprland::{Hyprland, HyprlandEvent, HyprlandMonitor, HyprlandToplevel, HyprlandWorkspace};
pub use mpris::{LoopStatus, Mpris, PlaybackStatus, Player};
pub use network::{Network, NetworkDevice, NetworkKind, NetworkState, WifiNetwork};
pub use notifications::{Notification, Notifications, Timeout, Urgency};
pub use pam::{Pam, PamMessage, PamResult, PamState};
pub use polkit::{AuthRequest, AuthState, PolkitAgent};
pub use power::{
  BatteryState, DeviceKind, PerformanceDegradation, PowerDevice, PowerProfile, PowerProfiles,
  UPower,
};
pub use process::{IntoCommand, Process};
pub use socket::Socket;
pub use sway::{Sway, SwayEvent, SwayEventKind, SwayOutput, Workspace};
pub use tray::{
  MenuEntry, MenuToggle, Tray, TrayCategory, TrayHost, TrayItem, TrayMenu, TrayStatus,
};

/// Makes the services reachable from `cx` and every context made from it.
/// The shell calls this once, with its root context and its event loop.
///
/// Nothing starts here: each service starts when a config first asks for
/// it. The loop is kept weakly, so it and the compositor connection still
/// go when the shell drops.
pub fn provide<D: 'static>(cx: &Cx, handle: &LoopHandle<'static, D>) {
  provide_with(cx, handle, Config::system());
}

/// [`provide`] with the system's parts swapped for a test's.
fn provide_with<D: 'static>(cx: &Cx, handle: &LoopHandle<'static, D>, config: Config) {
  let shared = Rc::new(Shared::new(cx.owner(), handle, config));
  cx.provide(clock::Clocks::new(Rc::clone(&shared)));
  cx.provide(sway::Service::new(Rc::clone(&shared)));
  cx.provide(hyprland::Service::new(Rc::clone(&shared)));
  cx.provide(audio::AudioService::new(Rc::clone(&shared)));
  cx.provide(tray::Service::new(Rc::clone(&shared)));
  cx.provide(power::UPowerService::new(Rc::clone(&shared)));
  cx.provide(power::ProfilesService::new(Rc::clone(&shared)));
  cx.provide(bluetooth::BluetoothService::new(Rc::clone(&shared)));
  cx.provide(notifications::Service::new(Rc::clone(&shared)));
  cx.provide(mpris::Service::new(Rc::clone(&shared)));
  cx.provide(network::NetworkService::new(Rc::clone(&shared)));
  cx.provide(socket::Sockets(Rc::clone(&shared)));
  cx.provide(process::Processes::new(Rc::clone(&shared)));
  cx.provide(desktop_entries::Service::new(Rc::clone(&shared)));
  cx.provide(file::Files(Rc::clone(&shared)));
  cx.provide(greetd::Service::new(Rc::clone(&shared)));
  cx.provide(pam::Service(Rc::clone(&shared)));
  cx.provide(polkit::Service::new(Rc::clone(&shared)));
}

/// Stops every program a config started with `cx.process`. The shell
/// calls this before it exits, since `std::process::exit` skips the drops
/// that would, and each program has a process group of its own.
#[doc(hidden)]
pub fn stop_programs(cx: &Cx) {
  process::stop_all(cx);
}

/// The services a context hands out.
pub trait ServicesExt {
  /// The time, truncated to `precision`: it changes on each whole second
  /// or minute and holds the one just passed, in the system time zone.
  /// Every caller asking for the same precision gets the same signal.
  ///
  /// A minute clock wakes the shell once a minute and at no other time.
  /// After a suspend, and when timedated changes the time zone, it is
  /// right again at once. A zone changed any other way shows from the
  /// next tick.
  fn clock(&self, precision: Precision) -> ReadSignal<jiff::Zoned>;

  /// Sway's workspaces, outputs, commands and events, over the IPC socket
  /// at `$SWAYSOCK`, as Quickshell's `I3`. Every caller shares one
  /// connection, made on the first call.
  ///
  /// The lists are empty while Sway isn't there. When `SWAYSOCK` is unset
  /// or the first connect fails, they stay empty and the service never
  /// wakes the shell. When a working connection closes, they are empty
  /// until a reconnect, tried every 2 s, works. While connected, it wakes
  /// the shell only for a workspace event, an output event once a config
  /// asked for the outputs, and the kinds of event a handler asked for.
  fn sway(&self) -> Sway;

  /// Hyprland's workspaces, monitors and windows, over its IPC sockets
  /// in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`. Every
  /// caller shares one event connection, made on the first call.
  ///
  /// The lists are empty while Hyprland isn't there: when the signature
  /// is unset or its sockets are missing, they stay empty, which is
  /// logged once, and the service never wakes the shell. When Hyprland
  /// closes its event socket, the lists empty and nothing reconnects.
  /// While connected, it wakes the shell only when Hyprland sends
  /// something.
  fn hyprland(&self) -> Hyprland;

  /// The audio server's default sink, with its volume and mute. Every
  /// caller shares one connection, which starts on the loop's next turn
  /// and never waits for the server. It wakes the shell only when the
  /// server reports a change.
  ///
  /// With no server, the sink is `None` and the service tries again
  /// every 2 s, for up to a minute after startup and after each drop of
  /// a working connection. Then it stops, logs that once and never wakes
  /// the shell again; a restart tries again.
  fn audio(&self) -> Audio;

  /// The system tray: every app's StatusNotifierItem on the session bus.
  /// Every caller shares one connection, which starts on the loop's next
  /// turn and never waits for the bus.
  ///
  /// The shell runs the StatusNotifierWatcher while no other program
  /// does, and otherwise reads the list from the one that runs, taking
  /// over when it leaves. With no session bus the list stays empty and
  /// the service never wakes the shell. It wakes the shell only when
  /// something arrives on the bus for it.
  fn tray(&self) -> TrayHost;

  /// UPower's batteries and power supplies, on the system bus. Every
  /// caller shares one connection, which starts on the loop's next turn.
  ///
  /// While UPower isn't running the lists are empty, and they fill when
  /// it starts. With no system bus they stay empty and the service never
  /// wakes the shell. It wakes the shell only when UPower says something
  /// changed.
  fn upower(&self) -> UPower;

  /// The power profile, from power-profiles-daemon on the system bus.
  /// Every caller shares one connection. It behaves as
  /// [`upower`](Self::upower) does while the daemon isn't there.
  fn power_profiles(&self) -> PowerProfiles;

  /// Bluetooth adapters and devices, from BlueZ on the system bus. Every
  /// caller shares one connection. It behaves as [`upower`](Self::upower)
  /// does while BlueZ isn't there, and wakes the shell only when BlueZ
  /// says something changed.
  fn bluetooth(&self) -> Bluetooth;

  /// The notification server on the session bus: what apps sent, and
  /// the popups still showing. Every caller shares one connection, which
  /// starts on the loop's next turn and never waits for the bus.
  ///
  /// The shell serves while no other program does. While another daemon
  /// has the name, the shell waits for it to leave, and keeps what it was
  /// sent if one takes the name over. With no session bus the lists stay
  /// empty and the service never wakes the shell. It wakes the shell only
  /// when an app calls and when a popup expires.
  fn notifications(&self) -> Notifications;

  /// The media players on the session bus, over MPRIS, such as a music
  /// player or a browser playing a video. Every caller shares one
  /// connection, which starts on the loop's next turn and never waits
  /// for the bus.
  ///
  /// With no session bus the list stays empty and the service never
  /// wakes the shell. It wakes the shell only when a player says
  /// something changed. MPRIS doesn't say when the position moves, so a
  /// bar works it out with [`Player::position_at`].
  fn mpris(&self) -> Mpris;

  /// Network devices and Wi-Fi networks, from NetworkManager on the
  /// system bus. Every caller shares one connection. It behaves as
  /// [`upower`](Self::upower) does while NetworkManager isn't there, and
  /// wakes the shell only when NetworkManager says something changed.
  fn network(&self) -> Network;

  /// A connection to the Unix socket at `path`, made at once, as
  /// Quickshell's `Socket`. Each call opens its own, which closes when
  /// this context goes. It wakes the shell only when the other end sends
  /// something or takes what was written.
  fn socket(&self, path: impl Into<PathBuf>) -> Socket;

  /// Runs the program `command` builds, as Quickshell's `Process`, and
  /// keeps what it prints and how it ended. `command` returns a
  /// [`Command`](std::process::Command) or a list such as
  /// `["uptime", "-p"]`. It starts on the loop's next turn, and again
  /// whenever a signal `command` read changes or [`Process::restart`] is
  /// called, stopping the run before. It is stopped when this context
  /// goes. It wakes the shell only when the program prints, takes input
  /// or ends.
  fn process<C: IntoCommand>(&self, command: impl Fn() -> C + 'static) -> Process;

  /// Starts `command` and lets it run on its own, as a launcher does:
  /// its input and outputs are empty, it has a process group of its own,
  /// and it is reaped when it ends. A failure to start is logged.
  fn exec_detached(&self, command: impl IntoCommand);

  /// The text file at `path`, as Quickshell's `FileView`: read now, and
  /// again when it changes on disk. Each call reads its own, which goes
  /// with this context. It is read on the main thread, so a file on a
  /// slow disk delays the frame.
  fn file_view(&self, path: impl Into<PathBuf>) -> FileView;

  /// greetd, for a greeter: makes a session for a user, answers its
  /// questions and starts it, over the socket `GREETD_SOCK` names. One
  /// connection per shell, made at the first request; it wakes the shell
  /// only when greetd answers.
  fn greetd(&self) -> Greetd;

  /// A value kept in the JSON file at `path`, as Quickshell's
  /// `JsonAdapter`: `default` until the file is read, the file's value
  /// once it is, and the file rewritten whenever the config sets another.
  /// A missing file, and its directory, is made at the first set. A file
  /// that isn't `T`'s
  /// JSON is logged and left alone, and the value stays.
  fn json_file<T>(&self, path: impl Into<PathBuf>, default: T) -> Signal<T>
  where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + 'static;

  /// The installed applications, from the desktop entries in the XDG
  /// data directories: what a launcher lists and starts, and where a
  /// window's app id finds its icon. Every caller shares one list, read
  /// on the loop's next turn after the first call.
  ///
  /// The directories are watched, and the list is read again once the
  /// files have been still for 100 ms after a change. It wakes the shell
  /// at no other time.
  fn desktop_entries(&self) -> DesktopEntries;

  /// Authentication through the PAM service `service`, the name of a
  /// file in `/etc/pam.d`, as a lock screen asks for the user's
  /// password. Each call has its own, which goes with this context.
  ///
  /// Nothing runs until [`Pam::start`]. Each attempt runs on a thread of
  /// its own, since libpam blocks, and wakes the shell only when PAM asks
  /// or says something, or the attempt ends. After a success PAM's
  /// account step runs too, which clears `pam_faillock`'s count of past
  /// failures; its refusal is only logged. An attempt ended while PAM
  /// waits for an answer counts as a failure, as [`Pam::start`] says.
  fn pam(&self, service: &str) -> Pam;

  /// The shell's polkit authentication agent, as Quickshell's
  /// `PolkitAgent`: when a program asks polkit for something only some
  /// users may do, its request shows here for the config's dialog. The
  /// first call registers the agent with polkit on the system bus for
  /// this session; a shell that never calls it never reaches polkit.
  ///
  /// A refused registration is logged and no requests arrive. It wakes
  /// the shell only when polkit calls and while a request is answered.
  fn polkit_agent(&self) -> PolkitAgent;
}

impl ServicesExt for Cx {
  fn clock(&self, precision: Precision) -> ReadSignal<jiff::Zoned> {
    clock::clock(self, precision)
  }

  fn sway(&self) -> Sway {
    sway::sway(self)
  }

  fn hyprland(&self) -> Hyprland {
    hyprland::hyprland(self)
  }

  fn audio(&self) -> Audio {
    audio::audio(self)
  }

  fn tray(&self) -> TrayHost {
    tray::tray(self)
  }

  fn upower(&self) -> UPower {
    power::upower(self)
  }

  fn power_profiles(&self) -> PowerProfiles {
    power::power_profiles(self)
  }

  fn bluetooth(&self) -> Bluetooth {
    bluetooth::bluetooth(self)
  }

  fn notifications(&self) -> Notifications {
    notifications::notifications(self)
  }

  fn mpris(&self) -> Mpris {
    mpris::mpris(self)
  }

  fn network(&self) -> Network {
    network::network(self)
  }

  fn socket(&self, path: impl Into<PathBuf>) -> Socket {
    socket::socket(self, path.into())
  }

  #[track_caller]
  fn process<C: IntoCommand>(&self, command: impl Fn() -> C + 'static) -> Process {
    process::process(self, command)
  }

  fn exec_detached(&self, command: impl IntoCommand) {
    process::exec_detached(self, command);
  }

  fn file_view(&self, path: impl Into<PathBuf>) -> FileView {
    file::file_view(self, path.into())
  }

  fn greetd(&self) -> Greetd {
    greetd::greetd(self)
  }

  fn json_file<T>(&self, path: impl Into<PathBuf>, default: T) -> Signal<T>
  where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + 'static,
  {
    file::json_file(self, path.into(), default)
  }

  fn desktop_entries(&self) -> DesktopEntries {
    desktop_entries::desktop_entries(self)
  }

  fn pam(&self, service: &str) -> Pam {
    pam::pam(self, service)
  }

  fn polkit_agent(&self) -> PolkitAgent {
    polkit::polkit_agent(self)
  }
}

/// What every service of one shell starts from.
struct Shared {
  /// The root context's owner. Services create their signals under it,
  /// so bars on every output share them and they go with the shell.
  root: Owner,
  /// The event loop, held weakly. Services' timers and futures live in
  /// the loop and hold the services, so a strong handle here would be an
  /// `Rc` cycle that keeps the loop and the compositor connection alive
  /// after the shell drops. calloop's weak handle has no type we can
  /// name, so a closure keeps it.
  event_loop: Box<dyn Fn() -> Option<Box<dyn EventLoop>>>,
  /// The one futures executor every service's D-Bus calls run on, added
  /// to the loop on first use. `None` if that failed.
  executor: OnceCell<Option<Scheduler<()>>>,
  /// The one inotify instance, made for the first directory watched.
  watcher: watch::Watcher,
  config: Config,
}

impl Shared {
  fn new<D: 'static>(root: Owner, handle: &LoopHandle<'static, D>, config: Config) -> Shared {
    let weak = handle.downgrade();
    Shared {
      root,
      event_loop: Box::new(move || {
        weak
          .upgrade()
          .map(|handle| Box::new(handle) as Box<dyn EventLoop>)
      }),
      executor: OnceCell::new(),
      watcher: watch::Watcher::default(),
      config,
    }
  }

  /// The event loop, while it is still there.
  fn event_loop(&self) -> Option<Box<dyn EventLoop>> {
    (self.event_loop)()
  }

  /// Runs `future` on the main thread, between the loop's other sources,
  /// so no D-Bus call delays a frame.
  fn spawn(&self, future: impl Future<Output = ()> + 'static) {
    let scheduler = self.executor.get_or_init(|| {
      let (executor, scheduler) = calloop::futures::executor()
        .inspect_err(|error| tracing::error!(%error, "no executor for the services"))
        .ok()?;
      self
        .event_loop()?
        .insert_executor(executor)
        .then_some(scheduler)
    });
    if let Some(scheduler) = scheduler
      && let Err(error) = scheduler.schedule(future)
    {
      tracing::error!(%error, "a service's future couldn't start");
    }
  }
}

impl Shared {
  /// Awaits `call`, or gives up after `limit` with a timed-out error. The
  /// timer is the loop's and goes as soon as the call ends. zbus's own
  /// timeout left its timer behind, which woke zbus's thread once, `limit`
  /// after every call, while nothing changed.
  async fn timeout<T>(
    &self,
    limit: Duration,
    call: impl Future<Output = zbus::Result<T>>,
  ) -> zbus::Result<T> {
    let fired = Rc::new(Cell::new(false));
    let waker: Rc<RefCell<Option<Waker>>> = Rc::default();
    let tick = {
      let (fired, waker) = (Rc::clone(&fired), Rc::clone(&waker));
      Box::new(move || {
        fired.set(true);
        if let Some(waker) = waker.take() {
          waker.wake();
        }
        TimeoutAction::Drop
      })
    };
    let token = self
      .event_loop()
      .and_then(|event_loop| event_loop.insert_timer(Instant::now() + limit, tick));
    let _timer = RemoveOnDrop(self, token);
    let mut call = pin!(call);
    poll_fn(|context| {
      if let Poll::Ready(result) = call.as_mut().poll(context) {
        return Poll::Ready(result);
      }
      if fired.get() {
        let error = std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in time");
        return Poll::Ready(Err(zbus::Error::InputOutput(error.into())));
      }
      waker.replace(Some(context.waker().clone()));
      Poll::Pending
    })
    .await
  }
}

/// Removes a loop source when dropped, as a call that ends removes its
/// timer.
struct RemoveOnDrop<'a>(&'a Shared, Option<RegistrationToken>);

impl Drop for RemoveOnDrop<'_> {
  fn drop(&mut self) {
    if let (Some(token), Some(event_loop)) = (self.1, self.0.event_loop()) {
      event_loop.remove(token);
    }
  }
}

/// The parts of the event loop services use. It hides the loop's data
/// type, since one services map serves any shell.
trait EventLoop {
  /// Calls `tick` at `deadline`, then again at whatever it returns.
  /// `None`, logged, if the loop refused it.
  fn insert_timer(
    &self,
    deadline: Instant,
    tick: Box<dyn FnMut() -> TimeoutAction>,
  ) -> Option<RegistrationToken>;

  /// Calls `ready` whenever `fd` is ready for `interest`. The loop owns
  /// `fd` and closes it when the source is removed. `None`, logged, if
  /// the loop refused it.
  fn insert_fd(
    &self,
    fd: OwnedFd,
    interest: Interest,
    ready: Box<dyn FnMut()>,
  ) -> Option<RegistrationToken>;

  /// Removes a source. One that is already gone is ignored.
  fn remove(&self, token: RegistrationToken);

  /// Adds the services' executor. `false`, logged, if the loop refused it.
  fn insert_executor(&self, executor: Executor<()>) -> bool;

  /// Calls `on_message` with each message libpulse's thread sends on
  /// `channel`. `false`, logged, if the loop refused it.
  fn insert_audio_channel(
    &self,
    channel: Channel<audio::Message>,
    on_message: Box<dyn FnMut(audio::Message)>,
  ) -> bool;

  /// Calls `on_event` with each event a PAM attempt's thread sends on
  /// `channel`, and with `Closed` once the thread is gone. `None`, logged,
  /// if the loop refused it.
  fn insert_pam_channel(
    &self,
    channel: Channel<pam::Event>,
    on_event: Box<dyn FnMut(calloop::channel::Event<pam::Event>)>,
  ) -> Option<RegistrationToken>;
}

impl<D: 'static> EventLoop for LoopHandle<'static, D> {
  fn insert_timer(
    &self,
    deadline: Instant,
    mut tick: Box<dyn FnMut() -> TimeoutAction>,
  ) -> Option<RegistrationToken> {
    self
      .insert_source(Timer::from_deadline(deadline), move |_, _, _| tick())
      .inspect_err(|error| tracing::error!(%error, "a service's timer couldn't be added"))
      .ok()
  }

  fn insert_fd(
    &self,
    fd: OwnedFd,
    interest: Interest,
    mut ready: Box<dyn FnMut()>,
  ) -> Option<RegistrationToken> {
    let source = Generic::new(fd, interest, Mode::Level);
    self
      .insert_source(source, move |_, _, _| {
        ready();
        Ok(PostAction::Continue)
      })
      .inspect_err(|error| tracing::error!(%error, "a service's socket couldn't be added"))
      .ok()
  }

  fn remove(&self, token: RegistrationToken) {
    LoopHandle::remove(self, token);
  }

  fn insert_executor(&self, executor: Executor<()>) -> bool {
    self
      .insert_source(executor, |(), _, _| {})
      .inspect_err(|error| tracing::error!(%error, "the services' executor couldn't be added"))
      .is_ok()
  }

  fn insert_audio_channel(
    &self,
    channel: Channel<audio::Message>,
    mut on_message: Box<dyn FnMut(audio::Message)>,
  ) -> bool {
    self
      .insert_source(channel, move |event, _, _| {
        if let calloop::channel::Event::Msg(message) = event {
          on_message(message);
        }
      })
      .inspect_err(|error| tracing::error!(%error, "the audio channel couldn't be added"))
      .is_ok()
  }

  fn insert_pam_channel(
    &self,
    channel: Channel<pam::Event>,
    mut on_event: Box<dyn FnMut(calloop::channel::Event<pam::Event>)>,
  ) -> Option<RegistrationToken> {
    self
      .insert_source(channel, move |event, _, _| on_event(event))
      .inspect_err(|error| tracing::error!(%error, "a PAM attempt's channel couldn't be added"))
      .ok()
  }
}

/// Where the services find the system. Tests point it at their own files
/// and a private bus.
struct Config {
  /// The link that names the system time zone, `/etc/localtime`. `None`
  /// when `TZ` is set, since it names the zone then, as it does for libc.
  localtime: Option<PathBuf>,
  /// The system bus, where logind, timedated, UPower,
  /// power-profiles-daemon, BlueZ, NetworkManager and polkit are. `None`
  /// for none.
  system_bus: Option<zbus::Address>,
  /// The session bus, where tray items, notifying apps and media
  /// players are. `None` for none.
  session_bus: Option<zbus::Address>,
  /// Sway's IPC socket, from `SWAYSOCK`. `None` when that is unset.
  sway: Option<sway::Socket>,
  /// The directory of Hyprland's sockets,
  /// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE`. `None` when
  /// either is unset.
  hyprland: Option<PathBuf>,
  /// The audio server. `None` for none, so a test never reaches the
  /// user's.
  audio: Option<audio::Server>,
  /// How long a popup shows when its app leaves that to the server: 5 s,
  /// and less in tests.
  popup_timeout: Duration,
  /// greetd's socket, from `GREETD_SOCK`. `None` when that is unset.
  greetd: Option<PathBuf>,
  /// Where the desktop entries are. Tests give directories of their own.
  desktop_entries: desktop_entries::Sources,
  /// What `cx.pam` authenticates with: libpam, and a script in tests, so
  /// no test counts toward locking a real account.
  pam: Arc<dyn pam::Backend>,
  /// polkit's setuid helper, which checks answers with PAM. Tests give a
  /// script of their own.
  polkit_helper: PathBuf,
  /// polkit's socket for its helper, tried before `polkit_helper`.
  polkit_socket: PathBuf,
}

impl Config {
  fn system() -> Config {
    Config {
      localtime: std::env::var_os("TZ")
        .is_none()
        .then(|| PathBuf::from("/etc/localtime")),
      system_bus: zbus::Address::system()
        .inspect_err(|error| tracing::warn!(%error, "no system bus address"))
        .ok(),
      session_bus: zbus::Address::session()
        .inspect_err(|error| tracing::warn!(%error, "no session bus address"))
        .ok(),
      sway: std::env::var_os("SWAYSOCK")
        .filter(|path| !path.is_empty())
        .map(|path| sway::Socket {
          path: path.into(),
          retry: sway::RETRY,
        }),
      hyprland: std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .zip(std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|sig| !sig.is_empty()))
        .map(|(dir, signature)| PathBuf::from(dir).join("hypr").join(signature)),
      audio: Some(audio::Server {
        address: None,
        retry: audio::RETRY,
        retry_for: audio::RETRY_FOR,
      }),
      popup_timeout: notifications::POPUP_TIMEOUT,
      greetd: std::env::var_os("GREETD_SOCK")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from),
      desktop_entries: desktop_entries::Sources::system(),
      pam: Arc::new(pam::LibPam),
      polkit_helper: PathBuf::from(polkit::HELPER),
      polkit_socket: PathBuf::from(polkit::HELPER_SOCKET),
    }
  }
}
