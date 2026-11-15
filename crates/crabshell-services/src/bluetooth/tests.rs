use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use zbus::{ObjectPath, Variant};

use super::{
  ADAPTER, BATTERY, BLUEZ, BluetoothAdapter, BluetoothAdapterState, BluetoothDevice,
  BluetoothDeviceState, DEVICE,
};
use crate::dbus::OBJECT_MANAGER;
use crate::testing::{
  FakeService, Harness, PrivateBus, capture_logs, capture_logs_from, offline, reply,
};
use crate::{Config, ServicesExt};

const HCI0: &str = "/org/bluez/hci0";
const HEADSET: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";
const MOUSE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
/// A device whose path sorts before the others.
const KEYBOARD: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";

type Props = Vec<(&'static str, Variant<'static>)>;

fn online(bus: &PrivateBus) -> Config {
  Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

fn path(path: &'static str) -> Variant<'static> {
  Variant::from(ObjectPath::try_from(path).expect("a path"))
}

/// A device on hci0, paired and not connected.
fn device(address: &'static str, alias: Option<&'static str>, name: Option<&'static str>) -> Props {
  let mut props = vec![
    ("Address", Variant::from(address)),
    ("Icon", Variant::from("input-mouse")),
    ("Adapter", path(HCI0)),
    ("Connected", Variant::from(false)),
    ("Paired", Variant::from(true)),
    ("Trusted", Variant::from(false)),
    ("RSSI", Variant::from(-60i16)),
  ];
  props.extend(alias.map(|alias| ("Alias", Variant::from(alias))));
  props.extend(name.map(|name| ("Name", Variant::from(name))));
  props
}

/// BlueZ with one adapter, a headset with a battery and a mouse, the way
/// bluetoothd exports them, with objects of its own beside them.
fn fake_bluez(shell: &mut Harness, bus: &PrivateBus) -> FakeService {
  let bluez = FakeService::start(shell, bus, BLUEZ);
  bluez.add("/org/bluez", "org.bluez.AgentManager1", &[]);
  bluez.add(
    HCI0,
    ADAPTER,
    &[
      ("Address", Variant::from("00:1A:7D:DA:71:13")),
      ("Alias", Variant::from("laptop")),
      ("Name", Variant::from("BlueZ 5.79")),
      ("Powered", Variant::from(true)),
      ("Discovering", Variant::from(false)),
    ],
  );
  let headset = device("AA:BB:CC:DD:EE:01", Some("headset"), Some("WH-1000XM4"));
  bluez.add(HEADSET, DEVICE, &headset);
  bluez.add(HEADSET, BATTERY, &[("Percentage", Variant::from(80u8))]);
  let service = format!("{HEADSET}/service0001");
  bluez.add(&service, "org.bluez.GattService1", &[]);
  bluez.add(
    MOUSE,
    DEVICE,
    &device("AA:BB:CC:DD:EE:02", None, Some("mouse")),
  );
  bluez.own(shell);
  bluez
}

/// What `fake_bluez`'s devices show as.
fn headset() -> BluetoothDevice {
  BluetoothDevice {
    path: HEADSET.into(),
    adapter: HCI0.into(),
    address: "AA:BB:CC:DD:EE:01".into(),
    name: "headset".into(),
    icon: "input-mouse".into(),
    connected: false,
    state: BluetoothDeviceState::Disconnected,
    paired: true,
    trusted: false,
    battery: Some(80),
  }
}

fn mouse() -> BluetoothDevice {
  BluetoothDevice {
    path: MOUSE.into(),
    address: "AA:BB:CC:DD:EE:02".into(),
    name: "mouse".into(),
    battery: None,
    ..headset()
  }
}

/// BlueZ's `InterfacesAdded` for `path`, which says it now has
/// `interfaces`.
fn added(
  bluez: &FakeService,
  shell: &mut Harness,
  at: &'static str,
  interfaces: Vec<(&'static str, Props)>,
) {
  let interfaces = interfaces
    .into_iter()
    .map(|(interface, props)| (interface, props.into_iter().collect::<HashMap<_, _>>()))
    .collect::<HashMap<_, _>>();
  let body = (ObjectPath::try_from(at).expect("a path"), interfaces);
  bluez.emit(shell, "/", OBJECT_MANAGER, "InterfacesAdded", body);
}

/// BlueZ's `InterfacesRemoved` for `path`.
fn removed(
  bluez: &FakeService,
  shell: &mut Harness,
  at: &'static str,
  interfaces: Vec<&'static str>,
) {
  let body = (ObjectPath::try_from(at).expect("a path"), interfaces);
  bluez.emit(shell, "/", OBJECT_MANAGER, "InterfacesRemoved", body);
}

fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

#[test]
fn adapters_and_devices_show_in_path_order() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _bluez = fake_bluez(&mut shell, &bus);
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  assert_eq!(
    adapters.get_untracked(),
    [BluetoothAdapter {
      path: HCI0.into(),
      name: "laptop".into(),
      address: "00:1A:7D:DA:71:13".into(),
      powered: true,
      state: BluetoothAdapterState::Enabled,
      discovering: false,
    }]
  );
  assert_eq!(devices.get_untracked(), [headset(), mouse()]);
}

#[test]
fn devices_come_and_go_as_bluez_says() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  let devices = shell.cx.bluetooth().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  let keyboard = device("00:11:22:33:44:55", None, None);
  added(&bluez, &mut shell, KEYBOARD, vec![(DEVICE, keyboard)]);
  wait_for(&mut shell, "the keyboard", || {
    devices.get_untracked().len() == 3
  });
  let first = devices.get_untracked()[0].clone();
  assert_eq!(first.path, KEYBOARD, "in path order");
  assert_eq!(first.name, "00:11:22:33:44:55", "no alias or name");

  let battery = vec![("Percentage", Variant::from(40u8))];
  added(&bluez, &mut shell, MOUSE, vec![(BATTERY, battery)]);
  wait_for(&mut shell, "the mouse's battery", || {
    devices.get_untracked()[2].battery == Some(40)
  });
  removed(&bluez, &mut shell, MOUSE, vec![BATTERY]);
  wait_for(&mut shell, "no battery", || {
    devices.get_untracked()[2].battery.is_none()
  });
  assert_eq!(devices.get_untracked()[2].name, "mouse", "the mouse stays");

  removed(&bluez, &mut shell, KEYBOARD, vec![DEVICE]);
  wait_for(&mut shell, "no keyboard", || {
    devices.get_untracked() == [headset(), mouse()]
  });
}

#[test]
fn property_changes_show() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  bluez.set(
    &mut shell,
    HEADSET,
    DEVICE,
    &[("Connected", Variant::from(true))],
  );
  wait_for(&mut shell, "a connected headset", || {
    devices.get_untracked()[0].connected
  });
  bluez.set(
    &mut shell,
    HEADSET,
    BATTERY,
    &[("Percentage", Variant::from(75u8))],
  );
  wait_for(&mut shell, "the new battery level", || {
    devices.get_untracked()[0].battery == Some(75)
  });
  bluez.set(
    &mut shell,
    HCI0,
    ADAPTER,
    &[("Discovering", Variant::from(true))],
  );
  wait_for(&mut shell, "discovering", || {
    adapters.get_untracked()[0].discovering
  });
  let expected = BluetoothDevice {
    connected: true,
    state: BluetoothDeviceState::Connected,
    battery: Some(75),
    ..headset()
  };
  assert_eq!(
    devices.get_untracked(),
    [expected, mouse()],
    "the rest stays"
  );
}

