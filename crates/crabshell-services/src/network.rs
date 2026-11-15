//! Network devices, Wi-Fi networks and saved connections from
//! NetworkManager, on the system bus.

mod saved;
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

pub use saved::SavedConnection;
use saved::{SAVED, SETTINGS, SETTINGS_PATH, SavedConnections};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, OBJECT_MANAGER, PROPERTIES, Props, set_changed};

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
/// Where NetworkManager's object manager is.
const ROOT: &str = "/org/freedesktop";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const WIRED: &str = "org.freedesktop.NetworkManager.Device.Wired";
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
const FAILED: u32 = 120;
/// The `InterfaceFlags` bit for a link, from NetworkManager 1.22, which
/// takes over from `Device.Wired.Carrier`.
const CARRIER: u32 = 0x10000;
/// The `Flags` bit for an access point that asks for a key, as WEP does.
const PRIVACY: u32 = 0x1;
/// The key managements in an access point's `WpaFlags` and `RsnFlags`,
/// from NetworkManager's `NM80211ApSecurityFlags`.
const KEY_MGMT_PSK: u32 = 0x100;
const KEY_MGMT_802_1X: u32 = 0x200;
const KEY_MGMT_SAE: u32 = 0x400;
/// Enhanced Open, and its transition mode.
const KEY_MGMT_OWE: u32 = 0x800 | 0x1000;
const KEY_MGMT_SUITE_B_192: u32 = 0x2000;
/// The most objects kept, so a flood can't take the memory.
const MAX_OBJECTS: usize = 4096;

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
  /// Going down, as when told to disconnect or before connecting
  /// elsewhere.
  Disconnecting,
  /// Ready but not connected, or failed to connect.
  Disconnected,
  /// Not ready, as with Wi-Fi off or no cable in, or a state not known.
  Unavailable,
}

/// How far the host reaches, from NetworkManager's `Connectivity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connectivity {
  /// Not known: NetworkManager isn't running, its checks are off, or
  /// they haven't run yet.
  Unknown,
  /// On no network at all.
  None,
  /// Behind a captive portal, which wants a sign-in in a browser first.
  Portal,
  /// On a network, but not reaching the internet.
  Limited,
  /// Reaching the internet.
  Full,
}

/// Why a device's last try to connect failed, from the reason
/// NetworkManager gave with its `Failed` state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionFailReason {
  /// It needed secrets, such as a Wi-Fi password, and got none.
  NoSecrets,
  /// The Wi-Fi supplicant failed, or its settings did.
  WifiClientFailed,
  /// The Wi-Fi supplicant disconnected.
  WifiClientDisconnected,
  /// The Wi-Fi network took too long to let it in, as with a wrong
  /// password.
  WifiAuthTimeout,
  /// The Wi-Fi network wasn't found.
  WifiNetworkLost,
  /// Any other reason, such as DHCP getting no address.
  Unknown,
}

/// What a Wi-Fi device does, from NetworkManager's `NM80211Mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiMode {
  /// Not known, and for a device that isn't Wi-Fi.
  Unknown,
  /// Part of an ad-hoc network, with no access point.
  AdHoc,
  /// A station that joins networks, the usual mode.
  Station,
  /// An access point of its own, as a hotspot.
  AccessPoint,
  /// An 802.11s mesh point.
  Mesh,
}

