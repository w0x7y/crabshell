use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant, Variant};

use super::{
  ACCESS_POINT, ACTIVE, ConnectionFailReason, Connectivity, DEVICE, NM, NM_PATH, NetworkDevice,
  NetworkKind, NetworkState, WIRED, WIRELESS, WifiMode, WifiNetwork, WifiSecurity,
};
use crate::dbus::OBJECT_MANAGER;
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

/// Where NetworkManager's object manager is.
const ROOT: &str = "/org/freedesktop";
const LO: &str = "/org/freedesktop/NetworkManager/Devices/1";
const WLAN: &str = "/org/freedesktop/NetworkManager/Devices/2";
const ETH: &str = "/org/freedesktop/NetworkManager/Devices/3";
const BRIDGE: &str = "/org/freedesktop/NetworkManager/Devices/4";
/// A device whose path sorts before the others as text, but after them
/// by number.
const DOCK: &str = "/org/freedesktop/NetworkManager/Devices/10";
const LO_CONNECTION: &str = "/org/freedesktop/NetworkManager/ActiveConnection/1";
const HOME_CONNECTION: &str = "/org/freedesktop/NetworkManager/ActiveConnection/2";
const HOME: &str = "/org/freedesktop/NetworkManager/AccessPoint/1";
const HOME_FAR: &str = "/org/freedesktop/NetworkManager/AccessPoint/2";
const LIBRARY: &str = "/org/freedesktop/NetworkManager/AccessPoint/3";
const HIDDEN: &str = "/org/freedesktop/NetworkManager/AccessPoint/4";
const CAFE: &str = "/org/freedesktop/NetworkManager/AccessPoint/5";
const ATTIC: &str = "/org/freedesktop/NetworkManager/AccessPoint/6";

type Props = Vec<(&'static str, Variant<'static>)>;

fn online(bus: &PrivateBus) -> Config {
  Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

fn path(path: &str) -> Variant<'static> {
  Variant::from(ObjectPath::try_from(path.to_owned()).expect("a path"))
}

fn paths(paths: &[&str]) -> Variant<'static> {
  let paths = paths.iter().map(|at| ObjectPath::try_from(at.to_string()));
  Variant::from(paths.collect::<Result<Vec<_>, _>>().expect("paths"))
}

fn device(interface: &'static str, kind: u32, state: u32, active: &str) -> Props {
  vec![
    ("Interface", Variant::from(interface)),
    ("DeviceType", Variant::from(kind)),
    ("State", Variant::from(state)),
    ("StateReason", Variant::from((state, 0u32))),
    ("ActiveConnection", path(active)),
    ("Managed", Variant::from(state != 10)),
  ]
}

/// An access point with `Flags`, `WpaFlags` and `RsnFlags`.
fn access_point(ssid: &[u8], strength: u8, (flags, wpa, rsn): (u32, u32, u32)) -> Props {
  vec![
    ("Ssid", Variant::from(ssid.to_vec())),
    ("Strength", Variant::from(strength)),
    ("Flags", Variant::from(flags)),
    ("WpaFlags", Variant::from(wpa)),
    ("RsnFlags", Variant::from(rsn)),
    ("Frequency", Variant::from(2412u32)),
    ("LastSeen", Variant::from(1000i32)),
  ]
}

/// A cable's `Speed` in Mb/s and its `Carrier`.
fn wired(speed: u32, carrier: bool) -> Props {
  vec![
    ("Speed", Variant::from(speed)),
    ("Carrier", Variant::from(carrier)),
    ("HwAddress", Variant::from("52:54:00:12:34:56")),
  ]
}

/// WPA2 with a pre-shared key, as `RsnFlags` says it.
const WPA2: (u32, u32, u32) = (1, 0, 0x188);

