//! Media players over MPRIS: every program on the session bus that owns
//! an `org.mpris.MediaPlayer2.*` name, such as a music player or a
//! browser playing a video.
//!
//! The players are listed with `ListNames` and followed through the
//! bus's `NameOwnerChanged`. Each is read whole when it appears, then
//! kept up to date from its `PropertiesChanged` and `Seeked`. MPRIS never
//! says when the position moves, so it is read again when a player
//! starts, stops, changes speed or changes track, and
//! [`Player::position_at`] works out the rest.
//!
//! Everything runs on the services' executor, on the main thread. One
//! stream reads every message the bus sends this connection, so a quiet
//! bus leaves the shell asleep.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{
  Array, Connection, DynamicType, Message, MessageStream, ObjectPath, OwnedObjectPath,
  OwnedVariant, Variant,
};

use crate::Shared;
use crate::dbus::{self, BUS, BUS_PATH, CALL_TIMEOUT, PROPERTIES, Props, set_changed};

/// What every player's bus name starts with.
const PREFIX: &str = "org.mpris.MediaPlayer2.";
/// playerctld mirrors whichever player was active last, so showing it
/// would show that player twice.
const PLAYERCTLD: &str = "org.mpris.MediaPlayer2.playerctld";
const PATH: &str = "/org/mpris/MediaPlayer2";
/// The interface with the player's name and `Raise`.
const ROOT: &str = "org.mpris.MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

const MAX_PLAYERS: usize = 32;
const MAX_ARTISTS: usize = 32;
/// The longest text kept, in bytes.
const MAX_TEXT: usize = 4096;

/// Whether a player plays, from MPRIS's `PlaybackStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
  /// Playing.
  Playing,
  /// Paused, to go on from where it is.
  Paused,
  /// Stopped, or a status this list doesn't know.
  Stopped,
}

/// What a player does at the end, from MPRIS's `LoopStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopStatus {
  /// Stops at the end of the playlist.
  None,
  /// Plays the track again.
  Track,
  /// Plays the playlist again.
  Playlist,
}

impl LoopStatus {
  fn parse(name: &str) -> Option<LoopStatus> {
    match name {
      "None" => Some(LoopStatus::None),
      "Track" => Some(LoopStatus::Track),
      "Playlist" => Some(LoopStatus::Playlist),
      _ => None,
    }
  }

  fn name(self) -> &'static str {
    match self {
      LoopStatus::None => "None",
      LoopStatus::Track => "Track",
      LoopStatus::Playlist => "Playlist",
    }
  }
}

/// A media player and its track, as MPRIS describes them. Text is cut to
/// 4 KiB.
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
  /// Its bus name, such as `org.mpris.MediaPlayer2.vlc`, unique while it
  /// runs. The key to give `children_keyed`, and what
  /// [`Mpris::play_pause`] and the rest take.
  pub id: String,
  /// Its name for people, such as `VLC media player`.
  pub identity: String,
  /// Its desktop file's name without `.desktop`, such as `vlc`, or `""`.
  pub desktop_entry: String,
  /// Whether it plays.
  pub status: PlaybackStatus,
  /// The track's title, or `""`.
  pub title: String,
  /// The track's artists, at most 32.
  pub artists: Vec<String>,
  /// The track's album, or `""`.
  pub album: String,
  /// Where the track's cover art is, such as a `file://` URL, or `""`.
  pub art_url: String,
  /// How long the track is, if the player says.
  pub length: Option<Duration>,
  /// Whether [`Mpris::play`] would do anything.
  pub can_play: bool,
  /// Whether [`Mpris::pause`] would do anything.
  pub can_pause: bool,
  /// Whether [`Mpris::next`] would do anything.
  pub can_go_next: bool,
  /// Whether [`Mpris::previous`] would do anything.
  pub can_go_previous: bool,
  /// Whether [`Mpris::seek`] and [`Mpris::set_position`] would do
  /// anything.
  pub can_seek: bool,
  /// Whether it can be controlled at all.
  pub can_control: bool,
  /// Its volume, 1.0 for full, if it has one.
  pub volume: Option<f64>,
  /// Whether it shuffles, if it can.
  pub shuffle: Option<bool>,
  /// What it does at the end, if it can loop.
  pub loop_status: Option<LoopStatus>,
  /// How fast it plays, 1.0 for normal speed.
  pub rate: f64,
  /// Where playback was at [`position_known_at`](Self::position_known_at).
  pub position: Duration,
  /// When [`position`](Self::position) was read.
  pub position_known_at: Instant,
}

