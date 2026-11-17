use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};

use super::{Clocks, EARLY, Precision, boundaries, current, system_zone};
use crate::testing::{Harness, PrivateBus, offline};
use crate::{Config, ServicesExt};

fn at(time: &str) -> Timestamp {
  time.parse().expect("a valid timestamp")
}

#[test]
fn boundaries_truncate_to_the_precision() {
  let now = at("2026-11-11T10:41:27.6Z");
  assert_eq!(
    boundaries(now, Precision::Second, &TimeZone::UTC),
    (at("2026-11-11T10:41:27Z"), at("2026-11-11T10:41:28Z"))
  );
  assert_eq!(
    boundaries(now, Precision::Minute, &TimeZone::UTC),
    (at("2026-11-11T10:41:00Z"), at("2026-11-11T10:42:00Z"))
  );
  // Exactly on a boundary shows that boundary.
  assert_eq!(
    boundaries(
      at("2026-11-11T10:42:00Z"),
      Precision::Minute,
      &TimeZone::UTC
    )
    .0,
    at("2026-11-11T10:42:00Z")
  );
}

#[test]
fn a_timer_a_hair_early_shows_the_new_value() {
  assert_eq!(
    boundaries(
      at("2026-11-11T10:41:59.99Z"),
      Precision::Minute,
      &TimeZone::UTC
    ),
    (at("2026-11-11T10:42:00Z"), at("2026-11-11T10:43:00Z"))
  );
  assert_eq!(
    boundaries(
      at("2026-11-11T10:41:27.999Z"),
      Precision::Second,
      &TimeZone::UTC
    ),
    (at("2026-11-11T10:41:28Z"), at("2026-11-11T10:41:29Z"))
  );
  // Further off is still the minute before.
  assert_eq!(
    boundaries(
      at("2026-11-11T10:41:59.9Z"),
      Precision::Minute,
      &TimeZone::UTC
    )
    .0,
    at("2026-11-11T10:41:00Z")
  );
}

#[test]
fn a_dst_change_is_a_minute_like_any_other() {
  // Central Europe springs forward at 02:00 on 2026-03-29, to 03:00.
  let cet = TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").expect("a valid POSIX zone");
  let shown_in = |time: Timestamp| time.to_zoned(cet.clone()).strftime("%H:%M %Z").to_string();
  let (shown, next) = boundaries(
    at("2026-03-29T00:59:30Z"),
    Precision::Minute,
    &TimeZone::UTC,
  );
  assert_eq!(shown_in(shown), "01:59 CET");
  assert_eq!(shown_in(next), "03:00 CEST");
  // And falls back at 03:00 on 2026-10-25, to 02:00.
  let (shown, next) = boundaries(
    at("2026-10-25T00:59:30Z"),
    Precision::Minute,
    &TimeZone::UTC,
  );
  assert_eq!(shown_in(shown), "02:59 CEST");
  assert_eq!(shown_in(next), "02:00 CET");
}

#[test]
fn a_day_clock_shows_local_midnight_however_long_the_day() {
  let new_york = TimeZone::get("America/New_York").expect("the zone database");
  let day = |now: &str| boundaries(at(now), Precision::Day, &new_york);
  assert_eq!(
    day("2026-11-16T15:00:00Z"),
    (at("2026-11-16T05:00:00Z"), at("2026-11-17T05:00:00Z"))
  );
  // New York's clocks go forward an hour on 2026-03-08 and back on
  // 2026-11-01, so those days last 23 and 25 hours.
  assert_eq!(
    day("2026-03-08T12:00:00Z"),
    (at("2026-03-08T05:00:00Z"), at("2026-03-09T04:00:00Z"))
  );
  assert_eq!(
    day("2026-11-01T12:00:00Z"),
    (at("2026-11-01T04:00:00Z"), at("2026-11-02T05:00:00Z"))
  );
  // A timer a hair early shows the new day; further off, the old one.
  assert_eq!(day("2026-11-17T04:59:59.99Z").0, at("2026-11-17T05:00:00Z"));
  assert_eq!(day("2026-11-17T04:59:59.9Z").0, at("2026-11-16T05:00:00Z"));
  // Shown as midnight in the zone, and due then.
  let (shown, due) = current(Precision::Day, &new_york);
  assert_eq!((shown.hour(), shown.minute(), shown.second()), (0, 0, 0));
  let midnight = shown.tomorrow().unwrap().start_of_day().unwrap();
  let wait = Duration::try_from(Timestamp::now().duration_until(midnight.timestamp())).unwrap();
  let off = due.saturating_duration_since(Instant::now()).abs_diff(wait);
  assert!(
    off < Duration::from_millis(100),
    "due {off:?} from midnight"
  );
}

