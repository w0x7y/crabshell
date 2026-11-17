use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::Backlight;
use crate::testing::{
  FakeService, Harness, PrivateBus, TempDir, capture_logs, capture_logs_from, offline, reply,
};
use crate::{Config, ServicesExt};

/// A backlight in the fake sysfs at `root`, as the kernel shows one.
fn backlight(root: &Path, name: &str, brightness: &str, max: &str) {
  let dir = root.join(name);
  std::fs::create_dir_all(&dir).expect("a backlight's directory");
  std::fs::write(dir.join("brightness"), brightness).expect("brightness");
  std::fs::write(dir.join("actual_brightness"), brightness).expect("actual_brightness");
  std::fs::write(dir.join("max_brightness"), max).expect("max_brightness");
}

/// What the kernel does when a program, or a key the firmware handles,
/// changes a backlight: the new value, then a notice on
/// `actual_brightness`.
fn change(root: &Path, name: &str, brightness: &str) {
  let dir = root.join(name);
  std::fs::write(dir.join("brightness"), brightness).expect("brightness");
  std::fs::write(dir.join("actual_brightness"), brightness).expect("actual_brightness");
}

fn with_sysfs(sysfs: &TempDir) -> Config {
  Config {
    backlight: sysfs.path().to_owned(),
    ..offline()
  }
}

fn device(name: &str, brightness: u32, max_brightness: u32) -> Backlight {
  Backlight {
    name: name.to_owned(),
    brightness,
    max_brightness,
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

#[test]
fn the_backlights_show_and_follow_the_kernel() {
  let sysfs = TempDir::new();
  backlight(sysfs.path(), "intel_backlight", "400\n", "1000\n");
  backlight(sysfs.path(), "acpi_video0", "7\n", "15\n");
  let mut shell = Harness::new(with_sysfs(&sysfs));
  let devices = shell.cx.brightness().devices();
  assert_eq!(
    devices.get_untracked(),
    [
      device("acpi_video0", 7, 15),
      device("intel_backlight", 400, 1000)
    ]
  );
  // Every caller shares the list.
  assert_eq!(shell.cx.child().brightness().devices(), devices);

  change(sysfs.path(), "intel_backlight", "250\n");
  shell.retry_until(
    "the new brightness",
    || devices.get_untracked()[1].brightness == 250,
    |_| {},
  );
  assert_eq!(devices.get_untracked()[0], device("acpi_video0", 7, 15));
}

#[test]
fn an_idle_backlight_wakes_nothing() {
  let sysfs = TempDir::new();
  backlight(sysfs.path(), "intel_backlight", "400\n", "1000\n");
  let mut shell = Harness::new(with_sysfs(&sysfs));
  let devices = shell.cx.brightness().devices();
  run(&mut shell, 100);
  assert_eq!(run(&mut shell, 2000), 1);
  assert_eq!(devices.get_untracked().len(), 1);
}

#[test]
fn bad_backlights_are_skipped_and_logged_once_and_the_list_is_capped() {
  let sysfs = TempDir::new();
  for n in 0..40 {
    backlight(sysfs.path(), &format!("panel{n:02}"), "1\n", "10\n");
  }
  backlight(sysfs.path(), "panel00", "bright\n", "10\n");
  let long = format!("{}5\n", "0".repeat(70));
  backlight(sysfs.path(), "panel01", "5\n", &long);
  let mut shell = Harness::new(with_sysfs(&sysfs));
  let logs = capture_logs(|| {
    let devices = shell.cx.brightness().devices();
    let names: Vec<_> = devices
      .get_untracked()
      .into_iter()
      .map(|device| device.name)
      .collect();
    assert_eq!(names.len(), 30, "{names:?}");
    assert_eq!(names[0], "panel02");
    assert_eq!(names[29], "panel31");
    // A change elsewhere reads them all again, and logs nothing new.
    change(sysfs.path(), "panel02", "3\n");
    shell.retry_until(
      "the change",
      || devices.get_untracked()[0].brightness == 3,
      |_| {},
    );
    // The bad one is listed once it reads well.
    change(sysfs.path(), "panel00", "4\n");
    shell.retry_until(
      "the mended backlight",
      || devices.get_untracked()[0] == device("panel00", 4, 10),
      |_| {},
    );
  });
  assert_eq!(
    logs.matches("can't read this backlight").count(),
    2,
    "{logs}"
  );
  assert!(logs.contains("holds \"bright\\n\""), "{logs}");
  assert!(logs.contains("is over 64 bytes"), "{logs}");
  assert!(logs.contains("more backlights than 32"), "{logs}");
}

#[test]
fn a_machine_without_backlights_lists_none() {
  let mut shell = Harness::new(offline());
  let logs = capture_logs_from(tracing::Level::INFO, || {
    let devices = shell.cx.brightness().devices();
    assert_eq!(devices.get_untracked(), []);
    assert_eq!(run(&mut shell, 200), 1);
  });
  assert!(logs.contains("no backlights"), "{logs}");
}

#[test]
fn set_asks_logind_for_a_value_held_to_the_max() {
  let sysfs = TempDir::new();
  backlight(sysfs.path(), "intel_backlight", "400\n", "1000\n");
  let bus = PrivateBus::start();
  let mut shell = Harness::new(Config {
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..with_sysfs(&sysfs)
  });
  let logind = FakeService::start(&mut shell, &bus, super::logind::LOGIND);
  let asked: Rc<RefCell<Vec<String>>> = Rc::default();
  let log = Rc::clone(&asked);
  logind.on("SetBrightness", move |call| {
    let (subsystem, name, value): (String, String, u32) = call.body().deserialize().expect("ssu");
    let path = call.header().path().expect("a path").to_string();
    let interface = call.header().interface().expect("an interface").to_string();
    log
      .borrow_mut()
      .push(format!("{path} {interface} {subsystem} {name} {value}"));
    reply(call, &())
  });
  logind.own(&mut shell);
  let brightness = shell.cx.brightness();
  // Logind is reached a moment after the first call.
  shell.retry_until(
    "a SetBrightness",
    || !asked.borrow().is_empty(),
    |_| brightness.set("intel_backlight", 5000),
  );
  for asked in asked.borrow().iter() {
    assert_eq!(
      asked,
      "/org/freedesktop/login1/session/auto org.freedesktop.login1.Session backlight \
       intel_backlight 1000"
    );
  }
  asked.borrow_mut().clear();
  brightness.set("intel_backlight", 200);
  shell.retry_until("the second", || !asked.borrow().is_empty(), |_| {});
  assert!(asked.borrow()[0].ends_with("intel_backlight 200"));

  let logs = capture_logs(|| brightness.set("nothing", 3));
  assert!(logs.contains("no such backlight"), "{logs}");
}
