//! The services a Crabshell bar reads: the clock, Sway's and Hyprland's
//! workspaces, audio, the tray, power, notifications, the installed
//! applications, PAM, greetd, a polkit agent and HTTP GET.
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

mod a11y;
mod activation;
mod audio;
mod bluetooth;
mod brightness;
mod clock;
mod dbus;
mod desktop_entries;
mod file;
mod folder;
mod greetd;
mod http;
mod hyprland;
mod lines;
mod locale;
mod logind;
mod mpris;
mod network;
mod niri;
mod notifications;
mod pam;
mod pipewire;
mod polkit;
mod power;
mod process;
mod socket;
mod socket_server;
mod spawn;
mod stats;
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
use crabshell_ui::{Cx, IntoProp};

#[doc(hidden)]
pub use a11y::follow_accessibility;
pub use audio::{Audio, AudioChannel, AudioNode, AudioStream, PeakTarget, Sink};
pub use bluetooth::{
  Bluetooth, BluetoothAdapter, BluetoothAdapterState, BluetoothAgent, BluetoothDevice,
  BluetoothDeviceState, PairingKind, PairingRequest,
};
pub use brightness::{Backlight, Brightness};
pub use clock::Precision;
pub use desktop_entries::{DesktopAction, DesktopEntries, DesktopEntry};
pub use file::{FileError, FileErrorKind, FileView};
pub use folder::{FolderEntry, FolderView};
pub use greetd::{GreetMessage, GreetState, Greetd};
pub use http::{Http, HttpError, HttpErrorKind};
pub use hyprland::{Hyprland, HyprlandEvent, HyprlandMonitor, HyprlandToplevel, HyprlandWorkspace};
pub use locale::Locale;
pub use logind::Logind;
pub use mpris::{LoopStatus, Mpris, PlaybackStatus, Player};
pub use network::Scanner;
pub use network::{
  ConnectionFailReason, Connectivity, Network, NetworkDevice, NetworkKind, NetworkState,
  SavedConnection, WifiMode, WifiNetwork, WifiSecurity,
};
pub use niri::{Niri, NiriKeyboardLayouts, NiriWindow, NiriWorkspace};
pub use notifications::{CloseReason, Notification, Notifications, Timeout, Urgency};
pub use pam::{Pam, PamError, PamMessage, PamResult, PamState};
pub use pipewire::{PipeWire, PwLink, PwLinkGroup, PwLinkState, PwNode};
pub use polkit::{AuthRequest, AuthState, PolkitAgent};
pub use power::{
  BatteryState, DeviceKind, PerformanceDegradation, PowerDevice, PowerProfile, PowerProfiles,
  UPower,
};
pub use process::{IntoCommand, Process};
pub use socket::Socket;
pub use socket_server::SocketServer;
#[doc(hidden)]
pub use spawn::{FIRST_START, shell_tunables};
pub use stats::{SystemStats, Temperature};
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
  cx.provide(niri::Service::new(Rc::clone(&shared)));
  cx.provide(pipewire::Service::new(Rc::clone(&shared)));
  cx.provide(audio::AudioService::new(Rc::clone(&shared)));
  cx.provide(tray::Service::new(Rc::clone(&shared)));
  cx.provide(power::UPowerService::new(Rc::clone(&shared)));
  cx.provide(power::ProfilesService::new(Rc::clone(&shared)));
  let agent = cx.provide(bluetooth::agent::Service::new(Rc::clone(&shared)));
  cx.provide(bluetooth::BluetoothService::new(Rc::clone(&shared), agent));
  cx.provide(notifications::Service::new(Rc::clone(&shared)));
  cx.provide(mpris::Service::new(Rc::clone(&shared)));
  cx.provide(network::NetworkService::new(Rc::clone(&shared)));
  cx.provide(socket::Sockets(Rc::clone(&shared)));
  cx.provide(process::Processes::new(Rc::clone(&shared)));
  cx.provide(desktop_entries::Service::new(Rc::clone(&shared)));
  cx.provide(file::Files(Rc::clone(&shared)));
  cx.provide(http::Service::new(Rc::clone(&shared)));
  cx.provide(stats::Service::new(Rc::clone(&shared)));
  cx.provide(greetd::Service::new(Rc::clone(&shared)));
  cx.provide(pam::Service(Rc::clone(&shared)));
  cx.provide(polkit::Service::new(Rc::clone(&shared)));
  let logind = cx.provide(logind::Service::new(Rc::clone(&shared)));
  cx.provide(brightness::Service::new(Rc::clone(&shared), logind));
  cx.provide(a11y::Service::new(Rc::clone(&shared)));
}