/// How a Wi-Fi network is secured, from its access point's flags.
///
/// Dynamic WEP and LEAP look the same as WEP from an access point, so
/// all three show as [`Wep`](Self::Wep).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiSecurity {
  /// No encryption.
  Open,
  /// WEP.
  Wep,
  /// WPA with a password.
  WpaPsk,
  /// WPA with 802.1X, as an enterprise network.
  WpaEap,
  /// WPA2 with a password.
  Wpa2Psk,
  /// WPA2 with 802.1X.
  Wpa2Eap,
  /// WPA3 with a password, also for a network that takes WPA2's too.
  Sae,
  /// Enhanced Open: encrypted, with no password.
  Owe,
  /// WPA3 Enterprise's 192-bit mode.
  Wpa3SuiteB192,
  /// Flags this doesn't know.
  Unknown,
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
  /// Why its last try to connect failed, kept until it tries again.
  /// `None` if it hasn't failed since the shell started following it.
  pub fail_reason: Option<ConnectionFailReason>,
  /// What it does, for a Wi-Fi device; `Unknown` for any other.
  pub wifi_mode: WifiMode,
  /// For a wired device, its link's speed in Mb/s; 0 when not known, and
  /// for any other device.
  pub link_speed: u32,
  /// For a wired device, whether a cable is in, from the carrier bit of
  /// `InterfaceFlags`, or `Carrier` before NetworkManager 1.22; `false`
  /// for any other.
  pub has_link: bool,
}

impl NetworkDevice {
  fn from_props(path: String, object: &Object, objects: &BTreeMap<String, Object>) -> Option<Self> {
    let props = object.device.as_ref()?;
    let connection = dbus::get::<&ObjectPath>(props, "ActiveConnection")
      .and_then(|at| objects.get(at.as_str())?.active.as_ref())
      .map_or(String::new(), |active| dbus::text(active, "Id"));
    let wifi_mode = object
      .wireless
      .as_ref()
      .and_then(|w| dbus::get::<u32>(w, "Mode"));
    let wired = object.wired.as_ref();
    Some(NetworkDevice {
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
        Some(110) => NetworkState::Disconnecting,
        Some(30 | FAILED) => NetworkState::Disconnected,
        _ => NetworkState::Unavailable,
      },
      connection,
      fail_reason: object.failure,
      wifi_mode: match wifi_mode {
        Some(1) => WifiMode::AdHoc,
        Some(2) => WifiMode::Station,
        Some(3) => WifiMode::AccessPoint,
        Some(4) => WifiMode::Mesh,
        _ => WifiMode::Unknown,
      },
      link_speed: wired.and_then(|w| dbus::get(w, "Speed")).unwrap_or(0),
      has_link: wired
        .and_then(|wired| {
          let flags = dbus::get::<u32>(props, "InterfaceFlags");
          let carrier = flags.map(|flags| flags & CARRIER != 0);
          carrier.or_else(|| dbus::get(wired, "Carrier"))
        })
        .unwrap_or(false),
    })
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
  /// Whether it asks for a key: any [`security`](Self::security) but
  /// `Open`, `Owe` and `Unknown`.
  pub secured: bool,
  /// How its strongest access point is secured.
  pub security: WifiSecurity,
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
  connectivity: ReadSignal<Connectivity>,
  devices: ReadSignal<Vec<NetworkDevice>>,
  wifi_networks: ReadSignal<Vec<WifiNetwork>>,
  saved_connections: ReadSignal<Vec<SavedConnection>>,
}

impl Network {
  /// Whether Wi-Fi is on. `false` while NetworkManager isn't running.
  pub fn wifi_enabled(&self) -> ReadSignal<bool> {
    self.wifi_enabled
  }

  /// How far the host reaches, from NetworkManager's own checks.
  /// `Unknown` while it isn't running.
  pub fn connectivity(&self) -> ReadSignal<Connectivity> {
    self.connectivity
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
      return self.activate(network, device, at);
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
    // NetworkManager picks the key management, WPA2 or WPA3, from the
    // access point.
    let security = HashMap::from([("psk", Variant::from(password.to_owned()))]);
    let settings = HashMap::from([
      ("802-11-wireless", wireless),
      ("802-11-wireless-security", security),
    ]);
    let body = (settings, device, at);
    self.call(NM_PATH, NM, "AddAndActivateConnection", body);
  }

