use std::time::{Duration, Instant};

use zbus::{ObjectPath, Variant};

use super::{
  BatteryState, DEVICE, DeviceKind, PROFILES, PROFILES_PATH, PowerProfile, UPOWER, UPOWER_PATH,
};
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

const BATTERY: &str = "/org/freedesktop/UPower/devices/battery_BAT0";
const AC: &str = "/org/freedesktop/UPower/devices/line_power_AC";
const DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const MOUSE: &str = "/org/freedesktop/UPower/devices/mouse_hid";

fn online(bus: &PrivateBus) -> Config {
  Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

/// A charging laptop battery at `percent`.
fn battery(percent: f64) -> Vec<(&'static str, Variant<'static>)> {
  vec![
    ("Type", Variant::from(2u32)),
    ("State", Variant::from(1u32)),
    ("Percentage", Variant::from(percent)),
    ("IsPresent", Variant::from(true)),
    ("PowerSupply", Variant::from(true)),
    ("TimeToEmpty", Variant::from(0i64)),
    ("TimeToFull", Variant::from(3600i64)),
    ("EnergyRate", Variant::from(12.5)),
    ("Model", Variant::from("BAT0")),
    ("IconName", Variant::from("battery-good-charging-symbolic")),
  ]
}

/// UPower with a battery, a charger and the display device, the way
/// upowerd lists them.
fn fake_upower(shell: &mut Harness, bus: &PrivateBus) -> FakeService {
  let upower = FakeService::start(shell, bus, UPOWER);
  upower.add(UPOWER_PATH, UPOWER, &[("OnBattery", Variant::from(false))]);
  upower.add(BATTERY, DEVICE, &battery(50.0));
  upower.add(
    AC,
    DEVICE,
    &[
      ("Type", Variant::from(1u32)),
      ("PowerSupply", Variant::from(true)),
    ],
  );
  upower.add(DISPLAY, DEVICE, &battery(50.0));
  upower.on("EnumerateDevices", |call| {
    let paths = [BATTERY, AC].map(|path| ObjectPath::try_from(path).expect("a path"));
    reply(call, &paths.to_vec())
  });
  upower.on("GetDisplayDevice", |call| {
    reply(call, &ObjectPath::try_from(DISPLAY).expect("a path"))
  });
  upower.own(shell);
  upower
}

fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

#[test]
fn the_battery_and_charger_show_and_follow_upower() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let upower_fake = fake_upower(&mut shell, &bus);
  let upower = shell.cx.upower();
  let devices = upower.devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  let [battery, ac] = <[_; 2]>::try_from(devices.get_untracked()).expect("two devices");
  assert_eq!(battery.path, BATTERY);
  assert_eq!(battery.kind, DeviceKind::Battery);
  assert_eq!(battery.state, BatteryState::Charging);
  assert_eq!(battery.percent, 50.0);
  assert_eq!(battery.time_to_full, Some(Duration::from_secs(3600)));
  assert_eq!(battery.time_to_empty, None, "0 means unknown");
  assert_eq!(battery.model, "BAT0");
  assert_eq!(ac.kind, DeviceKind::LinePower);
  let display = upower.display_device();
  assert_eq!(display.get_untracked().map(|d| d.percent), Some(50.0));
  assert!(!upower.on_battery().get_untracked());

  upower_fake.set(
    &mut shell,
    BATTERY,
    DEVICE,
    &[
      ("Percentage", Variant::from(45.0)),
      ("State", Variant::from(2u32)),
    ],
  );
  wait_for(&mut shell, "the new percentage", || {
    devices.get_untracked()[0].percent == 45.0
  });
  assert_eq!(devices.get_untracked()[0].state, BatteryState::Discharging);
  assert_eq!(devices.get_untracked()[0].model, "BAT0", "the rest stays");

  upower_fake.set(
    &mut shell,
    UPOWER_PATH,
    UPOWER,
    &[("OnBattery", Variant::from(true))],
  );
  let on_battery = upower.on_battery();
  wait_for(&mut shell, "on battery", || on_battery.get_untracked());
}

#[test]
fn devices_come_and_go_as_upower_says() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let upower_fake = fake_upower(&mut shell, &bus);
  let devices = shell.cx.upower().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  upower_fake.add(
    MOUSE,
    DEVICE,
    &[
      ("Type", Variant::from(5u32)),
      ("Percentage", Variant::from(80.0)),
    ],
  );
  let added = ObjectPath::try_from(MOUSE).expect("a path");
  upower_fake.emit(
    &mut shell,
    UPOWER_PATH,
    UPOWER,
    "DeviceAdded",
    added.clone(),
  );
  wait_for(&mut shell, "the mouse", || {
    devices.get_untracked().len() == 3
  });
  assert_eq!(devices.get_untracked()[2].kind, DeviceKind::Mouse);

  upower_fake.remove(MOUSE);
  upower_fake.emit(&mut shell, UPOWER_PATH, UPOWER, "DeviceRemoved", added);
  wait_for(&mut shell, "no mouse", || {
    devices.get_untracked().len() == 2
  });
}