#[test]
fn a_day_clock_sleeps_until_midnight() {
  let mut shell = Harness::new(offline());
  let day = shell.cx.clock(Precision::Day);
  assert_eq!(day.get_untracked().date(), Zoned::now().date());
  assert_eq!(day.get_untracked().hour(), 0);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}

/// A link like `/etc/localtime`, in a directory of its own, into the
/// system's zoneinfo. The directory goes on drop.
struct Localtime(PathBuf);

impl Localtime {
  fn new(zone: &str) -> Localtime {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-services-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let localtime = Localtime(dir);
    localtime.point_at(zone);
    localtime
  }

  fn path(&self) -> PathBuf {
    self.0.join("localtime")
  }

  /// Points the link at `zone`, as timedated does.
  fn point_at(&self, zone: &str) {
    let _ = std::fs::remove_file(self.path());
    std::os::unix::fs::symlink(format!("/usr/share/zoneinfo/{zone}"), self.path())
      .expect("a link in the temporary directory");
  }
}

impl Drop for Localtime {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

#[test]
fn a_changed_zone_is_read_again_at_once() {
  let localtime = Localtime::new("Europe/Paris");
  let path = localtime.path();
  assert_eq!(system_zone(Some(&path)).iana_name(), Some("Europe/Paris"));
  localtime.point_at("Asia/Tokyo");
  assert_eq!(system_zone(Some(&path)).iana_name(), Some("Asia/Tokyo"));
}

#[test]
fn a_second_clock_ticks_on_whole_seconds_with_one_wakeup_each() {
  let mut shell = Harness::new(offline());
  let clock = shell.cx.clock(Precision::Second);
  let seen: Rc<RefCell<Vec<(Zoned, Timestamp)>>> = Rc::default();
  let sink = Rc::clone(&seen);
  shell
    .cx
    .effect(move || sink.borrow_mut().push((clock.get(), Timestamp::now())));
  Runtime::current().flush();

  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(2500));

  let seen = seen.borrow();
  let ticks = seen.len() - 1;
  assert!((2..=3).contains(&ticks), "{ticks} ticks in 2.5 s: {seen:?}");
  // One wakeup per tick, plus the one that ends the run.
  assert!(wakeups <= ticks + 1, "{wakeups} wakeups for {ticks} ticks");
  for (shown, _) in seen.iter() {
    assert_eq!(shown.subsec_nanosecond(), 0, "{shown} isn't a whole second");
  }
  for pair in seen.windows(2) {
    let step = pair[1].0.timestamp().duration_since(pair[0].0.timestamp());
    assert_eq!(step, SignedDuration::from_secs(1), "{seen:?}");
  }
  for (shown, when) in &seen[1..] {
    let late = when.duration_since(shown.timestamp());
    assert!(
      late >= -EARLY && late < SignedDuration::from_millis(100),
      "{shown} shown at {when}"
    );
  }
}

#[test]
fn every_caller_shares_one_clock_per_precision() {
  let shell = Harness::new(offline());
  let minute = shell.cx.clock(Precision::Minute);
  let output = shell.cx.child();
  assert_eq!(output.clock(Precision::Minute), minute);
  let second = output.clock(Precision::Second);
  assert_ne!(second, minute);
  assert_eq!(shell.cx.clock(Precision::Second), second);
  // They belong to the root, so an output going away leaves them.
  output.owner().dispose();
  assert!(second.try_get().is_some());
  assert_eq!(minute.get().subsec_nanosecond(), 0);
  assert_eq!(minute.get().second(), 0);
}

