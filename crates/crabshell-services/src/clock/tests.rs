use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use calloop::futures::Scheduler;
use crabshell_reactive::Runtime;
use crabshell_ui::Cx;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};

use super::{Clocks, EARLY, Precision, boundaries, system_zone};
use crate::{Config, ServicesExt};

fn at(time: &str) -> Timestamp {
    time.parse().expect("a valid timestamp")
}

#[test]
fn boundaries_truncate_to_the_precision() {
    let now = at("2026-11-11T10:41:27.6Z");
    assert_eq!(
        boundaries(now, Precision::Second),
        (at("2026-11-11T10:41:27Z"), at("2026-11-11T10:41:28Z"))
    );
    assert_eq!(
        boundaries(now, Precision::Minute),
        (at("2026-11-11T10:41:00Z"), at("2026-11-11T10:42:00Z"))
    );
    // Exactly on a boundary shows that boundary.
    assert_eq!(
        boundaries(at("2026-11-11T10:42:00Z"), Precision::Minute).0,
        at("2026-11-11T10:42:00Z")
    );
}

#[test]
fn a_timer_a_hair_early_shows_the_new_value() {
    assert_eq!(
        boundaries(at("2026-11-11T10:41:59.99Z"), Precision::Minute),
        (at("2026-11-11T10:42:00Z"), at("2026-11-11T10:43:00Z"))
    );
    assert_eq!(
        boundaries(at("2026-11-11T10:41:27.999Z"), Precision::Second),
        (at("2026-11-11T10:41:28Z"), at("2026-11-11T10:41:29Z"))
    );
    // Further off is still the minute before.
    assert_eq!(
        boundaries(at("2026-11-11T10:41:59.9Z"), Precision::Minute).0,
        at("2026-11-11T10:41:00Z")
    );
}

#[test]
fn a_dst_change_is_a_minute_like_any_other() {
    // Central Europe springs forward at 02:00 on 2026-03-29, to 03:00.
    let cet = TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").expect("a valid POSIX zone");
    let shown_in = |time: Timestamp| time.to_zoned(cet.clone()).strftime("%H:%M %Z").to_string();
    let (shown, next) = boundaries(at("2026-03-29T00:59:30Z"), Precision::Minute);
    assert_eq!(shown_in(shown), "01:59 CET");
    assert_eq!(shown_in(next), "03:00 CEST");
    // And falls back at 03:00 on 2026-10-25, to 02:00.
    let (shown, next) = boundaries(at("2026-10-25T00:59:30Z"), Precision::Minute);
    assert_eq!(shown_in(shown), "02:59 CEST");
    assert_eq!(shown_in(next), "02:00 CET");
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

/// What a shell gives the services, with no compositor: a loop and a root
/// context.
struct Harness {
    event_loop: EventLoop<'static, ()>,
    cx: Cx,
    /// Runs the tests' own futures, such as a fake logind's.
    scheduler: Scheduler<()>,
}

impl Harness {
    fn new(config: Config) -> Harness {
        let event_loop = EventLoop::try_new().expect("an event loop");
        let cx = Cx::new(Runtime::current().new_owner());
        crate::provide_with(&cx, &event_loop.handle(), config);
        let (executor, scheduler) = calloop::futures::executor().expect("an executor");
        event_loop
            .handle()
            .insert_source(executor, |(), _, _| {})
            .expect("the tests' executor in the loop");
        Harness {
            event_loop,
            cx,
            scheduler,
        }
    }

    /// Runs the loop as the shell does, until `deadline`. Returns how many
    /// times it woke.
    fn run_until(&mut self, deadline: Instant) -> usize {
        let mut wakeups = 0;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            if left.is_zero() {
                break;
            }
            self.event_loop
                .dispatch(Some(left), &mut ())
                .expect("the loop runs");
            Runtime::current().flush();
            wakeups += 1;
        }
        wakeups
    }

    /// Runs `future` to its end on the loop, which runs everything else
    /// meanwhile. Gives up after 5 s.
    fn block_on<R: 'static>(&mut self, future: impl Future<Output = R> + 'static) -> R {
        let result = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&result);
        self.scheduler
            .schedule(async move { *sink.borrow_mut() = Some(future.await) })
            .expect("the tests' executor runs");
        let give_up = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = result.borrow_mut().take() {
                return result;
            }
            assert!(Instant::now() < give_up, "a future still running after 5 s");
            self.run_until(Instant::now() + Duration::from_millis(10));
        }
    }

    /// Does `act`, then runs the loop for 100 ms, until `done`. Gives up
    /// after 5 s.
    fn retry_until(
        &mut self,
        what: &str,
        done: impl Fn() -> bool,
        mut act: impl FnMut(&mut Harness),
    ) {
        let give_up = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < give_up, "no {what} after 5 s");
            act(self);
            self.run_until(Instant::now() + Duration::from_millis(100));
        }
    }
}