#[test]
fn upower_leaving_empties_the_list_and_coming_back_fills_it() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let upower_fake = fake_upower(&mut shell, &bus);
  let upower = shell.cx.upower();
  let (devices, display) = (upower.devices(), upower.display_device());
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });

  upower_fake.leave(&mut shell);
  wait_for(&mut shell, "an empty list", || {
    devices.get_untracked().is_empty()
  });
  assert_eq!(display.get_untracked(), None);

  let _again = fake_upower(&mut shell, &bus);
  wait_for(&mut shell, "the devices again", || {
    devices.get_untracked().len() == 2
  });
}

#[test]
fn a_property_of_the_wrong_type_reads_as_unknown() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let upower_fake = fake_upower(&mut shell, &bus);
  upower_fake.add(
    BATTERY,
    DEVICE,
    &[
      ("Type", Variant::from("battery")),
      ("Percentage", Variant::from(7u32)),
    ],
  );
  let devices = shell.cx.upower().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  let battery = devices.get_untracked()[0].clone();
  assert_eq!(battery.kind, DeviceKind::Unknown);
  assert_eq!(battery.percent, 0.0);
}

#[test]
fn signals_from_another_program_are_ignored() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _upower = fake_upower(&mut shell, &bus);
  let devices = shell.cx.upower().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  // Not UPower, but saying it changed, aimed at every connection on the
  // bus, the shell's too, which no match rule filters.
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
      let changed =
        crate::dbus::Props::from([("Percentage".to_owned(), Variant::from(1.0).try_to_owned()?)]);
      for name in names.iter().filter(|name| name.starts_with(':')) {
        let body = (DEVICE, &changed, Vec::<String>::new());
        let properties = crate::dbus::PROPERTIES;
        let signal = "PropertiesChanged";
        impostor
          .emit_signal(Some(name.as_str()), BATTERY, properties, signal, &body)
          .await?;
      }
      zbus::Result::Ok(())
    })
    .expect("the impostor's signals");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(devices.get_untracked()[0].percent, 50.0);
}

#[test]
fn an_idle_upower_wakes_nothing_and_none_at_all_is_quiet() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _upower = fake_upower(&mut shell, &bus);
  let devices = shell.cx.upower().devices();
  wait_for(&mut shell, "the devices", || {
    devices.get_untracked().len() == 2
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  // A system bus with no UPower on it.
  let quiet = PrivateBus::start();
  let mut shell = Harness::new(online(&quiet));
  let devices = shell.cx.upower().devices();
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(devices.get_untracked(), []);
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let upower = shell.cx.upower();
    assert_eq!(
      shell.run_until(Instant::now() + Duration::from_millis(200)),
      1
    );
    assert_eq!(upower.devices().get_untracked(), []);
  });
  assert!(logs.contains("no system bus"), "{logs}");
}

/// power-profiles-daemon with its three profiles, in `active`.
fn fake_profiles(shell: &mut Harness, bus: &PrivateBus, active: &str) -> FakeService {
  let daemon = FakeService::start(shell, bus, PROFILES);
  let profile = |name: &'static str| {
    std::collections::HashMap::from([
      ("Profile", Variant::from(name)),
      ("Driver", Variant::from("platform")),
    ])
  };
  let profiles = vec![
    profile("power-saver"),
    profile("balanced"),
    profile("performance"),
  ];
  daemon.add(
    PROFILES_PATH,
    PROFILES,
    &[
      ("ActiveProfile", Variant::from(active)),
      ("Profiles", Variant::from(profiles)),
    ],
  );
  daemon.own(shell);
  daemon
}

#[test]
fn the_active_profile_shows_and_setting_one_asks_the_daemon() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let daemon = fake_profiles(&mut shell, &bus, "balanced");
  let profiles = shell.cx.power_profiles();
  let active = profiles.active();
  wait_for(&mut shell, "the active profile", || {
    active.get_untracked().is_some()
  });
  assert_eq!(active.get_untracked(), Some(PowerProfile::Balanced));
  assert_eq!(
    profiles.available().get_untracked(),
    [
      PowerProfile::PowerSaver,
      PowerProfile::Balanced,
      PowerProfile::Performance
    ]
  );

  profiles.set(PowerProfile::Performance);
  wait_for(&mut shell, "performance", || {
    active.get_untracked() == Some(PowerProfile::Performance)
  });
  assert_eq!(
    daemon.calls.borrow().as_slice(),
    [format!("Set {PROFILES_PATH} ActiveProfile")]
  );

  daemon.leave(&mut shell);
  wait_for(&mut shell, "no profile", || {
    active.get_untracked().is_none()
  });
  assert_eq!(profiles.available().get_untracked(), []);
}

#[test]
fn an_unknown_profile_reads_as_none() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let _daemon = fake_profiles(&mut shell, &bus, "turbo");
  let profiles = shell.cx.power_profiles();
  let available = profiles.available();
  wait_for(&mut shell, "the profiles", || {
    available.get_untracked().len() == 3
  });
  assert_eq!(profiles.active().get_untracked(), None);
}