#[test]
fn without_provide_a_clock_stands_still_under_its_context() {
  let cx = Cx::new(Runtime::current().new_owner());
  let clock = cx.clock(Precision::Minute);
  assert_eq!(clock.get().second(), 0);
  cx.owner().dispose();
  assert_eq!(clock.try_get(), None);
}

#[test]
fn disposing_the_root_frees_every_clock() {
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let mut shell = Harness::new(offline());
  let second = shell.cx.clock(Precision::Second);
  shell.cx.child().clock(Precision::Minute);
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  let held = Rc::strong_count(&clocks);
  shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  // The second's timer fires, finds its signal gone and drops itself,
  // with the hold it had on the clocks.
  shell.run_until(Instant::now() + Duration::from_millis(1100));
  assert_eq!(second.try_get(), None);
  assert!(
    Rc::strong_count(&clocks) < held,
    "the second's timer still runs"
  );
}

#[test]
fn a_resync_replaces_each_timer() {
  let shell = Harness::new(offline());
  shell.cx.clock(Precision::Second);
  shell.cx.clock(Precision::Minute);
  shell.cx.clock(Precision::Day);
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  // Each timer holds the clocks, so a timer left behind shows here. It
  // would tick in the same dispatch as its replacement, unseen otherwise.
  let held = Rc::strong_count(&clocks);
  for _ in 0..3 {
    clocks.resync();
  }
  assert_eq!(Rc::strong_count(&clocks), held);
}

#[test]
fn one_system_bus_watcher_serves_every_clock() {
  let shell = Harness::new(Config {
    localtime: None,
    system_bus: Some(
      "unix:path=/nonexistent/crabshell-test-bus"
        .parse()
        .expect("an address"),
    ),
    ..offline()
  });
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  let held = Rc::strong_count(&clocks);
  shell.cx.clock(Precision::Minute);
  // The first clock adds its timer and the one that starts the watcher
  // a moment later; the second only its timer.
  assert_eq!(Rc::strong_count(&clocks), held + 2);
  shell.cx.clock(Precision::Second);
  assert_eq!(Rc::strong_count(&clocks), held + 3);
}

#[test]
fn the_system_bus_is_reached_only_once_the_shell_has_started() {
  let mut shell = Harness::new(Config {
    localtime: None,
    system_bus: Some(
      "unix:path=/nonexistent/crabshell-test-bus"
        .parse()
        .expect("an address"),
    ),
    ..offline()
  });
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  let held = Rc::strong_count(&clocks);
  shell.cx.clock(Precision::Day);
  // The clock's timer and the timer that starts the watcher.
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(Rc::strong_count(&clocks), held + 2, "not reached yet");
  // The watcher ran and, with no bus there, ended.
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(Rc::strong_count(&clocks), held + 1);
}

#[test]
fn a_tick_reads_the_zone_again() {
  // Under OpenRC no timedated says the zone changed: the link is all.
  let localtime = Localtime::new("Europe/Paris");
  let mut shell = Harness::new(Config {
    localtime: Some(localtime.path()),
    ..offline()
  });
  let clock = shell.cx.clock(Precision::Second);
  assert_eq!(
    clock.get_untracked().time_zone().iana_name(),
    Some("Europe/Paris")
  );
  localtime.point_at("Asia/Tokyo");
  shell.run_until(Instant::now() + Duration::from_millis(1100));
  assert_eq!(
    clock.get_untracked().time_zone().iana_name(),
    Some("Asia/Tokyo")
  );
}

