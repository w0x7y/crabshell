//! Network devices and Wi-Fi networks from NetworkManager, on the system
//! bus.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Once;

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::{Array, Connection, Message, ObjectPath, OwnedObjectPath, Variant};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, OBJECT_MANAGER, PROPERTIES, Props, set_changed};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
/// Where NetworkManager's object manager is.
const ROOT: &str = "/org/freedesktop";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";

/// The most devices kept, and the most active connections, about one a
/// device. A host running containers has a virtual device for each.
const MAX_DEVICES: usize = 64;
/// The most access points kept: a scan in a city finds hundreds.
const MAX_ACCESS_POINTS: usize = 512;
/// The longest SSID 802.11 allows, in bytes.
const MAX_SSID: usize = 32;

/// NetworkManager's `DeviceType` for loopback, left out of the list.
const LOOPBACK: u32 = 32;
/// NetworkManager's `State` for a device it doesn't manage, left out.
const UNMANAGED: u32 = 10;
const ACTIVATED: u32 = 100;
/// The `Flags` bit for an access point that asks for a key, as WEP does.
const PRIVACY: u32 = 0x1;

/// What a network device is, from NetworkManager's `DeviceType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkKind {
  /// A cable.
  Ethernet,
  /// Wi-Fi.
  Wifi,
  /// Anything else, such as a bridge or a VPN's tunnel.
  Other,
}

/// How a network device is doing, from NetworkManager's `State`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkState {
  /// Connected.
  Connected,
  /// On its way to connected: from preparing to waiting on secondary
  /// connections.
  Connecting,
  /// Ready but not connected, going down, or failed to connect.
  Disconnected,
  /// Not ready, as with Wi-Fi off or no cable in, or a state not known.
  Unavailable,
}

/// A network device NetworkManager manages, other than loopback.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkDevice {
  /// NetworkManager's object path for it, which names it while it exists.
  pub path: String,
  /// Its interface, such as `wlan0`.
  pub interface: String,
  /// What it is.
  pub kind: NetworkKind,
  /// How it is doing.
  pub state: NetworkState,
  /// The name of its active connection, or `""`.
  pub connection: String,
}

impl NetworkDevice {
  fn from_props(path: String, props: &Props, objects: &BTreeMap<String, Object>) -> NetworkDevice {
    let connection = dbus::get::<&ObjectPath>(props, "ActiveConnection")
      .and_then(|at| objects.get(at.as_str())?.active.as_ref())
      .map_or(String::new(), |active| dbus::text(active, "Id"));
    NetworkDevice {
      path,
      interface: dbus::text(props, "Interface"),
      kind: match dbus::get::<u32>(props, "DeviceType") {
        Some(1) => NetworkKind::Ethernet,
        Some(2) => NetworkKind::Wifi,
        _ => NetworkKind::Other,
      },
      state: match dbus::get::<u32>(props, "State") {
        Some(ACTIVATED) => NetworkState::Connected,
        Some(40..=90) => NetworkState::Connecting,
        Some(30 | 110 | 120) => NetworkState::Disconnected,
        _ => NetworkState::Unavailable,
      },
      connection,
    }
  }
}

/// A Wi-Fi network a device sees.
#[derive(Clone, Debug, PartialEq)]
pub struct WifiNetwork {
  /// Its name, at most 32 bytes, with bytes that aren't UTF-8 shown as
  /// `�`.
  pub ssid: String,
  /// How strong its strongest access point is, from 0 to 100.
  pub strength: u8,
  /// Whether it asks for a key.
  pub secured: bool,
  /// Whether the device is connected to it, through any of its access
  /// points.
  pub connected: bool,
  /// The path of the device that sees it.
  pub device: String,
  /// The path of its strongest access point.
  pub access_point: String,
}

/// Networking from NetworkManager, from `cx.network()`. Cloning it is
/// cheap.
#[derive(Clone)]
pub struct Network {
  service: Option<Rc<NetworkService>>,
  wifi_enabled: ReadSignal<bool>,
  devices: ReadSignal<Vec<NetworkDevice>>,
  wifi_networks: ReadSignal<Vec<WifiNetwork>>,
}