/// NetworkManager with loopback, Wi-Fi on a home network, an unplugged
/// cable, an unmanaged bridge, and five access points in range, the way
/// it exports them, with objects of its own beside them.
fn fake_nm(shell: &mut Harness, bus: &PrivateBus) -> FakeService {
  let nm = FakeService::start(shell, bus, NM);
  nm.add(
    NM_PATH,
    NM,
    &[
      ("WirelessEnabled", Variant::from(true)),
      ("Connectivity", Variant::from(4u32)),
      ("Version", Variant::from("1.52.0")),
    ],
  );
  let settings = format!("{NM_PATH}/Settings");
  nm.add(&settings, "org.freedesktop.NetworkManager.Settings", &[]);
  nm.add(LO, DEVICE, &device("lo", 32, 100, LO_CONNECTION));
  nm.add(LO_CONNECTION, ACTIVE, &[("Id", Variant::from("lo"))]);
  nm.add(WLAN, DEVICE, &device("wlan0", 2, 100, HOME_CONNECTION));
  nm.add(
    WLAN,
    WIRELESS,
    &[
      (
        "AccessPoints",
        paths(&[HOME, HOME_FAR, LIBRARY, HIDDEN, CAFE]),
      ),
      ("ActiveAccessPoint", path(HOME)),
      ("Mode", Variant::from(2u32)),
      ("LastScan", Variant::from(1000i64)),
    ],
  );
  nm.add(HOME_CONNECTION, ACTIVE, &[("Id", Variant::from("home"))]);
  nm.add(ETH, DEVICE, &device("eth0", 1, 20, "/"));
  nm.add(ETH, WIRED, &wired(0, false));
  nm.add(BRIDGE, DEVICE, &device("virbr0", 13, 10, "/"));
  nm.add(HOME, ACCESS_POINT, &access_point(b"home", 70, WPA2));
  nm.add(HOME_FAR, ACCESS_POINT, &access_point(b"home", 40, WPA2));
  nm.add(
    LIBRARY,
    ACCESS_POINT,
    &access_point(b"library", 85, (0, 0, 0)),
  );
  // Hidden, its name's length in NUL bytes.
  nm.add(HIDDEN, ACCESS_POINT, &access_point(&[0; 6], 90, WPA2));
  // Latin-1, as an old router may name itself, and WEP.
  nm.add(CAFE, ACCESS_POINT, &access_point(b"caf\xe9", 55, (1, 0, 0)));
  nm.own(shell);
  nm
}

/// What `fake_nm`'s devices and networks show as.
fn wlan() -> NetworkDevice {
  NetworkDevice {
    path: WLAN.into(),
    interface: "wlan0".into(),
    kind: NetworkKind::Wifi,
    state: NetworkState::Connected,
    connection: "home".into(),
    fail_reason: None,
    wifi_mode: WifiMode::Station,
    link_speed: 0,
    has_link: false,
  }
}

fn eth() -> NetworkDevice {
  NetworkDevice {
    path: ETH.into(),
    interface: "eth0".into(),
    kind: NetworkKind::Ethernet,
    state: NetworkState::Unavailable,
    connection: "".into(),
    fail_reason: None,
    wifi_mode: WifiMode::Unknown,
    link_speed: 0,
    has_link: false,
  }
}

fn home() -> WifiNetwork {
  WifiNetwork {
    ssid: "home".into(),
    strength: 70,
    secured: true,
    security: WifiSecurity::Wpa2Psk,
    connected: true,
    device: WLAN.into(),
    access_point: HOME.into(),
  }
}

fn library() -> WifiNetwork {
  WifiNetwork {
    ssid: "library".into(),
    strength: 85,
    secured: false,
    security: WifiSecurity::Open,
    connected: false,
    access_point: LIBRARY.into(),
    ..home()
  }
}

fn cafe() -> WifiNetwork {
  WifiNetwork {
    ssid: "caf\u{fffd}".into(),
    strength: 55,
    security: WifiSecurity::Wep,
    connected: false,
    access_point: CAFE.into(),
    ..home()
  }
}

/// NetworkManager's `InterfacesAdded` for `at`, which says it now has
/// `interfaces`.
fn added(nm: &FakeService, shell: &mut Harness, at: &str, interfaces: Vec<(&str, Props)>) {
  let interfaces = interfaces
    .into_iter()
    .map(|(interface, props)| {
      let props = props.into_iter().collect::<HashMap<_, _>>();
      (interface.to_owned(), props)
    })
    .collect::<HashMap<_, _>>();
  let body = (
    ObjectPath::try_from(at.to_owned()).expect("a path"),
    interfaces,
  );
  nm.emit(shell, ROOT, OBJECT_MANAGER, "InterfacesAdded", body);
}

/// NetworkManager's `InterfacesRemoved` for `at`.
fn removed(nm: &FakeService, shell: &mut Harness, at: &str, interfaces: Vec<&'static str>) {
  let body = (
    ObjectPath::try_from(at.to_owned()).expect("a path"),
    interfaces,
  );
  nm.emit(shell, ROOT, OBJECT_MANAGER, "InterfacesRemoved", body);
}

