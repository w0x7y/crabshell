//! The services a Crabshell bar reads: the clock, Sway's workspaces, audio,
//! the tray, power and notifications.
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
mod lines;
mod mpris;
mod network;
mod notifications;
mod power;
mod socket;
mod sway;
#[cfg(test)]
mod testing;
mod tray;

use std::cell::{Cell, OnceCell, RefCell};
use std::future::{Future, poll_fn};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use calloop::channel::Channel;
use calloop::futures::{Executor, Scheduler};
use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal};
use crabshell_ui::Cx;

pub use audio::{Audio, AudioNode, AudioStream, Sink};
pub use bluetooth::{Bluetooth, BluetoothAdapter, BluetoothDevice};
pub use clock::Precision;
pub use mpris::{LoopStatus, Mpris, PlaybackStatus, Player};
pub use network::{Network, NetworkDevice, NetworkKind, NetworkState, WifiNetwork};
pub use notifications::{Notification, Notifications, Timeout, Urgency};
pub use power::{BatteryState, DeviceKind, PowerDevice, PowerProfile, PowerProfiles, UPower};
pub use socket::Socket;
pub use sway::{Sway, Workspace};
pub use tray::{MenuEntry, MenuToggle, Tray, TrayHost, TrayItem, TrayMenu, TrayStatus};

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
  cx.provide(audio::AudioService::new(Rc::clone(&shared)));
  cx.provide(tray::Service::new(Rc::clone(&shared)));
  cx.provide(power::UPowerService::new(Rc::clone(&shared)));
  cx.provide(power::ProfilesService::new(Rc::clone(&shared)));
  cx.provide(bluetooth::BluetoothService::new(Rc::clone(&shared)));
  cx.provide(notifications::Service::new(Rc::clone(&shared)));
  cx.provide(mpris::Service::new(Rc::clone(&shared)));
  cx.provide(network::NetworkService::new(Rc::clone(&shared)));
  cx.provide(socket::Sockets(Rc::clone(&shared)));
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

  /// Sway's workspaces, over the IPC socket at `$SWAYSOCK`. Every caller
  /// shares one connection, made on the first call.
  ///
  /// The list is empty while Sway isn't there. When `SWAYSOCK` is unset
  /// or the first connect fails, it stays empty and the service never
  /// wakes the shell. When a working connection closes, the list is
  /// empty until a reconnect, tried every 2 s, works. While connected,
  /// it wakes the shell only when Sway sends something.
  fn sway(&self) -> Sway;

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
}

impl ServicesExt for Cx {
  fn clock(&self, precision: Precision) -> ReadSignal<jiff::Zoned> {
    clock::clock(self, precision)
  }

  fn sway(&self) -> Sway {
    sway::sway(self)
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
}

/// Where the services find the system. Tests point it at their own files
/// and a private bus.
struct Config {
  /// The link that names the system time zone, `/etc/localtime`. `None`
  /// when `TZ` is set, since it names the zone then, as it does for libc.
  localtime: Option<PathBuf>,
  /// The system bus, where logind, timedated, UPower,
  /// power-profiles-daemon, BlueZ and NetworkManager are. `None` for none.
  system_bus: Option<zbus::Address>,
  /// The session bus, where tray items, notifying apps and media
  /// players are. `None` for none.
  session_bus: Option<zbus::Address>,
  /// Sway's IPC socket, from `SWAYSOCK`. `None` when that is unset.
  sway: Option<sway::Socket>,
  /// The audio server. `None` for none, so a test never reaches the
  /// user's.
  audio: Option<audio::Server>,
  /// How long a popup shows when its app leaves that to the server: 5 s,
  /// and less in tests.
  popup_timeout: Duration,
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
      audio: Some(audio::Server {
        address: None,
        retry: audio::RETRY,
        retry_for: audio::RETRY_FOR,
      }),
      popup_timeout: notifications::POPUP_TIMEOUT,
    }
  }
}