/// Lets the services ask the compositor for activation tokens for the
/// programs they start, so it may focus them: `request` asks for one for
/// a program with the app id given, if any, and hands it to the callback.
/// The shell calls this once, when the compositor offers tokens; without
/// it programs start at once with none.
#[doc(hidden)]
pub fn provide_activation_tokens(
  cx: &Cx,
  request: impl Fn(Option<&str>, Box<dyn FnOnce(String)>) + 'static,
) {
  if let Some(processes) = cx.service::<process::Processes>() {
    let _ = processes.shared.activation.set(Box::new(request));
  }
}

/// Stops every program a config started with `cx.process` and every curl
/// `cx.http_get` and `cx.cover_art` run, and starts every program still
/// waiting for its activation token, without it. The
/// shell calls this before it exits, since `std::process::exit` skips the
/// drops that would stop them, each program has a process group of its
/// own, and no token or timer comes after the exit.
#[doc(hidden)]
pub fn stop_programs(cx: &Cx) {
  if let Some(processes) = cx.service::<process::Processes>() {
    activation::start_waiting(&processes.shared);
  }
  process::stop_all(cx);
  http::stop_all(cx);
}

/// What the services keep across a reload, for the shell to save: the
/// notifications kept. `None` when no service has anything to keep.
#[doc(hidden)]
pub fn save_state(cx: &Cx) -> Option<serde_json::Value> {
  let notifications = cx.service::<notifications::Service>()?.save()?;
  Some(serde_json::json!({ "notifications": notifications }))
}

/// Hands the services what [`save_state`] gave the shell before a reload.
/// The shell calls it before the root closure runs.
#[doc(hidden)]
pub fn restore_state(cx: &Cx, state: serde_json::Value) {
  let Some(service) = cx.service::<notifications::Service>() else {
    return;
  };
  if let serde_json::Value::Object(mut state) = state
    && let Some(notifications) = state.remove("notifications")
  {
    service.restore(notifications);
  }
}