/// A call's interface and member, such as
/// `org.freedesktop.NetworkManager.Device.Disconnect`.
fn named(call: &zbus::Message) -> String {
  let header = call.header();
  let interface = header
    .interface()
    .map_or("", |interface| interface.as_str());
  let member = header.member().map_or("", |member| member.as_str());
  format!("{interface}.{member}")
}

fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

#[test]
fn devices_show_without_loopback_or_unmanaged_ones() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let devices = network.devices();
  wait_for(&mut shell, "the devices", || {
    !devices.get_untracked().is_empty()
  });
  assert_eq!(devices.get_untracked(), [wlan(), eth()]);
  assert!(network.wifi_enabled().get_untracked());
  assert_eq!(network.connectivity().get_untracked(), Connectivity::Full);
}

#[test]
fn networks_are_one_per_ssid_strongest_first() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _nm = fake_nm(&mut shell, &bus);
  let networks = shell.cx.network().wifi_networks();
  wait_for(&mut shell, "the networks", || {
    !networks.get_untracked().is_empty()
  });
  assert_eq!(networks.get_untracked(), [library(), home(), cafe()]);
}

#[test]
fn objects_come_and_go_as_networkmanager_says() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let (devices, networks) = (network.devices(), network.wifi_networks());
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });

  let attic = access_point(b"attic", 30, WPA2);
  added(&nm, &mut shell, ATTIC, vec![(ACCESS_POINT, attic)]);
  let listed = paths(&[HOME, HOME_FAR, LIBRARY, HIDDEN, CAFE, ATTIC]);
  nm.set(&mut shell, WLAN, WIRELESS, &[("AccessPoints", listed)]);
  wait_for(&mut shell, "the attic", || {
    networks.get_untracked().len() == 4
  });
  assert_eq!(
    networks.get_untracked()[3].access_point,
    ATTIC,
    "the weakest"
  );

  let dock = device("enp0s13f0u1", 1, 30, "/");
  let wired = "org.freedesktop.NetworkManager.Device.Wired";
  added(
    &nm,
    &mut shell,
    DOCK,
    vec![(DEVICE, dock), (wired, Vec::new())],
  );
  wait_for(&mut shell, "the dock", || {
    devices.get_untracked().len() == 3
  });
  let last = devices.get_untracked()[2].clone();
  assert_eq!(last.path, DOCK, "in NetworkManager's numbering");
  assert_eq!(last.state, NetworkState::Disconnected);

  removed(&nm, &mut shell, LIBRARY, vec![ACCESS_POINT]);
  wait_for(&mut shell, "no library", || {
    networks.get_untracked().len() == 3
  });
  assert_eq!(networks.get_untracked()[0], home(), "the rest stays");
  removed(&nm, &mut shell, DOCK, vec![DEVICE, wired]);
  wait_for(&mut shell, "no dock", || {
    devices.get_untracked() == [wlan(), eth()]
  });
}

#[test]
fn property_changes_show() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let (enabled, devices, networks) = (
    network.wifi_enabled(),
    network.devices(),
    network.wifi_networks(),
  );
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });

  nm.set(
    &mut shell,
    NM_PATH,
    NM,
    &[("WirelessEnabled", Variant::from(false))],
  );
  wait_for(&mut shell, "Wi-Fi off", || !enabled.get_untracked());
  let portal = [("Connectivity", Variant::from(2u32))];
  nm.set(&mut shell, NM_PATH, NM, &portal);
  let connectivity = network.connectivity();
  wait_for(&mut shell, "a captive portal", || {
    connectivity.get_untracked() == Connectivity::Portal
  });

  nm.set(
    &mut shell,
    LIBRARY,
    ACCESS_POINT,
    &[("Strength", Variant::from(60u8))],
  );
  wait_for(&mut shell, "a weaker library", || {
    networks.get_untracked()[1].strength == 60
  });
  assert_eq!(networks.get_untracked()[0], home(), "now the strongest");

  nm.set(&mut shell, WLAN, DEVICE, &[("State", Variant::from(70u32))]);
  wait_for(&mut shell, "connecting", || {
    devices.get_untracked()[0].state == NetworkState::Connecting
  });
  assert!(!networks.get_untracked()[0].connected, "not yet");

  nm.set(
    &mut shell,
    HOME_CONNECTION,
    ACTIVE,
    &[("Id", Variant::from("home 2"))],
  );
  wait_for(&mut shell, "the new name", || {
    devices.get_untracked()[0].connection == "home 2"
  });

  let hotspot = [("Mode", Variant::from(3u32))];
  nm.set(&mut shell, WLAN, WIRELESS, &hotspot);
  wait_for(&mut shell, "a hotspot", || {
    devices.get_untracked()[0].wifi_mode == WifiMode::AccessPoint
  });
  let plugged = [
    ("Carrier", Variant::from(true)),
    ("Speed", Variant::from(1000u32)),
  ];
  nm.set(&mut shell, ETH, WIRED, &plugged);
  wait_for(&mut shell, "a cable in", || {
    devices.get_untracked()[1].has_link
  });
  assert_eq!(devices.get_untracked()[1].link_speed, 1000);

  nm.set(
    &mut shell,
    WLAN,
    DEVICE,
    &[("State", Variant::from(110u32))],
  );
  wait_for(&mut shell, "disconnecting", || {
    devices.get_untracked()[0].state == NetworkState::Disconnecting
  });

  nm.set(
    &mut shell,
    WLAN,
    DEVICE,
    &[
      ("State", Variant::from(30u32)),
      ("ActiveConnection", path("/")),
    ],
  );
  wait_for(&mut shell, "disconnected", || {
    devices.get_untracked()[0].state == NetworkState::Disconnected
  });
  assert_eq!(devices.get_untracked()[0].connection, "");
  let eth = NetworkDevice {
    link_speed: 1000,
    has_link: true,
    ..eth()
  };
  assert_eq!(devices.get_untracked()[1], eth, "the rest stays");
}