impl Network {
  /// Whether Wi-Fi is on. `false` while NetworkManager isn't running.
  pub fn wifi_enabled(&self) -> ReadSignal<bool> {
    self.wifi_enabled
  }

  /// Every device but loopback and those NetworkManager doesn't manage,
  /// in NetworkManager's numbering. Empty while it isn't running.
  pub fn devices(&self) -> ReadSignal<Vec<NetworkDevice>> {
    self.devices
  }

  /// The Wi-Fi networks each device sees, one per name and device,
  /// strongest first. Hidden networks are left out. Empty while
  /// NetworkManager isn't running.
  pub fn wifi_networks(&self) -> ReadSignal<Vec<WifiNetwork>> {
    self.wifi_networks
  }

  /// Turns Wi-Fi on or off.
  pub fn set_wifi_enabled(&self, enabled: bool) {
    let body = (NM, "WirelessEnabled", Variant::from(enabled));
    self.call(NM_PATH, PROPERTIES, "Set", body);
  }

  /// Asks the Wi-Fi device at `device`, its path, to look for networks.
  /// NetworkManager refuses a scan right after another, and that is
  /// logged.
  pub fn scan(&self, device: &str) {
    let options = HashMap::<&str, Variant<'static>>::new();
    self.call(device, WIRELESS, "RequestScan", options);
  }

  /// Connects `network`'s device to it. Without a password,
  /// NetworkManager uses a connection saved for it, and refuses if it has
  /// none. With one, it saves a new WPA connection and uses that. A
  /// refusal is logged, never the password.
  pub fn connect(&self, network: &WifiNetwork, password: Option<&str>) {
    let paths = (
      ObjectPath::try_from(network.device.clone()),
      ObjectPath::try_from(network.access_point.clone()),
    );
    let (Ok(device), Ok(at)) = paths else {
      return tracing::info!(network.ssid, "no such Wi-Fi network; nothing sent");
    };
    let Some(password) = password else {
      let pick = ObjectPath::from_static_str_unchecked("/");
      return self.call(NM_PATH, NM, "ActivateConnection", (pick, device, at));
    };
    // The access point's own bytes: `ssid` may have lost some.
    let ssid = self
      .service
      .as_ref()
      .and_then(|service| {
        let state = service.state.borrow();
        let object = state.as_ref()?.objects.get(&network.access_point)?;
        Some(ssid(object.access_point.as_ref()?))
      })
      .unwrap_or_else(|| network.ssid.clone().into_bytes());
    let wireless = HashMap::from([("ssid", Variant::from(ssid))]);
    let security = HashMap::from([
      ("key-mgmt", Variant::from("wpa-psk")),
      ("psk", Variant::from(password.to_owned())),
    ]);
    let settings = HashMap::from([
      ("802-11-wireless", wireless),
      ("802-11-wireless-security", security),
    ]);
    let body = (settings, device, at);
    self.call(NM_PATH, NM, "AddAndActivateConnection", body);
  }

  /// Disconnects the device at `device`, until it is connected again.
  pub fn disconnect(&self, device: &str) {
    self.call(device, DEVICE, "Disconnect", ());
  }

  /// Calls `method` on NetworkManager's object at `path`, and logs a
  /// refusal, without the body.
  fn call<B>(&self, path: &str, interface: &'static str, method: &'static str, body: B)
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let Some(service) = &self.service else {
      return;
    };
    let Some(connection) = service
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.connection.clone())
    else {
      return tracing::info!(method, "NetworkManager isn't running; nothing sent");
    };
    let (shared, path) = (Rc::clone(&service.shared), path.to_owned());
    service.shared.spawn(async move {
      let call = connection.call_method(Some(NM), path.as_str(), Some(interface), method, &body);
      if let Err(error) = shared.timeout(CALL_TIMEOUT, call).await {
        tracing::warn!(%error, path, method, "NetworkManager refused it");
      }
    });
  }
}