/// The name of user `uid` through NSS, so a user from LDAP or
/// systemd-homed counts as one in `/etc/passwd` does. `None` for no such
/// user; a lookup that fails is logged.
pub(crate) fn user_name(uid: u32) -> Option<String> {
  match nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid)) {
    Ok(user) => user.map(|user| user.name).filter(|name| !name.is_empty()),
    Err(error) => {
      tracing::warn!(%error, uid, "couldn't look the user up");
      None
    }
  }
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

  /// The month and day names of the user's locale for dates: `LC_ALL`,
  /// else `LC_TIME`, else `LANG`. English until glibc's names for it are
  /// read, once per shell, by running `locale -k LC_TIME` on a thread;
  /// that wakes the shell once. C, POSIX, unset, or a locale that isn't
  /// installed stays English.
  fn locale(&self) -> ReadSignal<Locale>;

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

  /// niri's workspaces, windows, keyboard layouts and overview, from its
  /// event stream on the IPC socket at `$NIRI_SOCKET`. Every caller
  /// shares one connection, made on the first call.
  ///
  /// The lists are empty while niri isn't there: when `NIRI_SOCKET` is
  /// unset or the socket is missing, they stay empty, which is logged
  /// once, and the service never wakes the shell. When niri closes the
  /// stream while its socket is still there, the shell connects once
  /// again; otherwise the lists empty and nothing reconnects. While
  /// connected, it wakes the shell only when niri sends something.
  fn niri(&self) -> Niri;

  /// PipeWire's nodes and the links between them, over its native
  /// protocol on `pipewire-0` in `$PIPEWIRE_RUNTIME_DIR`, else
  /// `$XDG_RUNTIME_DIR`. Every caller shares one connection, made on the
  /// first call, so a shell that never asks pays nothing.
  ///
  /// The lists are empty while PipeWire isn't there, which is logged
  /// once, and the service never wakes the shell. When PipeWire closes
  /// the connection while its socket is still there, the shell connects
  /// once again; otherwise the lists empty and nothing reconnects. While
  /// connected, it wakes the shell only when the graph changes.
  fn pipewire(&self) -> PipeWire;

  /// The audio server's default sink, with its volume and mute. Every
  /// caller shares one connection, which starts on the loop's next turn
  /// and never waits for the server. It wakes the shell only when the
  /// server reports a change, and while a peak meter from
  /// [`Audio::peaks`] records.
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

  /// The shell's Bluetooth pairing agent: when BlueZ needs a PIN, a
  /// passkey or a yes to pair a device, it asks the shell, and a config
  /// shows the prompt. The first call registers the agent with BlueZ on
  /// the system bus, as `KeyboardDisplay`, again whenever BlueZ starts
  /// again; a shell that never calls it never answers BlueZ. Every caller
  /// shares the one agent, which wakes the shell only when BlueZ calls.
  fn bluetooth_agent(&self) -> BluetoothAgent;

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

  /// A server on the Unix socket at `path`, as Quickshell's
  /// `SocketServer`: it listens at once, and each client that connects
  /// becomes a [`Socket`] for [`SocketServer::on_connection`]. Each call
  /// makes its own, which stops when this context goes, closing its
  /// connections and removing its socket file. It wakes the shell only
  /// when a client connects, sends something or takes what was written.
  fn socket_server(&self, path: impl Into<PathBuf>) -> SocketServer;

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
  /// and it is reaped when it ends. A failure to start is logged. When
  /// the compositor hands out activation tokens, it starts once one comes,
  /// or after 100 ms without one, with the token in
  /// `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`, so the compositor
  /// may focus its window.
  fn exec_detached(&self, command: impl IntoCommand);

  /// The text file at `path`, as Quickshell's `FileView`: read now, and
  /// again when it changes on disk. Each call reads its own, which goes
  /// with this context. It is read on the main thread, so a file on a
  /// slow disk delays the frame.
  fn file_view(&self, path: impl Into<PathBuf>) -> FileView;

  /// The folder at `path`, as Qt's `FolderListModel`: its entries, read
  /// at the next flush and again 50 ms after a change in it, so a burst
  /// of changes reads it once, and followed as `file_view` follows a
  /// file when the folder is made later, removed or replaced.
  /// `path` takes a value or a closure; a new path follows the new
  /// folder, and a panic in the closure is logged and empties the view
  /// until the next change. Each call reads its own, which goes with
  /// this context. It is read on the main thread, and wakes the shell
  /// only when the folder changes.
  fn folder_view(&self, path: impl IntoProp<PathBuf>) -> FolderView;

  /// CPU, memory and temperature stats, sampled at once and then every
  /// `period`, at least 250 ms, from `/proc` and `/sys/class/hwmon`. Each
  /// call samples on its own timer, which goes with this context, so
  /// stats wake the shell only while a context that shows them lives.
  /// Call it once, above `per_output`, and share the signal. The signal
  /// starts empty and fills when the first sample, asked for at once,
  /// comes back from the thread the reads run on, so a slow sensor never
  /// holds up a frame. A tick while a read still runs is skipped. Each
  /// sample wakes the shell twice: at the tick and when the answer comes.
  fn system_stats(&self, period: Duration) -> ReadSignal<SystemStats>;

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

  /// Fetches `url` with an HTTP GET through `curl`, in a process of its
  /// own, so nothing waits on the network. A closure fetches again when a
  /// signal it read changes the URL. It stops when this context goes, and
  /// wakes the shell only while curl runs.
  fn http_get(&self, url: impl IntoProp<String>) -> Http;

  /// The local file for the cover art at `url`, for an `Image`: a
  /// `file://` URL's file, or an `http:` or `https:` URL's picture
  /// downloaded through `curl` into `$XDG_CACHE_HOME/crabshell/art`.
  /// `None` while it downloads, and for anything else or a failure, which
  /// is logged once per URL.
  fn cover_art(&self, url: impl IntoProp<String>) -> ReadSignal<Option<PathBuf>>;

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

  /// logind on the system bus: when the machine is about to sleep, a
  /// delay so a config can get ready first, as a lock screen locks, and
  /// the power actions. Every caller shares one connection, which starts
  /// on the loop's next turn. It behaves as [`upower`](Self::upower)
  /// does while logind isn't there, and wakes the shell only for
  /// `PrepareForSleep`.
  fn logind(&self) -> Logind;

  /// The screen backlights in `/sys/class/backlight`, read on the first
  /// call and followed with inotify, and set through logind, which
  /// starts with it. Every caller shares one list. It wakes the shell
  /// only when the kernel says a brightness changed; a backlight added
  /// later shows after a restart.
  fn brightness(&self) -> Brightness;
}