#[test]
fn a_change_nothing_shows_sets_nothing() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let (enabled, devices, networks) = (
    network.wifi_enabled(),
    network.devices(),
    network.wifi_networks(),
  );
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });
  let runs = Rc::new(Cell::new(0));
  let counted = Rc::clone(&runs);
  shell.cx.effect(move || {
    enabled.with(|_| ());
    devices.with(|_| ());
    networks.with(|_| ());
    counted.set(counted.get() + 1);
  });
  shell.run_until(Instant::now() + Duration::from_millis(50));
  let ran = runs.get();

  // What a scan changes, and what nothing here reads.
  let seen = [("LastSeen", Variant::from(2000i32))];
  nm.set(&mut shell, HOME, ACCESS_POINT, &seen);
  let weaker = [("Strength", Variant::from(45u8))];
  nm.set(&mut shell, HOME_FAR, ACCESS_POINT, &weaker);
  let scanned = [("LastScan", Variant::from(2000i64))];
  nm.set(&mut shell, WLAN, WIRELESS, &scanned);
  let version = [("Version", Variant::from("1.52.1"))];
  nm.set(&mut shell, NM_PATH, NM, &version);
  let address = [("HwAddress", Variant::from("52:54:00:12:34:57"))];
  nm.set(&mut shell, ETH, WIRED, &address);
  // Then one that shows, sent after them, so they have arrived.
  let fainter = [("Strength", Variant::from(50u8))];
  nm.set(&mut shell, CAFE, ACCESS_POINT, &fainter);
  wait_for(&mut shell, "a fainter café", || {
    networks.get_untracked()[2].strength == 50
  });
  assert_eq!(runs.get(), ran + 1, "only the café's change ran it");
}