#[test]
fn an_adapters_state_is_its_power_state_or_else_whether_it_is_powered() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  let adapters = shell.cx.bluetooth().adapters();
  let state = || {
    adapters
      .get_untracked()
      .first()
      .map(|adapter| adapter.state)
  };
  // As an older BlueZ, which has no PowerState.
  wait_for(&mut shell, "the adapter", || {
    state() == Some(BluetoothAdapterState::Enabled)
  });
  bluez.set(
    &mut shell,
    HCI0,
    ADAPTER,
    &[("Powered", Variant::from(false))],
  );
  wait_for(&mut shell, "disabled", || {
    state() == Some(BluetoothAdapterState::Disabled)
  });

  for (sent, read) in [
    ("off-enabling", BluetoothAdapterState::Enabling),
    ("on", BluetoothAdapterState::Enabled),
    ("on-disabling", BluetoothAdapterState::Disabling),
    ("off-blocked", BluetoothAdapterState::Blocked),
    ("off", BluetoothAdapterState::Disabled),
  ] {
    let power_state = [("PowerState", Variant::from(sent))];
    bluez.set(&mut shell, HCI0, ADAPTER, &power_state);
    wait_for(&mut shell, sent, || state() == Some(read));
  }
  // One BlueZ may add later reads as whether it is powered.
  let logs = capture_logs_from(tracing::Level::DEBUG, || {
    let changed = [
      ("PowerState", Variant::from("on-sleeping")),
      ("Powered", Variant::from(true)),
    ];
    bluez.set(&mut shell, HCI0, ADAPTER, &changed);
    wait_for(&mut shell, "enabled", || {
      state() == Some(BluetoothAdapterState::Enabled)
    });
  });
  assert!(logs.contains("an unknown power state"), "{logs}");
}