  /// Connects through a saved connection NetworkManager picks, and for
  /// an open network with none, through a new one.
  fn activate(&self, network: &WifiNetwork, device: ObjectPath<'static>, at: ObjectPath<'static>) {
    let Some((shared, connection)) = self.bus("ActivateConnection") else {
      return;
    };
    let (spawner, open) = (Rc::clone(&shared), !network.secured);
    let ssid = network.ssid.clone().into_bytes();
    let ssid = self
      .service
      .as_ref()
      .and_then(|service| {
        let state = service.state.borrow();
        let object = state.as_ref()?.objects.get(&network.access_point)?;
        Some(self::ssid(object.access_point.as_ref()?))
      })
      .unwrap_or(ssid);
    spawner.spawn(async move {
      let pick = ObjectPath::from_static_str_unchecked("/");
      let body = (pick, &device, &at);
      let call = connection.call_method(Some(NM), NM_PATH, Some(NM), "ActivateConnection", &body);
      let Err(error) = shared.timeout(CALL_TIMEOUT, call).await else {
        return;
      };
      if !open {
        return tracing::warn!(%error, "no saved connection for that network");
      }
      let wireless = HashMap::from([("ssid", Variant::from(ssid))]);
      let settings = HashMap::from([("802-11-wireless", wireless)]);
      let body = (settings, &device, &at);
      let method = "AddAndActivateConnection";
      let call = connection.call_method(Some(NM), NM_PATH, Some(NM), method, &body);
      if let Err(error) = shared.timeout(CALL_TIMEOUT, call).await {
        tracing::warn!(%error, method, "NetworkManager refused it");
      }
    });
  }

  /// Disconnects the device at `device`, until it is connected again.
  pub fn disconnect(&self, device: &str) {
    self.call(device, DEVICE, "Disconnect", ());
  }

  /// The connections NetworkManager saved, in its numbering, at most
  /// 256. NetworkManager is asked for them on the first call and
  /// followed from then on, so a config that never lists them reads
  /// none. Empty while it isn't running.
  pub fn saved_connections(&self) -> ReadSignal<Vec<SavedConnection>> {
    let saved = self.service.as_ref().and_then(|service| {
      let state = service.state.borrow();
      Some(Rc::clone(&state.as_ref()?.saved))
    });
    if let Some(saved) = saved {
      saved.follow();
    }
    self.saved_connections
  }

  /// Deletes the saved connection `uuid` names, so NetworkManager
  /// forgets that network. A refusal is logged.
  pub fn forget(&self, uuid: &str) {
    self.call_saved(uuid, "Delete");
  }

  /// Takes the secrets, such as a Wi-Fi password, out of the saved
  /// connection `uuid` names, so NetworkManager asks for them next time.
  /// A refusal is logged.
  pub fn clear_secrets(&self, uuid: &str) {
    self.call_saved(uuid, "ClearSecrets");
  }

  /// Calls `method` on the saved connection `uuid` names, and logs a
  /// refusal.
  fn call_saved(&self, uuid: &str, method: &'static str) {
    let Some((shared, connection)) = self.bus(method) else {
      return;
    };
    let (spawner, uuid) = (Rc::clone(&shared), uuid.to_owned());
    spawner.spawn(async move {
      let called = async {
        let find = "GetConnectionByUuid";
        let call = connection.call_method(Some(NM), SETTINGS_PATH, Some(SETTINGS), find, &uuid);
        let reply = shared.timeout(CALL_TIMEOUT, call).await?;
        let path = reply.body().deserialize::<OwnedObjectPath>()?;
        let call = connection.call_method(Some(NM), path.as_str(), Some(SAVED), method, &());
        shared.timeout(CALL_TIMEOUT, call).await.map(drop)
      };
      if let Err(error) = called.await {
        tracing::warn!(%error, uuid, method, "NetworkManager refused it");
      }
    });
  }

