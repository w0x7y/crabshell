//! The system tray: every app's StatusNotifierItem on the session bus.
//!
//! An app's tray icon is an item, an object on the session bus with the
//! `org.kde.StatusNotifierItem` interface. A watcher, the owner of
//! `org.kde.StatusNotifierWatcher`, keeps the list of items, and hosts
//! such as this bar show them. The shell queues for the watcher's name,
//! so it runs the watcher, or takes it over when the program that runs
//! it leaves.
//!
//! Everything runs on the services' executor, on the main thread. One
//! stream reads every message the bus sends this connection, so a quiet
//! bus leaves the shell asleep.

mod watcher;

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::{Cx, IconPixels};
use zbus::export::futures_core::Stream;
use zbus::fdo::RequestNameFlags;
use zbus::message::Type as Kind;
use zbus::names::BusName;
use zbus::{Connection, DeserializeDict, Message, MessageStream, ObjectPath, Type};

use crate::Shared;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// The largest pixmap side kept, the same as the renderer's cap.
const MAX_PIXMAP_SIDE: i32 = 512;

/// The most pixmap bytes kept per item: one 512 px pixmap, or a usual
/// set of sizes from 16 to 256 px.
const MAX_PIXMAP_BYTES: usize = 512 * 512 * 4;

/// How long a call to an app may take, libdbus's default.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Whether a tray item wants to be seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrayStatus {
  /// Nothing to report now, so a bar may hide it.
  Passive,
  /// The usual state.
  #[default]
  Active,
  /// It asks for the user's attention, and shows its attention icon if
  /// it has one.
  NeedsAttention,
}

/// One app's tray icon, as its StatusNotifierItem describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrayItem {
  /// Unique while the item is registered: its bus name and object path,
  /// such as `:1.42/StatusNotifierItem`. The key to give
  /// `children_keyed`.
  pub id: String,
  /// The app's name for itself, such as `nm-applet`. Two copies of one
  /// app share it.
  pub app_id: String,
  /// A name for the item, such as `Network`.
  pub title: String,
  /// Whether it wants to be seen.
  pub status: TrayStatus,
  /// A freedesktop icon name, or empty.
  pub icon_name: String,
  /// The icon as raw pixels, in each size the app sent, for an item with
  /// no icon name. Each side is at most 512 px.
  pub icon_pixmaps: Vec<IconPixels>,
  /// The icon name to show while it needs attention, or empty.
  pub attention_icon_name: String,
  /// A directory where the app keeps its own icon files, or empty.
  pub icon_theme_path: String,
  /// The tooltip's title, or empty.
  pub tooltip: String,
  /// Whether the item offers only its menu, so a left click opens the
  /// menu.
  pub item_is_menu: bool,
}

/// The tray, from `cx.tray()`. A clone is cheap and shares the one
/// connection.
#[derive(Clone)]
pub struct TrayHost {
  items: ReadSignal<Vec<TrayItem>>,
}

impl TrayHost {
  /// Every item, in the order they registered. Empty while there is no
  /// session bus. An item shows once its properties are read.
  pub fn items(&self) -> ReadSignal<Vec<TrayItem>> {
    self.items
  }
}

/// `cx.tray()`.
pub(crate) fn tray(cx: &Cx) -> TrayHost {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.tray() needs the services the shell provides; the tray stays empty");
    });
    return TrayHost {
      items: cx.signal(Vec::new()).read_only(),
    };
  };
  let items = service.start();
  TrayHost { items }
}

/// The tray of one shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  items: Signal<Vec<TrayItem>>,
  /// Every registered item, in the order they came, read or not.
  entries: Vec<Entry>,
  /// Whether this shell owns the watcher's name.
  watcher: bool,
}

struct Entry {
  id: String,
  bus: String,
  path: String,
  /// The unique name behind `bus`, which the item's signals come from.
  /// `None` until the bus says.
  owner: Option<String>,
  /// Counts reads, so a slow reply never overwrites a newer one.
  reads: u64,
  /// The item as last read. `None` until a read works.
  shown: Option<TrayItem>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// The list, joining the bus if this is the first ask.
  fn start(self: &Rc<Self>) -> ReadSignal<Vec<TrayItem>> {
    if let Some(state) = &*self.state.borrow() {
      return state.items.read_only();
    }
    let items = self.shared.root.signal(Vec::new());
    *self.state.borrow_mut() = Some(State {
      items,
      entries: Vec::new(),
      watcher: false,
    });
    match self.shared.config.session_bus.clone() {
      Some(address) => self.shared.spawn(run(Rc::clone(self), address)),
      None => tracing::info!("no session bus, so the tray is empty"),
    }
    items.read_only()
  }