impl Player {
  /// Where playback is at `now`: [`position`](Self::position), moved on
  /// at [`rate`](Self::rate) since it was read while the player plays,
  /// and never past the track's length. A bar that shows it ticking
  /// calls this from a clock of its own.
  pub fn position_at(&self, now: Instant) -> Duration {
    let mut position = self.position;
    if self.status == PlaybackStatus::Playing {
      let elapsed = now.saturating_duration_since(self.position_known_at);
      let moved = position.as_secs_f64() + elapsed.as_secs_f64() * self.rate;
      position = Duration::try_from_secs_f64(moved.max(0.0)).unwrap_or(position);
    }
    self.length.map_or(position, |length| position.min(length))
  }

  fn from_props(
    id: String,
    (root, props): &(Props, Props),
    position: Duration,
    at: Instant,
  ) -> Player {
    let metadata = metadata(props);
    let flag = |name| dbus::get(props, name).unwrap_or(false);
    Player {
      id,
      identity: text(root, "Identity"),
      desktop_entry: text(root, "DesktopEntry"),
      status: match dbus::get::<&str>(props, "PlaybackStatus") {
        Some("Playing") => PlaybackStatus::Playing,
        Some("Paused") => PlaybackStatus::Paused,
        _ => PlaybackStatus::Stopped,
      },
      title: text(&metadata, "xesam:title"),
      artists: dbus::get::<&Array>(&metadata, "xesam:artist").map_or(Vec::new(), |artists| {
        let names = artists
          .inner()
          .iter()
          .filter_map(|name| <&str>::try_from(name).ok());
        names.take(MAX_ARTISTS).map(cut).collect()
      }),
      album: text(&metadata, "xesam:album"),
      // A cut URL would point somewhere else.
      art_url: dbus::get::<&str>(&metadata, "mpris:artUrl")
        .filter(|url| url.len() <= MAX_TEXT)
        .unwrap_or_default()
        .to_owned(),
      // Some players say 0 for a stream.
      length: metadata
        .get("mpris:length")
        .and_then(|length| micros(length))
        .filter(|length| !length.is_zero()),
      can_play: flag("CanPlay"),
      can_pause: flag("CanPause"),
      can_go_next: flag("CanGoNext"),
      can_go_previous: flag("CanGoPrevious"),
      can_seek: flag("CanSeek"),
      can_control: flag("CanControl"),
      volume: dbus::get::<f64>(props, "Volume").filter(|volume| volume.is_finite()),
      shuffle: dbus::get(props, "Shuffle"),
      loop_status: dbus::get::<&str>(props, "LoopStatus").and_then(LoopStatus::parse),
      rate: dbus::get::<f64>(props, "Rate")
        .filter(|rate| rate.is_finite())
        .unwrap_or(1.0),
      position,
      position_known_at: at,
    }
  }
}

/// The track's `Metadata`, an `a{sv}`, or nothing when it is of another
/// type.
fn metadata(props: &Props) -> Props {
  let metadata = props
    .get("Metadata")
    .and_then(|value| value.try_clone().ok());
  metadata
    .and_then(|value| Props::try_from(value).ok())
    .unwrap_or_default()
}

/// The text `name`, cut to [`MAX_TEXT`], or `""`.
fn text(props: &Props, name: &str) -> String {
  cut(dbus::get::<&str>(props, name).unwrap_or_default())
}

/// `text` cut to [`MAX_TEXT`] bytes, at a char boundary.
fn cut(text: &str) -> String {
  text[..text.floor_char_boundary(MAX_TEXT)].to_owned()
}