  /// Calls `method` on NetworkManager's object at `path`, and logs a
  /// refusal, without the body.
  fn call<B>(&self, path: &str, interface: &'static str, method: &'static str, body: B)
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let Some((shared, connection)) = self.bus(method) else {
      return;
    };
    let (spawner, path) = (Rc::clone(&shared), path.to_owned());
    spawner.spawn(async move {
      let call = connection.call_method(Some(NM), path.as_str(), Some(interface), method, &body);
      if let Err(error) = shared.timeout(CALL_TIMEOUT, call).await {
        tracing::warn!(%error, path, method, "NetworkManager refused it");
      }
    });
  }

  /// What a call to `method` goes through, or `None`, which is logged,
  /// while NetworkManager isn't running.
  fn bus(&self, method: &str) -> Option<(Rc<Shared>, Connection)> {
    let service = self.service.as_ref()?;
    let state = service.state.borrow();
    let Some(connection) = state.as_ref().and_then(|state| state.connection.clone()) else {
      tracing::info!(method, "NetworkManager isn't running; nothing sent");
      return None;
    };
    Some((Rc::clone(&service.shared), connection))
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
      connectivity: cx.signal(Connectivity::Unknown).read_only(),
      devices: cx.signal(Vec::new()).read_only(),
      wifi_networks: cx.signal(Vec::new()).read_only(),
      saved_connections: cx.signal(Vec::new()).read_only(),
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
  connectivity: Signal<Connectivity>,
  devices: Signal<Vec<NetworkDevice>>,
  wifi_networks: Signal<Vec<WifiNetwork>>,
  /// The saved connections, followed apart once a config lists them.
  saved: Rc<SavedConnections>,
  /// NetworkManager's objects that this service reads, by path.
  objects: BTreeMap<String, Object>,
  /// The bus, while NetworkManager runs, for the methods.
  connection: Option<Connection>,
  generation: u64,
}

impl State {
  /// The handle a config gets.
  fn network(&self, service: &Rc<NetworkService>) -> Network {
    Network {
      service: Some(Rc::clone(service)),
      wifi_enabled: self.wifi_enabled.read_only(),
      connectivity: self.connectivity.read_only(),
      devices: self.devices.read_only(),
      wifi_networks: self.wifi_networks.read_only(),
      saved_connections: self.saved.list.read_only(),
    }
  }
}

/// The interfaces this service reads on one of NetworkManager's objects.
/// An object is kept while it has one other than `wireless` and `wired`.
#[derive(Default)]
struct Object {
  manager: Option<Props>,
  device: Option<Props>,
  wireless: Option<Props>,
  wired: Option<Props>,
  access_point: Option<Props>,
  active: Option<Props>,
  /// A device's last failure, which its `StateReason` forgets as it moves
  /// on.
  failure: Option<ConnectionFailReason>,
}