/// `cx.network()`.
pub(crate) fn network(cx: &Cx) -> Network {
  let Some(service) = cx.service::<NetworkService>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.network() needs the services the shell provides; it stays empty");
    });
    return Network {
      service: None,
      wifi_enabled: cx.signal(false).read_only(),
      devices: cx.signal(Vec::new()).read_only(),
      wifi_networks: cx.signal(Vec::new()).read_only(),
    };
  };
  service.start()
}

/// NetworkManager for one shell, followed from the first ask.
pub(crate) struct NetworkService {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  wifi_enabled: Signal<bool>,
  devices: Signal<Vec<NetworkDevice>>,
  wifi_networks: Signal<Vec<WifiNetwork>>,
  /// NetworkManager's objects that this service reads, by path.
  objects: BTreeMap<String, Object>,
  /// The bus, while NetworkManager runs, for the methods.
  connection: Option<Connection>,
  generation: u64,
}

/// The interfaces this service reads on one of NetworkManager's objects.
/// An object is kept while it has one other than `wireless`.
#[derive(Default)]
struct Object {
  manager: Option<Props>,
  device: Option<Props>,
  wireless: Option<Props>,
  access_point: Option<Props>,
  active: Option<Props>,
}

impl Object {
  /// Where `interface`'s properties go, or `None` for one this service
  /// doesn't read.
  fn slot(&mut self, interface: &str) -> Option<&mut Option<Props>> {
    match interface {
      NM => Some(&mut self.manager),
      DEVICE => Some(&mut self.device),
      WIRELESS => Some(&mut self.wireless),
      ACCESS_POINT => Some(&mut self.access_point),
      ACTIVE => Some(&mut self.active),
      _ => None,
    }
  }

  fn kept(&self) -> bool {
    self.manager.is_some()
      || self.device.is_some()
      || self.access_point.is_some()
      || self.active.is_some()
  }
}

/// Orders NetworkManager's paths by the number that ends them, so
/// `Devices/2` comes before `Devices/10`.
fn by_number(a: &str, b: &str) -> Ordering {
  (a.len(), a).cmp(&(b.len(), b))
}

/// An access point's SSID, cut to 802.11's 32 bytes. Empty for one that
/// isn't an `ay`.
fn ssid(props: &Props) -> Vec<u8> {
  let bytes = dbus::get::<&Array>(props, "Ssid");
  let bytes = bytes.iter().flat_map(|bytes| bytes.inner());
  let bytes = bytes.map_while(|byte| match byte {
    Variant::U8(byte) => Some(*byte),
    _ => None,
  });
  bytes.take(MAX_SSID).collect()
}

/// The object paths in the `ao` property `name`.
fn paths<'a>(props: &'a Props, name: &str) -> impl Iterator<Item = &'a str> {
  let paths = dbus::get::<&Array>(props, name);
  paths
    .into_iter()
    .flat_map(|paths| paths.inner())
    .filter_map(|path| match path {
      Variant::ObjectPath(path) => Some(path.as_str()),
      _ => None,
    })
}

/// Gives the object at `path` `interfaces`, unless that would make more
/// devices, access points or active connections than the caps allow.
fn add(objects: &mut BTreeMap<String, Object>, path: String, interfaces: HashMap<String, Props>) {
  let mut object = objects.remove(&path).unwrap_or_default();
  let count = |kind: fn(&Object) -> bool| objects.values().filter(|object| kind(object)).count();
  for (interface, props) in interfaces {
    let full = match interface.as_str() {
      DEVICE => object.device.is_none() && count(|o| o.device.is_some()) >= MAX_DEVICES,
      ACTIVE => object.active.is_none() && count(|o| o.active.is_some()) >= MAX_DEVICES,
      ACCESS_POINT => {
        object.access_point.is_none() && count(|o| o.access_point.is_some()) >= MAX_ACCESS_POINTS
      }
      _ => false,
    };
    if full {
      tracing::warn!(path, interface, "too many network objects; leaving one out");
    } else if let Some(slot) = object.slot(&interface) {
      *slot = Some(props);
    }
  }
  if object.kept() {
    objects.insert(path, object);
  }
}

