//! The connections NetworkManager saved, read on the first ask over a
//! bus connection of their own, so a config that never lists them reads
//! none and never wakes for their changes.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crabshell_reactive::Signal;
use serde_json::Value as Json;
use zbus::{Connection, Message, OwnedObjectPath, Variant};

use super::{NM, by_number};
use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, Props, set_changed};

pub(super) const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
pub(super) const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
pub(super) const SAVED: &str = "org.freedesktop.NetworkManager.Settings.Connection";

/// The most saved connections read: a laptop keeps one for each Wi-Fi
/// network it joined.
const MAX_SAVED: usize = 256;
/// The biggest reply a saved connection may come in; one with 802.1X
/// certificates inside is about 10 KiB.
const MAX_REPLY: usize = 64 * 1024;
/// The most reply bytes whose settings are kept, over every saved
/// connection.
const MAX_SETTINGS: usize = 1024 * 1024;

/// A connection NetworkManager saved, as for a Wi-Fi network joined
/// before.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedConnection {
  /// What names it for good, as [`forget`](super::Network::forget) and
  /// [`clear_secrets`](super::Network::clear_secrets) take.
  pub uuid: String,
  /// Its name, often the Wi-Fi network's.
  pub id: String,
  /// NetworkManager's type for it, such as `802-11-wireless`,
  /// `802-3-ethernet`, `vpn` or `wireguard`. Its settings for that type
  /// are under the same name.
  pub kind: String,
  /// Its settings by group and name, such as `settings["ipv4"]["method"]`,
  /// without secrets, which NetworkManager never hands out with them. A
  /// byte array, such as an SSID, is an array of numbers. `null` for one
  /// whose settings didn't fit in the 1 MiB kept for them all.
  pub settings: Json,
}

impl SavedConnection {
  /// One from NetworkManager's `settings`, keeping them if `keep`.
  fn new(settings: &HashMap<String, Props>, keep: bool) -> SavedConnection {
    let about = settings.get("connection");
    let text = |name| about.map_or(String::new(), |props| dbus::text(props, name));
    let groups = settings.iter().map(|(group, props)| {
      let props = props
        .iter()
        .map(|(name, value)| (name.clone(), json(value)));
      (group.clone(), Json::Object(props.collect()))
    });
    SavedConnection {
      uuid: text("uuid"),
      id: text("id"),
      kind: text("type"),
      settings: if keep {
        Json::Object(groups.collect())
      } else {
        Json::Null
      },
    }
  }
}

/// `value` as JSON: a dictionary as an object, an array or a structure
/// as an array. A file descriptor, or a number JSON can't hold, is `null`.
pub(super) fn json(value: &Variant<'_>) -> Json {
  match value {
    Variant::U8(n) => Json::from(*n),
    Variant::Bool(b) => Json::from(*b),
    Variant::I16(n) => Json::from(*n),
    Variant::U16(n) => Json::from(*n),
    Variant::I32(n) => Json::from(*n),
    Variant::U32(n) => Json::from(*n),
    Variant::I64(n) => Json::from(*n),
    Variant::U64(n) => Json::from(*n),
    Variant::F64(n) => Json::from(*n),
    Variant::Str(text) => Json::from(text.as_str()),
    Variant::Signature(signature) => Json::from(signature.to_string()),
    Variant::ObjectPath(path) => Json::from(path.as_str()),
    Variant::Variant(inner) => json(inner),
    Variant::Array(array) => array.inner().iter().map(json).collect(),
    Variant::Structure(structure) => structure.fields().iter().map(json).collect(),
    Variant::Dict(dict) => {
      let entries = dict.iter().map(|(key, value)| {
        let key = match key {
          Variant::Str(text) => text.as_str().to_owned(),
          key => json(key).to_string(),
        };
        (key, json(value))
      });
      Json::Object(entries.collect())
    }
    Variant::Fd(_) => Json::Null,
  }
}

/// The saved connections for one shell, followed from the first ask.
pub(super) struct SavedConnections {
  shared: Rc<Shared>,
  pub(super) list: Signal<Vec<SavedConnection>>,
  followed: Cell<bool>,
  generation: Cell<u64>,
}

impl SavedConnections {
  pub(super) fn new(shared: Rc<Shared>) -> Rc<SavedConnections> {
    let list = shared.root.signal(Vec::new());
    Rc::new(SavedConnections {
      shared,
      list,
      followed: Cell::new(false),
      generation: Cell::new(0),
    })
  }

  /// Starts following them, the first time.
  pub(super) fn follow(self: &Rc<Self>) {
    if self.followed.replace(true) {
      return;
    }
    // Without a system bus there is nothing to follow, which the network
    // service has logged.
    if let Some(address) = self.shared.config.system_bus.clone() {
      let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
      self.shared.spawn(follow);
    }
  }
}

impl Daemon for SavedConnections {
  const NAME: &'static str = NM;

  /// A saved connection added, removed or changed: nothing else.
  fn rules() -> Vec<String> {
    vec![
      format!("type='signal',sender='{NM}',path='{SETTINGS_PATH}',interface='{SETTINGS}'"),
      format!("type='signal',sender='{NM}',interface='{SAVED}',member='Updated'"),
    ]
  }

  fn alive(&self) -> bool {
    self.list.try_with(|_| ()).is_some()
  }

  fn clear(&self) -> u64 {
    set_changed(self.list, Vec::new());
    self.generation.set(self.generation.get() + 1);
    self.generation.get()
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let shared = &self.shared;
    let call = connection.call_method(
      Some(NM),
      SETTINGS_PATH,
      Some(SETTINGS),
      "ListConnections",
      &(),
    );
    let listed = async {
      let reply = shared.timeout(CALL_TIMEOUT, call).await?;
      reply.body().deserialize::<Vec<OwnedObjectPath>>()
    };
    let mut paths = match listed.await {
      Ok(paths) => paths,
      Err(error) => {
        return tracing::warn!(%error, "couldn't list NetworkManager's saved connections");
      }
    };
    // By number, so the cap keeps the oldest, the same every time.
    paths.sort_by(|a, b| by_number(a.as_str(), b.as_str()));
    let (mut list, mut room, mut left_out) = (Vec::new(), MAX_SETTINGS, 0);
    for path in paths.iter().take(MAX_SAVED).map(|path| path.as_str()) {
      let call = connection.call_method(Some(NM), path, Some(SAVED), "GetSettings", &());
      let reply = match shared.timeout(CALL_TIMEOUT, call).await {
        Ok(reply) => reply,
        // Gone since the list, or hidden from this user.
        Err(error) => {
          tracing::info!(%error, path, "couldn't read a saved connection");
          continue;
        }
      };
      let body = reply.body();
      if body.len() > MAX_REPLY {
        tracing::warn!(
          path,
          bytes = body.len(),
          "a saved connection too big; left out"
        );
        continue;
      }
      let Ok(settings) = body.deserialize::<HashMap<String, Props>>() else {
        tracing::warn!(
          path,
          "a saved connection's settings that don't parse; left out"
        );
        continue;
      };
      let keep = body.len() <= room;
      if keep {
        room -= body.len();
      } else {
        left_out += 1;
      }
      list.push(SavedConnection::new(&settings, keep));
    }
    if left_out > 0 {
      tracing::warn!(left_out, "saved connections' settings past 1 MiB left out");
    }
    if self.generation.get() == generation {
      set_changed(self.list, list);
    }
  }

  fn signal(self: &Rc<Self>, _: &Connection, _: &Message) {}

  /// Each signal names a saved connection without its settings, so all
  /// are read again.
  fn reads_again(&self, _: &Message) -> bool {
    true
  }
}