/// A time in microseconds, as MPRIS gives them, of any integer type.
/// `None` for a negative one or another type.
fn micros(value: &Variant<'_>) -> Option<Duration> {
  let micros = match *value {
    Variant::I64(n) => u64::try_from(n).ok()?,
    Variant::U64(n) => n,
    Variant::I32(n) => u64::try_from(n).ok()?,
    Variant::U32(n) => n.into(),
    Variant::I16(n) => u64::try_from(n).ok()?,
    Variant::U16(n) => n.into(),
    Variant::U8(n) => n.into(),
    _ => return None,
  };
  Some(Duration::from_micros(micros))
}

/// The media players, from `cx.mpris()`. A clone is cheap and shares the
/// one connection.
#[derive(Clone)]
pub struct Mpris {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  players: ReadSignal<Vec<Player>>,
}

impl Mpris {
  /// Every player, in the order they appeared on the bus. Empty while
  /// there is no session bus. A player shows once it is read.
  pub fn players(&self) -> ReadSignal<Vec<Player>> {
    self.players
  }

  /// Tells player `id` to play if it is paused, and to pause if it
  /// plays. A refusal or an unknown `id` is logged, as for every method
  /// here.
  pub fn play_pause(&self, id: &str) {
    self.call(id, PLAYER, "PlayPause", ());
  }

  /// Tells player `id` to play.
  pub fn play(&self, id: &str) {
    self.call(id, PLAYER, "Play", ());
  }

  /// Tells player `id` to pause.
  pub fn pause(&self, id: &str) {
    self.call(id, PLAYER, "Pause", ());
  }

  /// Tells player `id` to stop.
  pub fn stop(&self, id: &str) {
    self.call(id, PLAYER, "Stop", ());
  }

  /// Tells player `id` to go to the next track.
  pub fn next(&self, id: &str) {
    self.call(id, PLAYER, "Next", ());
  }

  /// Tells player `id` to go to the previous track.
  pub fn previous(&self, id: &str) {
    self.call(id, PLAYER, "Previous", ());
  }

  /// Moves player `id` `seconds` on from where it is, or back for a
  /// negative number. One that isn't finite isn't sent.
  pub fn seek(&self, id: &str, seconds: f64) {
    if !seconds.is_finite() {
      return tracing::warn!(id, seconds, "a seek by no finite time; not sent");
    }
    let offset = (seconds * 1e6).round() as i64;
    self.call(id, PLAYER, "Seek", offset);
  }

  /// Moves player `id` to `position` in its track. A player that names
  /// no track can't be moved this way, which is logged.
  pub fn set_position(&self, id: &str, position: Duration) {
    let track = self.service.as_ref().and_then(|service| service.track(id));
    let Some(track) = track else {
      return tracing::warn!(id, "the media player names no track; no position set");
    };
    let at = i64::try_from(position.as_micros()).unwrap_or(i64::MAX);
    self.call(id, PLAYER, "SetPosition", (track, at));
  }

  /// Sets player `id`'s volume, 1.0 for full. One that isn't finite
  /// isn't sent.
  pub fn set_volume(&self, id: &str, volume: f64) {
    if !volume.is_finite() {
      return tracing::warn!(id, volume, "a volume that isn't finite; not sent");
    }
    self.set(id, "Volume", Variant::from(volume));
  }

  /// Turns player `id`'s shuffle on or off.
  pub fn set_shuffle(&self, id: &str, shuffle: bool) {
    self.set(id, "Shuffle", Variant::from(shuffle));
  }

  /// Sets what player `id` does at the end.
  pub fn set_loop(&self, id: &str, status: LoopStatus) {
    self.set(id, "LoopStatus", Variant::from(status.name()));
  }

  /// Asks player `id` to show its window.
  pub fn raise(&self, id: &str) {
    self.call(id, ROOT, "Raise", ());
  }