/// The networks each Wi-Fi device in `objects` sees: the strongest access
/// point per SSID and device, strongest first.
fn wifi_networks(objects: &BTreeMap<String, Object>) -> Vec<WifiNetwork> {
  let mut best = BTreeMap::<(&str, Vec<u8>), WifiNetwork>::new();
  for (path, object) in objects {
    let (Some(device), Some(wireless)) = (&object.device, &object.wireless) else {
      continue;
    };
    let on = dbus::get::<u32>(device, "State") == Some(ACTIVATED);
    let active = dbus::get::<&ObjectPath>(wireless, "ActiveAccessPoint");
    let active = active.map(|at| at.as_str());
    for at in paths(wireless, "AccessPoints") {
      let Some(point) = objects
        .get(at)
        .and_then(|object| object.access_point.as_ref())
      else {
        continue;
      };
      let ssid = ssid(point);
      // Hidden: empty, or NUL bytes standing in for its length.
      if ssid.iter().all(|byte| *byte == 0) {
        continue;
      }
      let flags = |name| dbus::get::<u32>(point, name).unwrap_or(0);
      let network = WifiNetwork {
        ssid: String::from_utf8_lossy(&ssid).into_owned(),
        strength: dbus::get::<u8>(point, "Strength").unwrap_or(0).min(100),
        secured: flags("Flags") & PRIVACY != 0 || flags("WpaFlags") != 0 || flags("RsnFlags") != 0,
        connected: on && active == Some(at),
        device: path.clone(),
        access_point: at.to_owned(),
      };
      match best.entry((path, ssid)) {
        Entry::Vacant(entry) => {
          entry.insert(network);
        }
        Entry::Occupied(mut entry) => {
          let connected = entry.get().connected || network.connected;
          if network.strength > entry.get().strength {
            entry.insert(network);
          }
          entry.get_mut().connected = connected;
        }
      }
    }
  }
  let mut networks = best.into_values().collect::<Vec<_>>();
  networks.sort_by(|a, b| {
    let ties = || (&a.ssid, &a.device).cmp(&(&b.ssid, &b.device));
    b.strength.cmp(&a.strength).then_with(ties)
  });
  networks
}

impl NetworkService {
  pub(crate) fn new(shared: Rc<Shared>) -> NetworkService {
    NetworkService {
      shared,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> Network {
    if let Some(state) = &*self.state.borrow() {
      return Network {
        service: Some(Rc::clone(self)),
        wifi_enabled: state.wifi_enabled.read_only(),
        devices: state.devices.read_only(),
        wifi_networks: state.wifi_networks.read_only(),
      };
    }
    let root = &self.shared.root;
    let wifi_enabled = root.signal(false);
    let (devices, wifi_networks) = (root.signal(Vec::new()), root.signal(Vec::new()));
    *self.state.borrow_mut() = Some(State {
      wifi_enabled,
      devices,
      wifi_networks,
      objects: BTreeMap::new(),
      connection: None,
      generation: 0,
    });
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so there is no networking"),
    }
    Network {
      service: Some(Rc::clone(self)),
      wifi_enabled: wifi_enabled.read_only(),
      devices: devices.read_only(),
      wifi_networks: wifi_networks.read_only(),
    }
  }

  /// Shows Wi-Fi, the devices and the networks as they are now. Each is
  /// set only if it changed, since a scan changes much that none shows.
  fn publish(&self) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let objects = &state.objects;
      let enabled = objects
        .get(NM_PATH)
        .and_then(|object| dbus::get(object.manager.as_ref()?, "WirelessEnabled"))
        .unwrap_or(false);
      let mut devices = objects
        .iter()
        .filter_map(|(path, object)| {
          let props = object.device.as_ref()?;
          let kind = dbus::get::<u32>(props, "DeviceType");
          let state = dbus::get::<u32>(props, "State");
          if kind == Some(LOOPBACK) || state == Some(UNMANAGED) {
            return None;
          }
          Some(NetworkDevice::from_props(path.clone(), props, objects))
        })
        .collect::<Vec<_>>();
      devices.sort_by(|a, b| by_number(&a.path, &b.path));
      let shown = (enabled, devices, wifi_networks(objects));
      (
        state.wifi_enabled,
        state.devices,
        state.wifi_networks,
        shown,
      )
    });
    if let Some((enabled_signal, devices_signal, networks_signal, shown)) = shown {
      let (enabled, devices, networks) = shown;
      set_changed(enabled_signal, enabled);
      set_changed(devices_signal, devices);
      set_changed(networks_signal, networks);
    }
  }
}