#[test]
fn methods_reach_networkmanager_and_the_password_is_never_logged() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let bodies = Rc::new(RefCell::new(Vec::new()));
  let sink = Rc::clone(&bodies);
  nm.on("RequestScan", move |call| {
    let options: HashMap<String, OwnedVariant> = call.body().deserialize().ok()?;
    let signature = call.body().signature().to_string();
    let named = named(call);
    sink
      .borrow_mut()
      .push(format!("{named} {signature} {}", options.len()));
    reply(call, &())
  });
  let sink = Rc::clone(&bodies);
  nm.on("Disconnect", move |call| {
    sink.borrow_mut().push(named(call));
    reply(call, &())
  });
  let sink = Rc::clone(&bodies);
  nm.on("ActivateConnection", move |call| {
    let (connection, device, at): (OwnedObjectPath, OwnedObjectPath, OwnedObjectPath) =
      call.body().deserialize().ok()?;
    let named = named(call);
    sink
      .borrow_mut()
      .push(format!("{named} {connection} {device} {at}"));
    reply(call, &ObjectPath::try_from(HOME_CONNECTION).ok()?)
  });
  let sink = Rc::clone(&bodies);
  nm.on("AddAndActivateConnection", move |call| {
    type Settings = HashMap<String, HashMap<String, OwnedVariant>>;
    let (settings, device, at): (Settings, OwnedObjectPath, OwnedObjectPath) =
      call.body().deserialize().ok()?;
    let wireless = &settings["802-11-wireless"];
    let security = settings.get("802-11-wireless-security")?;
    let ssid = Vec::<u8>::try_from(wireless["ssid"].try_clone().ok()?).ok()?;
    let key = |name: &str| {
      <&str>::try_from(security.get(name)?)
        .ok()
        .map(str::to_owned)
    };
    sink.borrow_mut().push(format!(
      "{} {ssid:?} {:?} {:?} {device} {at}",
      named(call),
      key("key-mgmt"),
      key("psk")?
    ));
    let name = "org.freedesktop.NetworkManager.Settings.Connection.InvalidProperty";
    let error = zbus::Message::error(&call.header(), name);
    Some(error.build(&("802-11-wireless-security.psk: property is invalid",)))
  });
  let network = shell.cx.network();
  let (enabled, networks) = (network.wifi_enabled(), network.wifi_networks());
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });

  let logs = capture_logs(|| {
    network.set_wifi_enabled(false);
    network.scan(WLAN);
    network.connect(&home(), None);
    network.connect(&networks.get_untracked()[2], Some("hunter22"));
    network.disconnect(WLAN);
    let calls = Rc::clone(&nm.calls);
    wait_for(&mut shell, "every call", || calls.borrow().len() == 5);
    wait_for(&mut shell, "Wi-Fi off", || !enabled.get_untracked());
    // The refusal's reply, after the call.
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  let mut calls = nm.calls.borrow().clone();
  calls.sort();
  assert_eq!(
    calls,
    [
      format!("ActivateConnection {NM_PATH}"),
      format!("AddAndActivateConnection {NM_PATH}"),
      format!("Disconnect {WLAN}"),
      format!("RequestScan {WLAN}"),
      format!("Set {NM_PATH} WirelessEnabled"),
    ]
  );
  let mut bodies = bodies.borrow().clone();
  bodies.sort();
  assert_eq!(
    bodies,
    [
      format!("{NM}.ActivateConnection / {WLAN} {HOME}"),
      format!("{NM}.AddAndActivateConnection [99, 97, 102, 233] None \"hunter22\" {WLAN} {CAFE}"),
      format!("{DEVICE}.Disconnect"),
      format!("{WIRELESS}.RequestScan a{{sv}} 0"),
    ],
    "the SSID's own bytes, not the shown text's"
  );
  assert!(logs.contains("InvalidProperty"), "{logs}");
  assert_eq!(logs.matches("refused").count(), 1, "{logs}");
  assert!(!logs.contains("hunter22"), "{logs}");
}

#[test]
fn networkmanager_leaving_empties_everything_and_coming_back_fills_it() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let (enabled, devices, networks) = (
    network.wifi_enabled(),
    network.devices(),
    network.wifi_networks(),
  );
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });

  let connectivity = network.connectivity();
  nm.leave(&mut shell);
  wait_for(&mut shell, "nothing", || {
    devices.get_untracked().is_empty()
      && networks.get_untracked().is_empty()
      && !enabled.get_untracked()
      && connectivity.get_untracked() == Connectivity::Unknown
  });

  let _again = fake_nm(&mut shell, &bus);
  wait_for(&mut shell, "everything again", || {
    devices.get_untracked() == [wlan(), eth()]
      && networks.get_untracked() == [library(), home(), cafe()]
      && enabled.get_untracked()
      && connectivity.get_untracked() == Connectivity::Full
  });
}