  /// The service and its connection, if player `id` is on the bus. An
  /// unknown `id` is logged.
  fn target(&self, id: &str, method: &str) -> Option<(Rc<Shared>, Connection)> {
    let service = self.service.as_ref()?;
    let state = service.state.borrow();
    let target = state
      .as_ref()
      .filter(|state| state.entries.iter().any(|entry| entry.id == id))
      .and_then(|state| state.connection.clone());
    if target.is_none() {
      tracing::warn!(id, method, "no such media player");
    }
    Some((Rc::clone(&service.shared), target?))
  }

  /// Calls `method` on player `id`, without waiting for the answer.
  fn call<B>(&self, id: &str, interface: &'static str, method: &'static str, body: B)
  where
    B: serde::Serialize + DynamicType + 'static,
  {
    let Some((shared, connection)) = self.target(id, method) else {
      return;
    };
    let id = id.to_owned();
    let spawner = Rc::clone(&shared);
    spawner.spawn(async move {
      let call = connection.call_method(Some(id.as_str()), PATH, Some(interface), method, &body);
      if let Err(error) = shared.timeout(CALL_TIMEOUT, call).await {
        tracing::info!(id, method, %error, "the media player refused");
      }
    });
  }

  /// Sets player `id`'s property `name`, without waiting for the answer.
  fn set(&self, id: &str, name: &'static str, value: Variant<'static>) {
    let Some((shared, connection)) = self.target(id, name) else {
      return;
    };
    let id = id.to_owned();
    let spawner = Rc::clone(&shared);
    spawner.spawn(async move {
      let set = dbus::set(&shared, &connection, &id, (PATH, PLAYER), name, value);
      if let Err(error) = set.await {
        tracing::info!(id, name, %error, "the media player refused");
      }
    });
  }
}

/// `cx.mpris()`.
pub(crate) fn mpris(cx: &Cx) -> Mpris {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.mpris() needs the services the shell provides; it stays empty");
    });
    return Mpris {
      service: None,
      players: cx.signal(Vec::new()).read_only(),
    };
  };
  let players = service.start();
  Mpris {
    service: Some(service),
    players,
  }
}

/// The media players of one shell, followed from the first ask.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  players: Signal<Vec<Player>>,
  /// The session bus, once joined.
  connection: Option<Connection>,
  /// Every player on the bus, in the order they appeared, read or not.
  entries: Vec<Entry>,
}

struct Entry {
  /// Its bus name.
  id: String,
  /// The unique name that owns it, which its signals come from. `None`
  /// until the bus or a read says.
  owner: Option<String>,
  /// Counts reads, so a slow reply never overwrites a newer one.
  reads: u64,
  /// Its `org.mpris.MediaPlayer2` and `Player` properties. `None` until
  /// a read works.
  props: Option<(Props, Props)>,
  /// The position as last read, and when.
  position: (Duration, Instant),
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The list, joining the bus if this is the first ask.
  fn start(self: &Rc<Self>) -> ReadSignal<Vec<Player>> {
    if let Some(state) = &*self.state.borrow() {
      return state.players.read_only();
    }
    let players = self.shared.root.signal(Vec::new());
    *self.state.borrow_mut() = Some(State {
      players,
      connection: None,
      entries: Vec::new(),
    });
    match self.shared.config.session_bus.clone() {
      Some(address) => self.shared.spawn(run(Rc::clone(self), address)),
      None => tracing::info!("no session bus, so there are no media players"),
    }
    players.read_only()
  }