#[test]
fn without_a_system_bus_the_clock_keeps_ticking() {
  let mut shell = Harness::new(Config {
    localtime: None,
    system_bus: Some(
      "unix:path=/nonexistent/crabshell-test-bus"
        .parse()
        .expect("an address"),
    ),
    ..offline()
  });
  let clock = shell.cx.clock(Precision::Second);
  let first = clock.get_untracked().timestamp();
  shell.run_until(Instant::now() + Duration::from_millis(1100));
  assert!(clock.get_untracked().timestamp() > first);
}

async fn prepare_for_sleep(from: zbus::Connection, start: bool) {
  let (path, interface) = ("/org/freedesktop/login1", "org.freedesktop.login1.Manager");
  from
    .emit_signal(None::<&str>, path, interface, "PrepareForSleep", &start)
    .await
    .expect("a signal on the private bus");
}

async fn properties_changed(from: zbus::Connection, property: &'static str) {
  let path = "/org/freedesktop/timedate1";
  let changed = HashMap::from([(property, zbus::Variant::from("Asia/Tokyo"))]);
  let body = ("org.freedesktop.timedate1", changed, Vec::<&str>::new());
  from
    .emit_signal(
      None::<&str>,
      path,
      "org.freedesktop.DBus.Properties",
      "PropertiesChanged",
      &body,
    )
    .await
    .expect("a signal on the private bus");
}

#[test]
fn the_clock_resyncs_once_the_bus_is_reached() {
  // A suspend before the watcher's match rules are in, whose resume
  // logind announced to nobody, is caught up at once.
  let bus = PrivateBus::start();
  let mut shell = Harness::new(Config {
    localtime: None,
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  });
  shell.cx.clock(Precision::Minute);
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  let timer = || clocks.started.borrow()[0].timer;
  let armed = timer();
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(timer(), armed, "a tick keeps its timer");
  shell.retry_until(
    "a re-sync once the bus is reached",
    || timer() != armed,
    |_| {},
  );
}

#[test]
fn logind_and_timedated_resync_the_clock() {
  let bus = PrivateBus::start();
  let localtime = Localtime::new("Europe/Paris");
  let mut shell = Harness::new(Config {
    localtime: Some(localtime.path()),
    system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  });
  let clock = shell.cx.clock(Precision::Minute);
  let zone = || {
    clock
      .get_untracked()
      .time_zone()
      .iana_name()
      .map(str::to_owned)
  };
  let (paris, tokyo) = (
    Some("Europe/Paris".to_owned()),
    Some("Asia/Tokyo".to_owned()),
  );
  assert_eq!(zone(), paris);
  // A re-sync sets a new timer; a tick keeps the one it has.
  let clocks = shell.cx.service::<Clocks>().expect("provided");
  let timer = || clocks.started.borrow()[0].timer;
  let system =
    shell.block_on(bus.connect(&["org.freedesktop.login1", "org.freedesktop.timedate1"]));
  let stranger = shell.block_on(bus.connect(&[]));

  // The watcher re-syncs once its match rules are in, then a resume
  // re-syncs again.
  let armed = timer();
  shell.retry_until("the watcher's re-sync", || timer() != armed, |_| {});
  let armed = timer();
  shell.block_on(prepare_for_sleep(system.clone(), false));
  shell.retry_until(
    "re-sync after PrepareForSleep(false)",
    || timer() != armed,
    |_| {},
  );

  // Going to sleep, other timedated properties and other senders change
  // nothing.
  let armed = timer();
  shell.block_on(prepare_for_sleep(system.clone(), true));
  shell.block_on(properties_changed(system.clone(), "NTP"));
  shell.block_on(prepare_for_sleep(stranger.clone(), false));
  shell.block_on(properties_changed(stranger, "Timezone"));
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(timer(), armed);
  assert_eq!(zone(), paris);

  // timedated's new zone is read and shown at once.
  localtime.point_at("Asia/Tokyo");
  shell.retry_until(
    "new zone after PropertiesChanged",
    || zone() == tokyo,
    |shell| shell.block_on(properties_changed(system.clone(), "Timezone")),
  );
  assert_ne!(timer(), armed);
}