#[test]
fn signals_from_another_program_are_ignored() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _nm = fake_nm(&mut shell, &bus);
  let network = shell.cx.network();
  let (enabled, networks) = (network.wifi_enabled(), network.wifi_networks());
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });
  // Not NetworkManager, but saying Wi-Fi went off and an access point
  // came, aimed at every connection on the bus, the shell's too, which no
  // match rule filters.
  let impostor = shell.block_on(bus.connect(&[]));
  shell
    .block_on(async move {
      let call = impostor.call_method(
        Some(crate::dbus::BUS),
        crate::dbus::BUS_PATH,
        Some(crate::dbus::BUS),
        "ListNames",
        &(),
      );
      let names: Vec<String> = call.await?.body().deserialize()?;
      let off = crate::dbus::Props::from([(
        "WirelessEnabled".to_owned(),
        Variant::from(false).try_to_owned()?,
      )]);
      let strong = crate::dbus::Props::from([
        (
          "Ssid".to_owned(),
          Variant::from(b"evil".to_vec()).try_to_owned()?,
        ),
        ("Strength".to_owned(), Variant::from(100u8).try_to_owned()?),
      ]);
      let added = (
        ObjectPath::try_from(ATTIC)?,
        HashMap::from([(ACCESS_POINT, strong)]),
      );
      for name in names.iter().filter(|name| name.starts_with(':')) {
        let destination = Some(name.as_str());
        let changed = (NM, &off, Vec::<String>::new());
        let properties = crate::dbus::PROPERTIES;
        impostor
          .emit_signal(
            destination,
            NM_PATH,
            properties,
            "PropertiesChanged",
            &changed,
          )
          .await?;
        impostor
          .emit_signal(destination, ROOT, OBJECT_MANAGER, "InterfacesAdded", &added)
          .await?;
      }
      zbus::Result::Ok(())
    })
    .expect("the impostor's signals");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert!(enabled.get_untracked());
  assert_eq!(networks.get_untracked(), [library(), home(), cafe()]);
}

#[test]
fn wrong_types_read_as_defaults_and_ssids_are_cut_to_32_bytes() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  nm.add(
    NM_PATH,
    NM,
    &[
      ("WirelessEnabled", Variant::from("yes")),
      ("Connectivity", Variant::from("full")),
    ],
  );
  nm.add(
    ETH,
    DEVICE,
    &[
      ("Interface", Variant::from(7u32)),
      ("DeviceType", Variant::from("ethernet")),
      ("State", Variant::from("up")),
      ("StateReason", Variant::from((120u32, "no-secrets"))),
      ("ActiveConnection", Variant::from(HOME_CONNECTION)),
    ],
  );
  let wired = [
    ("Speed", Variant::from(-1i32)),
    ("Carrier", Variant::from(1u32)),
  ];
  nm.add(ETH, WIRED, &wired);
  nm.add(
    HOME,
    ACCESS_POINT,
    &[
      ("Ssid", Variant::from("home")),
      ("Strength", Variant::from(99u8)),
    ],
  );
  let mut long = access_point(&[b'x'; 40], 85, (0, 0, 0));
  long[1].1 = Variant::from(85u32);
  nm.add(LIBRARY, ACCESS_POINT, &long);
  nm.add(
    CAFE,
    ACCESS_POINT,
    &access_point(b"caf\xe9", 150, (1, 0, 0)),
  );
  let network = shell.cx.network();
  let (enabled, devices, networks) = (
    network.wifi_enabled(),
    network.devices(),
    network.wifi_networks(),
  );
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });
  assert!(!enabled.get_untracked());
  assert_eq!(
    network.connectivity().get_untracked(),
    Connectivity::Unknown
  );
  let expected = NetworkDevice {
    interface: "".into(),
    kind: NetworkKind::Other,
    state: NetworkState::Unavailable,
    connection: "".into(),
    ..eth()
  };
  assert_eq!(devices.get_untracked(), [wlan(), expected]);
  let home = WifiNetwork {
    strength: 40,
    connected: false,
    access_point: HOME_FAR.into(),
    ..home()
  };
  let library = WifiNetwork {
    ssid: "x".repeat(32),
    strength: 0,
    ..library()
  };
  let cafe = WifiNetwork {
    strength: 100,
    ..cafe()
  };
  assert_eq!(networks.get_untracked(), [cafe, home, library]);
}