  /// Whether the list is still there: its owner, the root, isn't disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.items.try_with(|_| ()));
    alive.is_some()
  }

  /// Every registered item's id, read or not, as the watcher lists them.
  fn ids(&self) -> Vec<String> {
    let state = self.state.borrow();
    let entries = state.as_ref().map(|state| state.entries.as_slice());
    entries
      .unwrap_or_default()
      .iter()
      .map(|entry| entry.id.clone())
      .collect()
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

  /// Subscribes to what items say, then takes a host's name and queues
  /// for the watcher's.
  async fn join(self: &Rc<Self>, connection: &Connection) -> zbus::Result<()> {
    bus_call(
      connection,
      "AddMatch",
      &format!("type='signal',interface='{ITEM}'"),
    )
    .await?;
    let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
    connection.request_name(host.as_str()).await?;
    // Queued rather than refused, so the bus hands the name over when
    // another watcher leaves. NameAcquired says so, in `on_signal`.
    connection
      .request_name_with_flags(WATCHER, RequestNameFlags::empty())
      .await?;
    Ok(())
  }

  /// Acts on one message from the bus.
  async fn on_message(self: &Rc<Self>, connection: &Connection, message: &Message) {
    match message.header().message_type() {
      Kind::MethodCall => watcher::answer(self, connection, message).await,
      Kind::Signal => self.on_signal(connection, message),
      _ => {}
    }
  }

  fn on_signal(self: &Rc<Self>, connection: &Connection, message: &Message) {
    let header = message.header();
    let (Some(interface), Some(member)) = (header.interface(), header.member()) else {
      return;
    };
    let from_bus = header.sender().is_some_and(|sender| sender.as_str() == BUS);
    let body = message.body();
    match (interface.as_str(), member.as_str()) {
      (BUS, "NameAcquired") if from_bus && body.deserialize::<&str>().ok() == Some(WATCHER) => {
        self.become_watcher(connection);
      }
      (BUS, "NameOwnerChanged") if from_bus => {
        if let Ok((name, _, owner)) = body.deserialize::<(&str, &str, &str)>() {
          self.owner_changed(connection, name, owner);
        }
      }
      // NewIcon, NewTitle, NewStatus and the rest: read it all again.
      (ITEM, member) if member.starts_with("New") => {
        let (sender, path) = (header.sender(), header.path());
        let (Some(sender), Some(path)) = (sender, path) else {
          return;
        };
        let state = self.state.borrow();
        let entries = state.as_ref().map(|state| state.entries.as_slice());
        let ids: Vec<String> = entries
          .unwrap_or_default()
          .iter()
          .filter(|entry| entry.owner.as_deref() == Some(sender.as_str()))
          .filter(|entry| entry.path == path.as_str())
          .map(|entry| entry.id.clone())
          .collect();
        drop(state);
        for id in ids {
          self.read(connection, id);
        }
      }
      _ => {}
    }
  }

  /// This shell owns the watcher's name now, from the start or after
  /// another watcher left. The items already known stay.
  fn become_watcher(&self, connection: &Connection) {
    tracing::info!("running the tray watcher");
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.watcher = true;
    }
    self.emit(connection, "StatusNotifierHostRegistered", None);
  }

  /// Starts showing the item at `bus` and `path`, unless it is already
  /// there: the bus is asked who owns `bus`, then the item is read.
  fn add(self: &Rc<Self>, connection: &Connection, bus: String, path: String) {
    let id = format!("{bus}{path}");
    let mut state = self.state.borrow_mut();
    let Some(state) = state.as_mut() else {
      return;
    };
    if state.entries.iter().any(|entry| entry.id == id) {
      return;
    }
    state.entries.push(Entry {
      id: id.clone(),
      bus: bus.clone(),
      path,
      owner: None,
      reads: 0,
      shown: None,
    });
    if state.watcher {
      self.emit(connection, "StatusNotifierItemRegistered", Some(id.clone()));
    }
    let service = Rc::clone(self);
    let connection = connection.clone();
    self.shared.spawn(async move {
      // Before asking for the owner, so a leave in between is still seen.
      if let Err(error) = bus_call(&connection, "AddMatch", &owner_rule(&bus)).await {
        tracing::warn!(id, %error, "can't watch a tray item's name");
      }
      let ask = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &bus);
      let owner = ask
        .await
        .and_then(|reply| reply.body().deserialize::<String>());
      match owner {
        Ok(owner) => {
          service.with_entry(&id, |entry| entry.owner = Some(owner));
          service.read(&connection, id);
        }
        Err(error) => {
          tracing::info!(id, %error, "a tray item left before it was read");
          service.remove(&connection, &id);
        }
      }
    });
  }

  /// Reads item `id`'s properties again and shows them. A failed read is
  /// logged, and the item keeps what it showed.
  fn read(self: &Rc<Self>, connection: &Connection, id: String) {
    let read = self.with_entry(&id, |entry| {
      entry.reads += 1;
      (entry.bus.clone(), entry.path.clone(), entry.reads)
    });
    let Some((bus, path, read)) = read else {
      return;
    };
    let service = Rc::clone(self);
    let connection = connection.clone();
    self.shared.spawn(async move {
      let get_all = connection.call_method(
        Some(bus.as_str()),
        path.as_str(),
        Some(PROPERTIES),
        "GetAll",
        &ITEM,
      );
      let properties = get_all
        .await
        .and_then(|reply| reply.body().deserialize::<Properties>());
      let item = match properties {
        Ok(properties) => properties.into_item(id.clone()),
        Err(error) => return tracing::warn!(id, %error, "can't read a tray item"),
      };
      let newest = service.with_entry(&id, |entry| {
        if entry.reads == read {
          entry.shown = Some(item);
        }
      });
      if newest.is_some() {
        service.publish();
      }
    });
  }

  /// A name an item is at got a new owner, or none when it left the bus.
  fn owner_changed(self: &Rc<Self>, connection: &Connection, name: &str, owner: &str) {
    let ids: Vec<String> = {
      let state = self.state.borrow();
      let entries = state.as_ref().map(|state| state.entries.as_slice());
      let at_name = entries
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.bus == name);
      at_name.map(|entry| entry.id.clone()).collect()
    };
    for id in ids {
      if owner.is_empty() {
        self.remove(connection, &id);
      } else {
        self.with_entry(&id, |entry| entry.owner = Some(owner.to_owned()));
        self.read(connection, id);
      }
    }
  }

  /// Stops showing item `id`. One that isn't there is ignored.
  fn remove(&self, connection: &Connection, id: &str) {
    let removed = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      let at = state.entries.iter().position(|entry| entry.id == id);
      at.map(|at| (state.entries.remove(at), state.watcher))
    };
    let Some((entry, watcher)) = removed else {
      return;
    };
    let rule = owner_rule(&entry.bus);
    let bus = connection.clone();
    self.shared.spawn(async move {
      if let Err(error) = bus_call(&bus, "RemoveMatch", &rule).await {
        tracing::debug!(%error, "a tray item's name match was already gone");
      }
    });
    if watcher {
      self.emit(connection, "StatusNotifierItemUnregistered", Some(entry.id));
    }
    self.publish();
  }

  /// Sets the list from the items read so far, if it changed.
  fn publish(&self) {
    let state = self.state.borrow();
    let Some(state) = state.as_ref() else {
      return;
    };
    let list: Vec<TrayItem> = state
      .entries
      .iter()
      .filter_map(|entry| entry.shown.clone())
      .collect();
    if state.items.try_with(|old| *old != list) == Some(true) {
      state.items.set(list);
    }
  }

  /// Empties the list, once the bus is gone.
  fn clear(&self) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.entries.clear();
    }
    self.publish();
  }

  /// Sends one of the watcher's signals, with an item's id or nothing.
  fn emit(&self, connection: &Connection, signal: &'static str, id: Option<String>) {
    let connection = connection.clone();
    self.shared.spawn(async move {
      let sent = match id {
        Some(id) => {
          let emit = connection.emit_signal(None::<&str>, WATCHER_PATH, WATCHER, signal, &id);
          emit.await
        }
        None => {
          let emit = connection.emit_signal(None::<&str>, WATCHER_PATH, WATCHER, signal, &());
          emit.await
        }
      };
      if let Err(error) = sent {
        tracing::warn!(signal, %error, "the tray watcher couldn't send a signal");
      }
    });
  }
}