impl Object {
  /// Where `interface`'s properties go, or `None` for one this service
  /// doesn't read.
  fn slot(&mut self, interface: &str) -> Option<&mut Option<Props>> {
    match interface {
      NM => Some(&mut self.manager),
      DEVICE => Some(&mut self.device),
      WIRELESS => Some(&mut self.wireless),
      WIRED => Some(&mut self.wired),
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

  /// Notes a device's failure from its `StateReason`, and forgets it once
  /// the device tries again.
  fn note_failure(&mut self) {
    let Some(device) = &self.device else {
      return;
    };
    // `(uu)`, the state and the reason the device went into it.
    let reason = device.get("StateReason");
    let reason = reason.and_then(|reason| <(u32, u32)>::try_from(&**reason).ok());
    if let Some((FAILED, reason)) = reason {
      self.failure = Some(fail_reason(reason));
    }
    if let Some(40..=ACTIVATED) = dbus::get::<u32>(device, "State") {
      self.failure = None;
    }
  }
}

/// What NetworkManager's `NMDeviceStateReason` `reason` says about a
/// failure.
fn fail_reason(reason: u32) -> ConnectionFailReason {
  match reason {
    7 => ConnectionFailReason::NoSecrets,
    8 => ConnectionFailReason::WifiClientDisconnected,
    // The supplicant's settings failed, or the supplicant did.
    9 | 10 => ConnectionFailReason::WifiClientFailed,
    11 => ConnectionFailReason::WifiAuthTimeout,
    53 => ConnectionFailReason::WifiNetworkLost,
    _ => ConnectionFailReason::Unknown,
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

/// Gives the object at `path` `interfaces`. Past [`MAX_OBJECTS`] a new
/// one is left out; which are shown is decided as they are.
fn add(objects: &mut BTreeMap<String, Object>, path: String, interfaces: HashMap<String, Props>) {
  if objects.len() >= MAX_OBJECTS && !objects.contains_key(&path) {
    return tracing::debug!(path, "too many network objects; leaving one out");
  }
  let mut object = objects.remove(&path).unwrap_or_default();
  for (interface, props) in interfaces {
    if let Some(slot) = object.slot(&interface) {
      *slot = Some(props);
    }
  }
  object.note_failure();
  if object.kept() {
    objects.insert(path, object);
  }
}

/// How the access point `point` is secured, from the strongest key
/// management it offers: RSN's before WPA's, a password before 802.1X,
/// as one will do, and SAE before a WPA2 password. Enhanced Open comes
/// after those, and then WEP, whose privacy bit is all that marks it.
fn security(point: &Props) -> WifiSecurity {
  let flags = |name| dbus::get::<u32>(point, name).unwrap_or(0);
  let (wpa, rsn) = (flags("WpaFlags"), flags("RsnFlags"));
  let strongest_first = [
    (rsn, KEY_MGMT_SUITE_B_192, WifiSecurity::Wpa3SuiteB192),
    (rsn, KEY_MGMT_SAE, WifiSecurity::Sae),
    (rsn, KEY_MGMT_PSK, WifiSecurity::Wpa2Psk),
    (rsn, KEY_MGMT_802_1X, WifiSecurity::Wpa2Eap),
    (wpa, KEY_MGMT_PSK, WifiSecurity::WpaPsk),
    (wpa, KEY_MGMT_802_1X, WifiSecurity::WpaEap),
    (rsn, KEY_MGMT_OWE, WifiSecurity::Owe),
  ];
  let offered = strongest_first
    .into_iter()
    .find(|(flags, key, _)| flags & key != 0);
  match offered {
    Some((_, _, security)) => security,
    None if wpa != 0 || rsn != 0 => WifiSecurity::Unknown,
    None if flags("Flags") & PRIVACY != 0 => WifiSecurity::Wep,
    None => WifiSecurity::Open,
  }
}

/// Whether a network secured as `security` asks for a key. Enhanced
/// Open encrypts without one.
fn secured(security: WifiSecurity) -> bool {
  !matches!(
    security,
    WifiSecurity::Open | WifiSecurity::Owe | WifiSecurity::Unknown
  )
}

/// The properties this service reads, by interface: a change to any
/// other, such as an access point's `LastSeen` on each scan, shows
/// nothing new.
fn read(interface: &str, name: &str) -> bool {
  match interface {
    NM => matches!(name, "WirelessEnabled" | "Connectivity"),
    DEVICE => matches!(
      name,
      "Interface" | "DeviceType" | "State" | "StateReason" | "ActiveConnection" | "InterfaceFlags"
    ),
    WIRELESS => matches!(name, "AccessPoints" | "ActiveAccessPoint" | "Mode"),
    WIRED => matches!(name, "Speed" | "Carrier"),
    ACCESS_POINT => matches!(
      name,
      "Ssid" | "Strength" | "Flags" | "WpaFlags" | "RsnFlags"
    ),
    ACTIVE => name == "Id",
    _ => false,
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
      let security = self::security(point);
      let network = WifiNetwork {
        ssid: String::from_utf8_lossy(&ssid).into_owned(),
        strength: dbus::get::<u8>(point, "Strength").unwrap_or(0).min(100),
        secured: secured(security),
        security,
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
  networks.truncate(MAX_ACCESS_POINTS);
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
      return state.network(self);
    }
    let root = &self.shared.root;
    let state = State {
      wifi_enabled: root.signal(false),
      connectivity: root.signal(Connectivity::Unknown),
      devices: root.signal(Vec::new()),
      wifi_networks: root.signal(Vec::new()),
      saved: SavedConnections::new(Rc::clone(&self.shared)),
      objects: BTreeMap::new(),
      connection: None,
      generation: 0,
    };
    let network = state.network(self);
    *self.state.borrow_mut() = Some(state);
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so there is no networking"),
    }
    network
  }

  /// Shows Wi-Fi, connectivity, the devices and the networks as they are
  /// now. Each is set only if it changed, since a scan changes much that
  /// none shows.
  fn publish(&self) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let objects = &state.objects;
      let manager = objects
        .get(NM_PATH)
        .and_then(|object| object.manager.as_ref());
      let enabled = manager.and_then(|props| dbus::get(props, "WirelessEnabled"));
      let connectivity = match manager.and_then(|props| dbus::get::<u32>(props, "Connectivity")) {
        Some(1) => Connectivity::None,
        Some(2) => Connectivity::Portal,
        Some(3) => Connectivity::Limited,
        Some(4) => Connectivity::Full,
        _ => Connectivity::Unknown,
      };
      let mut devices = objects
        .iter()
        .filter_map(|(path, object)| {
          let props = object.device.as_ref()?;
          let kind = dbus::get::<u32>(props, "DeviceType");
          let state = dbus::get::<u32>(props, "State");
          if kind == Some(LOOPBACK) || state == Some(UNMANAGED) {
            return None;
          }
          NetworkDevice::from_props(path.clone(), object, objects)
        })
        .collect::<Vec<_>>();
      devices.sort_by(|a, b| by_number(&a.path, &b.path));
      devices.truncate(MAX_DEVICES);
      let shown = (
        enabled.unwrap_or(false),
        connectivity,
        devices,
        wifi_networks(objects),
      );
      let signals = (
        state.wifi_enabled,
        state.connectivity,
        state.devices,
        state.wifi_networks,
      );
      (signals, shown)
    });
    if let Some((signals, shown)) = shown {
      set_changed(signals.0, shown.0);
      set_changed(signals.1, shown.1);
      set_changed(signals.2, shown.2);
      set_changed(signals.3, shown.3);
    }
  }
}

impl Daemon for NetworkService {
  const NAME: &'static str = NM;

  /// Only what this service reads: NetworkManager also broadcasts IP and
  /// DHCP settings, statistics and saved connections, which would each
  /// wake the shell for nothing.
  fn rules() -> Vec<String> {
    let mut rules = vec![format!(
      "type='signal',sender='{NM}',path='{ROOT}',interface='{OBJECT_MANAGER}'"
    )];
    rules.extend(
      [NM, DEVICE, WIRELESS, WIRED, ACCESS_POINT, ACTIVE].map(|interface| {
        format!(
          "type='signal',sender='{NM}',interface='{PROPERTIES}',member='PropertiesChanged',\
         arg0='{interface}'"
        )
      }),
    );
    rules
  }

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
        let object = state
          .as_mut()
          .and_then(|state| state.objects.get_mut(&path));
        let Some(object) = object else {
          return;
        };
        let Some(props) = object.slot(&interface).and_then(Option::as_mut) else {
          return;
        };
        let shown = changed
          .keys()
          .chain(&dropped)
          .any(|name| read(&interface, name));
        dbus::apply(props, changed, dropped);
        object.note_failure();
        if !shown {
          return;
        }
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