#[test]
fn at_most_64_devices_connections_and_512_access_points_are_kept() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = FakeService::start(&mut shell, &bus, NM);
  let point = |n: usize| format!("{NM_PATH}/AccessPoint/{n}");
  let points = (1..=600).map(point).collect::<Vec<_>>();
  let listed = points.iter().map(String::as_str).collect::<Vec<_>>();
  for n in 1..=70 {
    let at = format!("{NM_PATH}/Devices/{n}");
    // Each on the connection numbered 6 after it.
    let active = format!("{NM_PATH}/ActiveConnection/{}", n + 6);
    nm.add(&at, DEVICE, &device("wlan", 2, 100, &active));
    let active = format!("{NM_PATH}/ActiveConnection/{n}");
    nm.add(&active, ACTIVE, &[("Id", Variant::from(format!("c{n}")))]);
  }
  nm.add(WLAN, WIRELESS, &[("AccessPoints", paths(&listed))]);
  for (n, at) in points.iter().enumerate() {
    let ssid = format!("ap{n:03}");
    nm.add(at, ACCESS_POINT, &access_point(ssid.as_bytes(), 50, WPA2));
  }
  nm.own(&mut shell);
  let network = shell.cx.network();
  let (devices, networks) = (network.devices(), network.wifi_networks());
  wait_for(&mut shell, "the networks", || {
    !networks.get_untracked().is_empty()
  });
  let devices = devices.get_untracked();
  assert_eq!(devices.len(), 64);
  assert_eq!(
    devices[63].path,
    format!("{NM_PATH}/Devices/64"),
    "the first by number"
  );
  assert_eq!(devices[57].connection, "c64");
  assert_eq!(
    devices[58].connection, "c65",
    "connections aren't capped apart"
  );
  let shown = |networks: &[WifiNetwork], n| networks.iter().any(|w| w.access_point == point(n));
  assert_eq!(networks.get_untracked().len(), 512);
  assert!(shown(&networks.get_untracked(), 512));
  assert!(!shown(&networks.get_untracked(), 513));

  let extra = access_point(b"extra", 100, WPA2);
  added(&nm, &mut shell, &point(601), vec![(ACCESS_POINT, extra)]);
  let mut more = listed.clone();
  let extra = point(601);
  more.push(&extra);
  nm.set(
    &mut shell,
    WLAN,
    WIRELESS,
    &[("AccessPoints", paths(&more))],
  );
  // The strongest shows past the cap, and the faintest makes room.
  wait_for(&mut shell, "the extra, strongest", || {
    networks
      .get_untracked()
      .first()
      .map(|w| w.access_point.clone())
      == Some(point(601))
  });
  assert_eq!(networks.get_untracked().len(), 512);
}

#[test]
fn an_idle_networkmanager_wakes_nothing_and_none_at_all_is_quiet() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _nm = fake_nm(&mut shell, &bus);
  let networks = shell.cx.network().wifi_networks();
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  // A system bus with no NetworkManager on it.
  let quiet = PrivateBus::start();
  let mut shell = Harness::new(online(&quiet));
  let devices = shell.cx.network().devices();
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(devices.get_untracked(), []);
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let network = shell.cx.network();
    network.disconnect(WLAN);
    assert_eq!(
      shell.run_until(Instant::now() + Duration::from_millis(200)),
      1
    );
    assert_eq!(network.devices().get_untracked(), []);
  });
  assert!(logs.contains("no system bus"), "{logs}");
}

#[test]
fn only_what_networking_reads_is_asked_of_the_bus() {
  use crate::dbus::Daemon;

  let rules = super::NetworkService::rules();
  assert_eq!(rules.len(), 7, "{rules:?}");
  assert!(
    rules
      .iter()
      .all(|rule| rule.contains(&format!("sender='{NM}'")))
  );
  assert!(rules[0].contains(&format!("interface='{OBJECT_MANAGER}'")));
  let read = [NM, DEVICE, WIRELESS, WIRED, ACCESS_POINT, ACTIVE];
  for (rule, interface) in rules[1..].iter().zip(read) {
    assert!(rule.contains(&format!("arg0='{interface}'")), "{rule}");
  }
}

#[test]
fn an_open_network_with_no_saved_connection_gets_a_new_one() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  nm.on("ActivateConnection", |call| {
    let name = "org.freedesktop.NetworkManager.UnknownConnection";
    Some(zbus::Message::error(&call.header(), name).build(&("no connection",)))
  });
  let added = Rc::new(RefCell::new(None));
  let sink = Rc::clone(&added);
  nm.on("AddAndActivateConnection", move |call| {
    type Settings = HashMap<String, HashMap<String, OwnedVariant>>;
    let (settings, _, _): (Settings, OwnedObjectPath, OwnedObjectPath) =
      call.body().deserialize().ok()?;
    let mut groups: Vec<String> = settings.keys().cloned().collect();
    groups.sort();
    *sink.borrow_mut() = Some(groups);
    let made = (
      ObjectPath::try_from(HOME_CONNECTION).ok()?,
      ObjectPath::try_from(HOME_CONNECTION).ok()?,
    );
    reply(call, &made)
  });
  let network = shell.cx.network();
  let networks = network.wifi_networks();
  wait_for(&mut shell, "the networks", || {
    networks.get_untracked().len() == 3
  });
  network.connect(&library(), None);
  wait_for(&mut shell, "a new connection", || added.borrow().is_some());
  assert_eq!(
    added.borrow().clone(),
    Some(vec!["802-11-wireless".to_owned()])
  );

  // A secured one with no saved connection needs its password.
  let logs = capture_logs(|| {
    network.connect(&home(), None);
    shell.run_until(Instant::now() + Duration::from_millis(300));
  });
  assert!(logs.contains("no saved connection"), "{logs}");
}