/// Joins the session bus at `address` and acts on what arrives, until the
/// bus closes or the list is disposed.
async fn run(service: Rc<Service>, address: zbus::Address) {
  // A call to an app that hangs fails after this, instead of keeping its
  // future forever. There is no timer while no call is out.
  let builder = zbus::connection::Builder::address(address).method_timeout(CALL_TIMEOUT);
  let connection = match builder.build().await {
    Ok(connection) => connection,
    Err(error) => {
      return tracing::info!(%error, "can't reach the session bus, so the tray is empty");
    }
  };
  // Made before anything is asked, so no reply or signal is missed.
  let mut messages = MessageStream::from(&connection);
  if let Err(error) = service.join(&connection).await {
    return tracing::warn!(%error, "couldn't join the tray on the session bus");
  }
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !service.alive() {
      return service.clear();
    }
    match next {
      Some(Ok(message)) => service.on_message(&connection, &message).await,
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the session bus"),
      None => break,
    }
  }
  tracing::warn!("the session bus closed, so the tray is empty");
  service.clear();
}

/// Calls the bus itself, such as `AddMatch` with a match rule.
async fn bus_call(connection: &Connection, method: &str, rule: &str) -> zbus::Result<()> {
  let call = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), method, &rule);
  call.await.map(drop)
}