/// A config that reads the system zone through jiff and has no bus.
fn offline() -> Config {
    Config {
        localtime: None,
        system_bus: None,
    }
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
    shell.cx.owner().dispose();
    assert_eq!(runtime.node_count(), before);
    // The second's timer fires, finds its signal gone and stops.
    shell.run_until(Instant::now() + Duration::from_millis(1100));
    assert_eq!(second.try_get(), None);
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
    });
    let clock = shell.cx.clock(Precision::Second);
    let first = clock.get_untracked().timestamp();
    shell.run_until(Instant::now() + Duration::from_millis(1100));
    assert!(clock.get_untracked().timestamp() > first);
}

/// A private bus standing in for the system bus: `dbus-daemon --session`
/// on an address of its own. It stops on drop.
struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    fn start() -> PrivateBus {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon starts (it comes with dbus)");
        let stdout = daemon.stdout.take().expect("dbus-daemon's stdout");
        let mut address = String::new();
        BufReader::new(stdout)
            .read_line(&mut address)
            .expect("dbus-daemon prints its address");
        let address = address.trim().to_owned();
        PrivateBus { daemon, address }
    }

    /// A connection to it, owning `names`.
    fn connect(&self, names: &[&'static str]) -> impl Future<Output = zbus::Connection> + 'static {
        let mut builder = zbus::connection::Builder::address(self.address.as_str());
        for &name in names {
            builder = builder.name(name);
        }
        async move {
            builder
                .build()
                .await
                .expect("a connection to the private bus")
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

async fn prepare_for_sleep(from: zbus::Connection, start: bool) {
    let (path, interface) = ("/org/freedesktop/login1", "org.freedesktop.login1.Manager");
    from.emit_signal(None::<&str>, path, interface, "PrepareForSleep", &start)
        .await
        .expect("a signal on the private bus");
}

async fn properties_changed(from: zbus::Connection, property: &'static str) {
    let path = "/org/freedesktop/timedate1";
    let changed = HashMap::from([(property, zbus::Variant::from("Asia/Tokyo"))]);
    let body = ("org.freedesktop.timedate1", changed, Vec::<&str>::new());
    from.emit_signal(
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
fn logind_and_timedated_resync_the_clock() {
    let bus = PrivateBus::start();
    let localtime = Localtime::new("Europe/Paris");
    let mut shell = Harness::new(Config {
        localtime: Some(localtime.path()),
        system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
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
    localtime.point_at("Asia/Tokyo");

    // A resume re-syncs, until the watcher's match rules are in.
    let armed = timer();
    shell.retry_until(
        "re-sync after PrepareForSleep(false)",
        || timer() != armed,
        |shell| shell.block_on(prepare_for_sleep(system.clone(), false)),
    );
    assert_eq!(zone(), paris, "a resume doesn't read the zone");

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
    shell.retry_until(
        "new zone after PropertiesChanged",
        || zone() == tokyo,
        |shell| shell.block_on(properties_changed(system.clone(), "Timezone")),
    );
    assert_ne!(timer(), armed);
}