impl Daemon for NetworkService {
  const NAME: &'static str = NM;

  fn alive(&self) -> bool {
    let state = self.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.devices.try_with(|_| ()).is_some())
  }

  fn clear(&self) -> u64 {
    let generation = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      state.objects.clear();
      state.connection = None;
      state.generation += 1;
      state.generation
    };
    self.publish();
    generation
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let call = connection.call_method(
      Some(NM),
      ROOT,
      Some(OBJECT_MANAGER),
      "GetManagedObjects",
      &(),
    );
    let read = async {
      let reply = self.shared.timeout(CALL_TIMEOUT, call).await?;
      reply
        .body()
        .deserialize::<HashMap<OwnedObjectPath, HashMap<String, Props>>>()
    };
    let managed = match read.await {
      Ok(managed) => managed,
      Err(error) => return tracing::warn!(%error, "couldn't read NetworkManager"),
    };
    // By number, so the caps keep the oldest objects, the same every time.
    let mut managed = managed
      .into_iter()
      .map(|(path, interfaces)| (path.as_str().to_owned(), interfaces))
      .collect::<Vec<_>>();
    managed.sort_by(|(a, _), (b, _)| by_number(a, b));
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state
        .as_mut()
        .filter(|state| state.generation == generation)
      else {
        return;
      };
      state.objects.clear();
      for (path, interfaces) in managed {
        add(&mut state.objects, path, interfaces);
      }
      state.connection = Some(connection);
    }
    self.publish();
  }

  fn signal(self: &Rc<Self>, _: &Connection, message: &Message) {
    if let Some((path, interface, changed, dropped)) = dbus::properties_changed(message) {
      {
        let mut state = self.state.borrow_mut();
        let props = state
          .as_mut()
          .and_then(|state| state.objects.get_mut(&path))
          .and_then(|object| object.slot(&interface)?.as_mut());
        let Some(props) = props else {
          return;
        };
        dbus::apply(props, changed, dropped);
      }
      return self.publish();
    }
    let header = message.header();
    if header.interface().map(|interface| interface.as_str()) != Some(OBJECT_MANAGER) {
      return;
    }
    let body = message.body();
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      match header.member().map(|member| member.as_str()) {
        Some("InterfacesAdded") => {
          let Ok((path, interfaces)) =
            body.deserialize::<(OwnedObjectPath, HashMap<String, Props>)>()
          else {
            return;
          };
          add(&mut state.objects, path.as_str().to_owned(), interfaces);
        }
        Some("InterfacesRemoved") => {
          let Ok((path, interfaces)) = body.deserialize::<(OwnedObjectPath, Vec<String>)>() else {
            return;
          };
          let Some(object) = state.objects.get_mut(path.as_str()) else {
            return;
          };
          for interface in interfaces {
            if let Some(slot) = object.slot(&interface) {
              *slot = None;
            }
          }
          if !object.kept() {
            state.objects.remove(path.as_str());
          }
        }
        _ => return,
      }
    }
    self.publish();
  }
}