/// The match rule that says when `bus` gets a new owner or none. `bus`
/// is a checked bus name, so it holds no quote.
fn owner_rule(bus: &str) -> String {
  format!("type='signal',sender='{BUS}',interface='{BUS}',member='NameOwnerChanged',arg0='{bus}'")
}

/// The bus name and object path a registration names. A path alone is
/// at `sender`, as Ayatana and Electron apps register. A name alone is at
/// `/StatusNotifierItem`. A watcher's list joins the two, as in
/// `:1.42/org/ayatana/NotificationItem/nm`. `None` for anything else.
fn parse_service(service: &str, sender: Option<&str>) -> Option<(String, String)> {
  let (bus, path) = match service.find('/') {
    Some(0) => (sender?, service),
    Some(at) => service.split_at(at),
    None => (service, ITEM_PATH),
  };
  BusName::try_from(bus).ok()?;
  ObjectPath::try_from(path).ok()?;
  Some((bus.to_owned(), path.to_owned()))
}

/// An icon in the spec's `a(iiay)`: width, height and ARGB32 bytes.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// The properties an item's `GetAll` returns that the tray reads. One of
/// the wrong type fails the whole read.
#[derive(Default, DeserializeDict, Type)]
#[cfg_attr(test, derive(Clone, zbus::SerializeDict))]
#[zbus(signature = "a{sv}", rename_all = "PascalCase")]
struct Properties {
  id: Option<String>,
  title: Option<String>,
  status: Option<String>,
  icon_name: Option<String>,
  icon_pixmap: Option<Pixmaps>,
  attention_icon_name: Option<String>,
  icon_theme_path: Option<String>,
  /// Icon name, icon pixmaps, title and description.
  tool_tip: Option<(String, Pixmaps, String, String)>,
  item_is_menu: Option<bool>,
}

impl Properties {
  fn into_item(self, id: String) -> TrayItem {
    let status = match self.status.as_deref() {
      Some("Passive") => TrayStatus::Passive,
      Some("NeedsAttention") => TrayStatus::NeedsAttention,
      Some("Active") | None => TrayStatus::Active,
      Some(other) => {
        tracing::debug!(
          id,
          status = other,
          "a tray item's status is unknown; taken as Active"
        );
        TrayStatus::Active
      }
    };
    TrayItem {
      icon_pixmaps: pixmaps(&id, self.icon_pixmap.unwrap_or_default()),
      app_id: self.id.unwrap_or_default(),
      title: self.title.unwrap_or_default(),
      status,
      icon_name: self.icon_name.unwrap_or_default(),
      attention_icon_name: self.attention_icon_name.unwrap_or_default(),
      icon_theme_path: self.icon_theme_path.unwrap_or_default(),
      tooltip: self
        .tool_tip
        .map(|(_, _, title, _)| title)
        .unwrap_or_default(),
      item_is_menu: self.item_is_menu.unwrap_or_default(),
      id,
    }
  }
}

/// The pixmaps a bar can draw. One that is empty, over 512 px a side, or
/// whose bytes don't match its size is logged and skipped. The smallest
/// are kept first, up to [`MAX_PIXMAP_BYTES`] in all.
fn pixmaps(id: &str, raw: Pixmaps) -> Vec<IconPixels> {
  let side = 1..=MAX_PIXMAP_SIDE;
  let mut kept: Vec<IconPixels> = raw
    .into_iter()
    .filter_map(|(width, height, data)| {
      let fits = side.contains(&width) && side.contains(&height);
      // Both sides are positive and at most 512, so the casts are exact.
      if !fits || data.len() != width as usize * height as usize * 4 {
        let bytes = data.len();
        tracing::debug!(
          id,
          width,
          height,
          bytes,
          "a tray icon pixmap of a bad size; skipped"
        );
        return None;
      }
      let (width, height) = (width as u32, height as u32);
      Some(IconPixels {
        width,
        height,
        data: data.into(),
      })
    })
    .collect();
  kept.sort_by_key(|pixels| pixels.data.len());
  let mut total = 0;
  kept.retain(|pixels| {
    total += pixels.data.len();
    total <= MAX_PIXMAP_BYTES
  });
  kept
}