  /// Whether the list is still there: its owner, the root, isn't disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.players.try_with(|_| ()));
    alive.is_some()
  }

  fn with_entry<R>(&self, id: &str, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
    let mut state = self.state.borrow_mut();
    let entry = state
      .as_mut()?
      .entries
      .iter_mut()
      .find(|entry| entry.id == id)?;
    Some(f(entry))
  }

  /// The track player `id` plays, for `SetPosition`.
  fn track(&self, id: &str) -> Option<OwnedObjectPath> {
    self
      .with_entry(id, |entry| {
        let (_, props) = entry.props.as_ref()?;
        let track = dbus::get::<&ObjectPath>(&metadata(props), "mpris:trackid")?.to_owned();
        Some(OwnedObjectPath::from(track))
      })
      .flatten()
  }

  /// Subscribes to the players' names and signals, then reads every
  /// player already on the bus.
  async fn join(self: &Rc<Self>, connection: &Connection) -> zbus::Result<()> {
    let names = format!(
      "type='signal',sender='{BUS}',interface='{BUS}',member='NameOwnerChanged',\
       arg0namespace='{ROOT}'"
    );
    // From any sender: `on_signal` keeps the players' own.
    let signals = format!("type='signal',path='{PATH}'");
    for rule in [names, signals] {
      dbus::bus_call(connection, "AddMatch", &rule).await?;
    }
    let call = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), "ListNames", &());
    let names: Vec<String> = call.await?.body().deserialize()?;
    // Each read asks for its owner. Awaiting them here would leave the
    // bus's messages unread, and zbus stops reading at 64.
    for name in names.into_iter().filter(|name| is_player(name)) {
      self.appeared(connection, name, None);
    }
    Ok(())
  }

  /// Player `id` is on the bus, newly or under a new `owner`.
  fn appeared(self: &Rc<Self>, connection: &Connection, id: String, owner: Option<String>) {
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      let full = state.entries.len() >= MAX_PLAYERS;
      match state.entries.iter_mut().find(|entry| entry.id == id) {
        Some(entry) if entry.owner == owner => return,
        Some(entry) => entry.owner = owner,
        None if full => {
          return tracing::warn!(id, "too many media players; leaving one out");
        }
        None => state.entries.push(Entry {
          id: id.clone(),
          owner,
          reads: 0,
          props: None,
          position: (Duration::ZERO, Instant::now()),
        }),
      }
    }
    self.read(connection, id);
  }

  /// Reads player `id` whole and shows it. A failed read is logged, and
  /// the player keeps what it showed.
  fn read(self: &Rc<Self>, connection: &Connection, id: String) {
    let Some(read) = self.with_entry(&id, |entry| {
      entry.reads += 1;
      entry.reads
    }) else {
      return;
    };
    let (service, connection) = (Rc::clone(self), connection.clone());
    self.shared.spawn(async move {
      let shared = &service.shared;
      let all = async {
        // A new owner since then reads again, so this read is dropped.
        let owner = dbus::owner(&connection, &id).await;
        let root = dbus::get_all(shared, &connection, &id, PATH, ROOT).await?;
        let props = dbus::get_all(shared, &connection, &id, PATH, PLAYER).await?;
        zbus::Result::Ok((owner, (root, props)))
      };
      let (owner, props) = match all.await {
        Ok(read) => read,
        Err(error) => return tracing::warn!(id, %error, "can't read a media player"),
      };
      let position = props.1.get("Position").and_then(|at| micros(at));
      let newest = service.with_entry(&id, |entry| {
        if entry.reads == read {
          entry.owner = owner;
          entry.position = (position.unwrap_or_default(), Instant::now());
          entry.props = Some(props);
        }
      });
      if newest.is_some() {
        service.publish();
      }
    });
  }

  /// Reads player `id`'s position, which MPRIS doesn't announce.
  fn read_position(self: &Rc<Self>, connection: &Connection, id: String) {
    let Some(read) = self.with_entry(&id, |entry| entry.reads) else {
      return;
    };
    let (service, connection) = (Rc::clone(self), connection.clone());
    self.shared.spawn(async move {
      let get = (PLAYER, "Position");
      let call = connection.call_method(Some(id.as_str()), PATH, Some(PROPERTIES), "Get", &get);
      let reply = service.shared.timeout(CALL_TIMEOUT, call).await;
      let position = match reply.and_then(|reply| reply.body().deserialize::<OwnedVariant>()) {
        Ok(position) => micros(&position).unwrap_or_default(),
        Err(error) => return tracing::info!(id, %error, "can't read a media player's position"),
      };
      // A whole read since brings its own.
      let newest = service.with_entry(&id, |entry| {
        if entry.reads == read {
          entry.position = (position, Instant::now());
        }
      });
      if newest.is_some() {
        service.publish();
      }
    });
  }

  fn on_signal(self: &Rc<Self>, connection: &Connection, message: &Message) {
    if let Some((name, owner)) = dbus::owner_changed(message) {
      match (is_player(&name), owner.is_empty()) {
        (false, _) => {}
        (true, true) => self.left(&name),
        (true, false) => self.appeared(connection, name, Some(owner)),
      }
      return;
    }
    let header = message.header();
    // Broadcasts from a player's current owner only: any program can
    // send one from that path, or aim one at this connection.
    let (Some(sender), None) = (header.sender(), header.destination()) else {
      return;
    };
    if header.path().map(|path| path.as_str()) != Some(PATH) {
      return;
    }
    let ids: Vec<String> = {
      let state = self.state.borrow();
      let entries = state.as_ref().map(|state| state.entries.as_slice());
      let from = entries
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.owner.as_deref() == Some(sender.as_str()));
      from.map(|entry| entry.id.clone()).collect()
    };
    if let Some((_, interface, changed, dropped)) = dbus::properties_changed(message) {
      let moved = interface == PLAYER
        && ["PlaybackStatus", "Rate", "Metadata"]
          .iter()
          .any(|name| changed.contains_key(*name));
      for id in ids {
        self.with_entry(&id, |entry| {
          let Some((root, props)) = &mut entry.props else {
            return;
          };
          let props = match interface.as_str() {
            ROOT => root,
            PLAYER => props,
            _ => return,
          };
          dbus::apply(props, changed.clone(), dropped.clone());
        });
        // A player that only says a property changed is read whole.
        if !dropped.is_empty() {
          self.read(connection, id);
        } else if moved {
          self.read_position(connection, id);
        }
      }
      return self.publish();
    }
    let member = header.member().map(|member| member.as_str());
    let seeked = header.interface().map(|interface| interface.as_str()) == Some(PLAYER)
      && member == Some("Seeked");
    if let (true, Ok(at)) = (seeked, message.body().deserialize::<i64>()) {
      let position = micros(&Variant::I64(at)).unwrap_or_default();
      for id in ids {
        self.with_entry(&id, |entry| entry.position = (position, Instant::now()));
      }
      self.publish();
    }
  }

  /// Player `id` left the bus.
  fn left(&self, id: &str) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.entries.retain(|entry| entry.id != id);
    }
    self.publish();
  }

  /// Sets the list from the players read so far, if it changed.
  fn publish(&self) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let players = state.entries.iter().filter_map(|entry| {
        let props = entry.props.as_ref()?;
        let (position, at) = entry.position;
        Some(Player::from_props(entry.id.clone(), props, position, at))
      });
      (state.players, players.collect::<Vec<_>>())
    });
    if let Some((signal, players)) = shown {
      set_changed(signal, players);
    }
  }

  /// Empties the list, once the bus is gone.
  fn clear(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.entries.clear();
      state.connection = None;
    }
    self.publish();
  }
}

/// Whether bus name `name` is a player to show.
fn is_player(name: &str) -> bool {
  name.starts_with(PREFIX) && name != PLAYERCTLD
}

/// Joins the session bus at `address` and follows the players, until the
/// bus closes or the list is disposed.
async fn run(service: Rc<Service>, address: zbus::Address) {
  let connection = match zbus::connection::Builder::address(address).build().await {
    Ok(connection) => connection,
    Err(error) => {
      return tracing::info!(%error, "can't reach the session bus, so there are no media players");
    }
  };
  // Made before anything is asked, so no signal is missed.
  let mut messages = MessageStream::from(&connection);
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.connection = Some(connection.clone());
  }
  if let Err(error) = service.join(&connection).await {
    return tracing::warn!(%error, "couldn't follow the media players on the session bus");
  }
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !service.alive() {
      return service.clear();
    }
    match next {
      Some(Ok(message)) if message.header().message_type() == Kind::Signal => {
        service.on_signal(&connection, &message);
      }
      Some(Ok(_)) => {}
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the session bus"),
      None => break,
    }
  }
  tracing::warn!("the session bus closed, so there are no media players");
  service.clear();
}