impl ServicesExt for Cx {
  fn clock(&self, precision: Precision) -> ReadSignal<jiff::Zoned> {
    clock::clock(self, precision)
  }

  fn locale(&self) -> ReadSignal<Locale> {
    locale::locale(self)
  }

  fn sway(&self) -> Sway {
    sway::sway(self)
  }

  fn hyprland(&self) -> Hyprland {
    hyprland::hyprland(self)
  }

  fn niri(&self) -> Niri {
    niri::niri(self)
  }

  fn pipewire(&self) -> PipeWire {
    pipewire::pipewire(self)
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

  fn bluetooth_agent(&self) -> BluetoothAgent {
    bluetooth::agent::bluetooth_agent(self)
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

  fn socket_server(&self, path: impl Into<PathBuf>) -> SocketServer {
    socket_server::socket_server(self, path.into())
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

  #[track_caller]
  fn folder_view(&self, path: impl IntoProp<PathBuf>) -> FolderView {
    folder::folder_view(self, path, std::panic::Location::caller())
  }

  fn system_stats(&self, period: Duration) -> ReadSignal<SystemStats> {
    stats::system_stats(self, period)
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

  #[track_caller]
  fn http_get(&self, url: impl IntoProp<String>) -> Http {
    http::http_get(self, url)
  }

  #[track_caller]
  fn cover_art(&self, url: impl IntoProp<String>) -> ReadSignal<Option<PathBuf>> {
    http::art::cover_art(self, url)
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

  fn logind(&self) -> Logind {
    logind::logind(self)
  }

  fn brightness(&self) -> Brightness {
    brightness::brightness(self)
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
  /// How to ask for an activation token, once the shell gives it.
  activation: OnceCell<activation::Request>,
  /// The programs waiting for their activation token.
  waiting: activation::Waiting,
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
      activation: OnceCell::new(),
      waiting: activation::Waiting::default(),
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

  /// Adds the channel a user lookup's thread sends names on.
  fn insert_users_channel(
    &self,
    channel: Channel<polkit::Found>,
    on_event: Box<dyn FnMut(calloop::channel::Event<polkit::Found>)>,
  ) -> Option<RegistrationToken>;

  /// Calls `on_answer` with each sample the stats' thread sends on
  /// `channel`. `None`, logged, if the loop refused it.
  fn insert_stats_channel(
    &self,
    channel: Channel<stats::Answer>,
    on_answer: Box<dyn FnMut(stats::Answer)>,
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

  fn insert_users_channel(
    &self,
    channel: Channel<polkit::Found>,
    mut on_event: Box<dyn FnMut(calloop::channel::Event<polkit::Found>)>,
  ) -> Option<RegistrationToken> {
    self
      .insert_source(channel, move |event, _, _| on_event(event))
      .inspect_err(|error| tracing::error!(%error, "a user lookup's channel couldn't be added"))
      .ok()
  }

  fn insert_stats_channel(
    &self,
    channel: Channel<stats::Answer>,
    mut on_answer: Box<dyn FnMut(stats::Answer)>,
  ) -> Option<RegistrationToken> {
    self
      .insert_source(channel, move |event, _, _| {
        if let calloop::channel::Event::Msg(answer) = event {
          on_answer(answer);
        }
      })
      .inspect_err(|error| tracing::error!(%error, "the stats' channel couldn't be added"))
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
  /// niri's IPC socket, from `NIRI_SOCKET`. `None` when that is unset.
  niri: Option<PathBuf>,
  /// PipeWire's socket, `pipewire-0` in `PIPEWIRE_RUNTIME_DIR`, else
  /// `XDG_RUNTIME_DIR`. `None` when both are unset.
  pipewire: Option<PathBuf>,
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
  /// Where polkit's setuid helper, which checks answers with PAM, may be;
  /// the first that exists runs. Tests give a script of their own.
  polkit_helpers: Vec<PathBuf>,
  /// polkit's socket for its helper, tried before `polkit_helpers`.
  polkit_socket: PathBuf,
  /// How polkit's identities are named: through NSS, and in tests a
  /// lookup as slow as they like. It runs off the loop's thread.
  polkit_users: Arc<dyn Fn(u32) -> Option<String> + Send + Sync>,
  /// How long a request waits for its identities' names before it goes on
  /// with those found.
  polkit_users_limit: Duration,
  /// The `curl` that `cx.http_get` and `cx.cover_art` run.
  curl: PathBuf,
  /// Where `cx.cover_art` keeps what it downloads. `None` for no cache.
  art_cache: Option<PathBuf>,
  /// Where the backlights are, `/sys/class/backlight`.
  backlight: PathBuf,
  /// Where procfs and sysfs are, `/proc` and `/sys`, for the stats.
  proc: PathBuf,
  sys: PathBuf,
  /// A pause before each stats read, so tests can make sensors slow.
  #[cfg(test)]
  stats_delay: Duration,
  /// Whether the first stats read panics, as a bug would.
  #[cfg(test)]
  stats_panic: bool,
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
      niri: std::env::var_os("NIRI_SOCKET")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from),
      pipewire: ["PIPEWIRE_RUNTIME_DIR", "XDG_RUNTIME_DIR"]
        .into_iter()
        .find_map(|name| std::env::var_os(name).filter(|dir| !dir.is_empty()))
        .map(|dir| PathBuf::from(dir).join("pipewire-0")),
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
      polkit_helpers: polkit::HELPERS.map(PathBuf::from).to_vec(),
      polkit_socket: PathBuf::from(polkit::HELPER_SOCKET),
      polkit_users: Arc::new(user_name),
      polkit_users_limit: polkit::USERS_LIMIT,
      curl: PathBuf::from("curl"),
      art_cache: http::art::cache_dir(),
      backlight: PathBuf::from("/sys/class/backlight"),
      proc: PathBuf::from("/proc"),
      sys: PathBuf::from("/sys"),
      #[cfg(test)]
      stats_delay: Duration::ZERO,
      #[cfg(test)]
      stats_panic: false,
    }
  }
}