#[test]
fn a_device_connects_and_disconnects_while_its_call_is_out() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  for member in ["Connect", "Disconnect"] {
    bluez.on(member, |call| reply(call, &()));
  }
  let bluetooth = shell.cx.bluetooth();
  let devices = bluetooth.devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  let state = || devices.get_untracked()[0].state;

  // A connect that BlueZ answers without connecting.
  bluez.hold();
  bluetooth.connect(HEADSET);
  assert_eq!(state(), BluetoothDeviceState::Connecting, "at once");
  wait_for(&mut shell, "the call", || bluez.held() == 1);
  assert_eq!(state(), BluetoothDeviceState::Connecting);
  bluez.release(&mut shell);
  wait_for(&mut shell, "the answer", || {
    state() == BluetoothDeviceState::Disconnected
  });

  // One that connects before BlueZ answers.
  bluez.hold();
  bluetooth.connect(HEADSET);
  wait_for(&mut shell, "the call", || bluez.held() == 1);
  let connected = [("Connected", Variant::from(true))];
  bluez.set(&mut shell, HEADSET, DEVICE, &connected);
  wait_for(&mut shell, "connected", || {
    state() == BluetoothDeviceState::Connected
  });
  bluez.release(&mut shell);
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(state(), BluetoothDeviceState::Connected);

  // A disconnect over a connect: the connect's answer leaves it
  // disconnecting.
  bluez.hold();
  bluetooth.connect(HEADSET);
  bluetooth.disconnect(HEADSET);
  assert_eq!(state(), BluetoothDeviceState::Disconnecting);
  wait_for(&mut shell, "both calls", || bluez.held() == 2);
  bluez.release_first(&mut shell);
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(state(), BluetoothDeviceState::Disconnecting);
  let disconnected = [("Connected", Variant::from(false))];
  bluez.set(&mut shell, HEADSET, DEVICE, &disconnected);
  wait_for(&mut shell, "disconnected", || {
    state() == BluetoothDeviceState::Disconnected
  });
  bluez.release(&mut shell);
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(state(), BluetoothDeviceState::Disconnected);
  assert!(!devices.get_untracked()[0].connected);

  // BlueZ leaving ends the call that was out.
  bluez.hold();
  bluetooth.connect(HEADSET);
  wait_for(&mut shell, "the call", || bluez.held() == 1);
  bluez.leave(&mut shell);
  wait_for(&mut shell, "no devices", || {
    devices.get_untracked().is_empty()
  });
  let _again = fake_bluez(&mut shell, &bus);
  wait_for(&mut shell, "the devices again", || {
    devices.get_untracked().len() == 2
  });
  assert_eq!(state(), BluetoothDeviceState::Disconnected);
}