/// An access point's properties as the service keeps them, from
/// `Flags`, `WpaFlags` and `RsnFlags`.
fn point_props(flags: (u32, u32, u32)) -> crate::dbus::Props {
  let point = access_point(b"ap", 60, flags);
  let owned = point
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.try_to_owned()));
  owned.map(|(k, v)| (k, v.expect("owned"))).collect()
}

#[test]
fn security_comes_from_the_strongest_key_management() {
  use WifiSecurity::*;

  let flags = [
    ((0, 0, 0), Open, false),
    ((1, 0, 0), Wep, true),
    // TKIP and PSK on WPA.
    ((1, 0x144, 0), WpaPsk, true),
    ((1, 0x244, 0), WpaEap, true),
    // WPA and WPA2 mixed reads as WPA2.
    ((1, 0x144, 0x188), Wpa2Psk, true),
    ((1, 0, 0x288), Wpa2Eap, true),
    // PSK beside 802.1X: a password will do.
    ((1, 0, 0x388), Wpa2Psk, true),
    ((1, 0, 0x488), Sae, true),
    // WPA2 and WPA3 transition: SAE before PSK.
    ((1, 0, 0x588), Sae, true),
    ((1, 0, 0x2288), Wpa3SuiteB192, true),
    // Enhanced Open, and the open side of its transition mode.
    ((1, 0, 0x888), Owe, false),
    ((0, 0, 0x1000), Owe, false),
    // RSN with no key management known.
    ((1, 0, 0x88), Unknown, false),
  ];
  for (flags, security, secured) in flags {
    let props = point_props(flags);
    assert_eq!(super::security(&props), security, "{flags:x?}");
    assert_eq!(super::secured(security), secured, "{security:?}");
  }
}

#[test]
fn failure_reasons_follow_networkmanagers_numbers() {
  use ConnectionFailReason::*;

  let reasons = [
    (7, NoSecrets),
    (8, WifiClientDisconnected),
    (9, WifiClientFailed),
    (10, WifiClientFailed),
    (11, WifiAuthTimeout),
    (53, WifiNetworkLost),
    // A DHCP failure, and none given.
    (17, Unknown),
    (0, Unknown),
  ];
  for (reason, expected) in reasons {
    assert_eq!(super::fail_reason(reason), expected, "{reason}");
  }
}

#[test]
fn a_failure_shows_until_the_next_try() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let nm = fake_nm(&mut shell, &bus);
  let devices = shell.cx.network().devices();
  wait_for(&mut shell, "the devices", || {
    !devices.get_untracked().is_empty()
  });
  let state = |state: u32, reason: u32| {
    [
      ("State", Variant::from(state)),
      ("StateReason", Variant::from((state, reason))),
    ]
  };
  let wlan = || devices.get_untracked()[0].clone();

  // No password: NetworkManager asks for secrets nobody gives.
  nm.set(&mut shell, WLAN, DEVICE, &state(120, 7));
  wait_for(&mut shell, "the failure", || {
    wlan().fail_reason == Some(ConnectionFailReason::NoSecrets)
  });
  assert_eq!(wlan().state, NetworkState::Disconnected);
  // It moves on to disconnected with no reason, and the failure stays.
  nm.set(&mut shell, WLAN, DEVICE, &state(30, 0));
  nm.set(&mut shell, WLAN, WIRELESS, &[("Mode", Variant::from(0u32))]);
  wait_for(&mut shell, "the later change", || {
    wlan().wifi_mode == WifiMode::Unknown
  });
  assert_eq!(wlan().fail_reason, Some(ConnectionFailReason::NoSecrets));

  nm.set(&mut shell, WLAN, DEVICE, &state(40, 0));
  wait_for(&mut shell, "the next try", || {
    wlan().state == NetworkState::Connecting
  });
  assert_eq!(wlan().fail_reason, None);

  nm.set(&mut shell, WLAN, DEVICE, &state(120, 53));
  wait_for(&mut shell, "a network gone", || {
    wlan().fail_reason == Some(ConnectionFailReason::WifiNetworkLost)
  });
  assert_eq!(devices.get_untracked()[1].fail_reason, None, "only its own");
}