#[test]
fn methods_reach_bluez_and_a_refusal_is_logged() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  for member in ["StartDiscovery", "StopDiscovery", "Connect", "Disconnect"] {
    bluez.on(member, |call| reply(call, &()));
  }
  bluez.on("Pair", |call| {
    let error = zbus::Message::error(&call.header(), "org.bluez.Error.AuthenticationFailed");
    Some(error.build(&("Authentication Failed",)))
  });
  let forgotten = Rc::new(RefCell::new(Vec::new()));
  let sink = Rc::clone(&forgotten);
  bluez.on("RemoveDevice", move |call| {
    let device: zbus::OwnedObjectPath = call.body().deserialize().ok()?;
    sink.borrow_mut().push(device.to_string());
    reply(call, &())
  });
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  let logs = capture_logs(|| {
    bluetooth.set_powered(HCI0, false);
    bluetooth.start_discovery(HCI0);
    bluetooth.stop_discovery(HCI0);
    bluetooth.connect(HEADSET);
    bluetooth.disconnect(HEADSET);
    bluetooth.pair(MOUSE);
    bluetooth.set_trusted(MOUSE, true);
    bluetooth.forget(MOUSE);
    let calls = Rc::clone(&bluez.calls);
    wait_for(&mut shell, "every call", || calls.borrow().len() == 8);
    wait_for(&mut shell, "the adapter off and the mouse trusted", || {
      !adapters.get_untracked()[0].powered && devices.get_untracked()[1].trusted
    });
    // The refusal's reply, after the call.
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  let mut calls = bluez.calls.borrow().clone();
  calls.sort();
  assert_eq!(
    calls,
    [
      format!("Connect {HEADSET}"),
      format!("Disconnect {HEADSET}"),
      format!("Pair {MOUSE}"),
      format!("RemoveDevice {HCI0}"),
      format!("Set {HCI0} Powered"),
      format!("Set {MOUSE} Trusted"),
      format!("StartDiscovery {HCI0}"),
      format!("StopDiscovery {HCI0}"),
    ]
  );
  assert_eq!(*forgotten.borrow(), [MOUSE]);
  assert!(logs.contains("AuthenticationFailed"), "{logs}");
  assert_eq!(logs.matches("refused").count(), 1, "{logs}");
}

#[test]
fn bluez_leaving_empties_the_lists_and_coming_back_fills_them() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  bluez.leave(&mut shell);
  wait_for(&mut shell, "empty lists", || {
    devices.get_untracked().is_empty() && adapters.get_untracked().is_empty()
  });

  let _again = fake_bluez(&mut shell, &bus);
  wait_for(&mut shell, "the devices again", || {
    devices.get_untracked() == [headset(), mouse()] && adapters.get_untracked().len() == 1
  });
}

#[test]
fn signals_from_another_program_are_ignored() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _bluez = fake_bluez(&mut shell, &bus);
  let devices = shell.cx.bluetooth().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  // Not BlueZ, but saying a device came and one connected, aimed at every
  // connection on the bus, the shell's too, which no match rule filters.
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
      let connected =
        crate::dbus::Props::from([("Connected".to_owned(), Variant::from(true).try_to_owned()?)]);
      let keyboard = HashMap::from([(DEVICE, connected.clone())]);
      let keyboard = (ObjectPath::try_from(KEYBOARD)?, keyboard);
      for name in names.iter().filter(|name| name.starts_with(':')) {
        let destination = Some(name.as_str());
        let changed = (DEVICE, &connected, Vec::<String>::new());
        let properties = crate::dbus::PROPERTIES;
        impostor
          .emit_signal(
            destination,
            HEADSET,
            properties,
            "PropertiesChanged",
            &changed,
          )
          .await?;
        impostor
          .emit_signal(
            destination,
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &keyboard,
          )
          .await?;
      }
      zbus::Result::Ok(())
    })
    .expect("the impostor's signals");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(devices.get_untracked(), [headset(), mouse()]);
}

#[test]
fn wrong_types_read_as_defaults() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = fake_bluez(&mut shell, &bus);
  bluez.add(
    HCI0,
    ADAPTER,
    &[
      ("Alias", Variant::from(7u32)),
      ("Powered", Variant::from("yes")),
    ],
  );
  bluez.add(
    HEADSET,
    DEVICE,
    &[
      ("Address", Variant::from("AA:BB:CC:DD:EE:01")),
      ("Alias", Variant::from(7u32)),
      ("Adapter", Variant::from(HCI0)),
      ("Connected", Variant::from(1u32)),
    ],
  );
  bluez.add(HEADSET, BATTERY, &[("Percentage", Variant::from(150u8))]);
  bluez.add(MOUSE, BATTERY, &[("Percentage", Variant::from("full"))]);
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  let adapter = adapters.get_untracked()[0].clone();
  assert_eq!((adapter.name.as_str(), adapter.powered), ("", false));
  let headset = devices.get_untracked()[0].clone();
  assert_eq!(headset.name, "AA:BB:CC:DD:EE:01");
  assert_eq!(headset.adapter, "", "a string, not an object path");
  assert!(!headset.connected);
  assert_eq!(headset.state, BluetoothDeviceState::Disconnected);
  assert_eq!(headset.battery, Some(100), "clamped");
  assert_eq!(devices.get_untracked()[1].battery, None);
}

#[test]
fn at_most_16_adapters_and_256_devices_are_kept() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let bluez = FakeService::start(&mut shell, &bus, BLUEZ);
  for n in 0..20 {
    bluez.add(&format!("/org/bluez/hci{n:02}"), ADAPTER, &[]);
  }
  for n in 0..300 {
    let at = format!("{HCI0}/dev_{n:03}");
    bluez.add(&at, DEVICE, &[("Address", Variant::from(format!("{n}")))]);
  }
  bluez.own(&mut shell);
  let bluetooth = shell.cx.bluetooth();
  let (adapters, devices) = (bluetooth.adapters(), bluetooth.devices());
  wait_for(&mut shell, "the devices", || {
    !devices.get_untracked().is_empty()
  });
  assert_eq!(adapters.get_untracked().len(), 16);
  assert_eq!(adapters.get_untracked()[15].path, "/org/bluez/hci15");
  assert_eq!(devices.get_untracked().len(), 256);
  assert_eq!(
    devices.get_untracked()[255].address,
    "255",
    "the first in path order"
  );

  // A device past the cap shows when it sorts earlier, and a connected
  // one shows first, whatever its path.
  added(&bluez, &mut shell, KEYBOARD, vec![(DEVICE, Vec::new())]);
  wait_for(&mut shell, "the keyboard", || {
    devices
      .get_untracked()
      .iter()
      .any(|device| device.path == KEYBOARD)
  });
  assert_eq!(devices.get_untracked().len(), 256);
  let far = "/org/bluez/hci0/dev_299";
  bluez.set(
    &mut shell,
    far,
    DEVICE,
    &[("Connected", Variant::from(true))],
  );
  wait_for(&mut shell, "the far device, connected", || {
    devices
      .get_untracked()
      .iter()
      .any(|device| device.path == far)
  });
  assert_eq!(devices.get_untracked().len(), 256);
}

#[test]
fn only_what_adapters_and_devices_are_is_asked_of_the_bus() {
  use crate::dbus::Daemon;

  // Every rule names BlueZ and either the object manager or one of the
  // three interfaces read, so its media, GATT values and other
  // programs' notifications never reach the shell.
  let rules = super::BluetoothService::rules();
  assert_eq!(rules.len(), 4, "{rules:?}");
  assert!(rules.iter().all(|rule| rule.contains("sender='org.bluez'")));
  assert!(rules[0].contains("interface='org.freedesktop.DBus.ObjectManager'"));
  for (rule, interface) in rules[1..]
    .iter()
    .zip([ADAPTER, DEVICE, "org.bluez.Battery1"])
  {
    assert!(rule.contains("member='PropertiesChanged'"), "{rule}");
    assert!(rule.contains(&format!("arg0='{interface}'")), "{rule}");
  }
}

#[test]
fn an_idle_bluez_wakes_nothing_and_none_at_all_is_quiet() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _bluez = fake_bluez(&mut shell, &bus);
  let devices = shell.cx.bluetooth().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  // A system bus with no BlueZ on it.
  let quiet = PrivateBus::start();
  let mut shell = Harness::new(online(&quiet));
  let devices = shell.cx.bluetooth().devices();
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(devices.get_untracked(), []);
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let bluetooth = shell.cx.bluetooth();
    bluetooth.connect(HEADSET);
    assert_eq!(
      shell.run_until(Instant::now() + Duration::from_millis(200)),
      1
    );
    assert_eq!(bluetooth.adapters().get_untracked(), []);
  });
  assert!(logs.contains("no system bus"), "{logs}");
}
