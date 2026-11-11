//! The clock: the time, truncated to a whole second or minute.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};

use crate::Shared;

#[cfg(test)]
mod tests;

/// How often a clock changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precision {
    /// On every whole second.
    Second,
    /// On every whole minute.
    Minute,
}

impl Precision {
    fn seconds(self) -> i64 {
        match self {
            Precision::Second => 1,
            Precision::Minute => 60,
        }
    }
}

/// How early a timer may fire and still count as on its boundary.
const EARLY: SignedDuration = SignedDuration::from_millis(50);

/// The clocks of one shell: one per precision, each started on first use.
pub(crate) struct Clocks {
    shared: Rc<Shared>,
    /// The system time zone, read when the first clock starts.
    zone: RefCell<Option<TimeZone>>,
    started: RefCell<Vec<Clock>>,
}

struct Clock {
    precision: Precision,
    signal: Signal<Zoned>,
}

/// `cx.clock(precision)`.
pub(crate) fn clock(cx: &Cx, precision: Precision) -> ReadSignal<Zoned> {
    if let Some(clocks) = cx.service::<Clocks>() {
        return clocks.get(precision);
    }
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
        tracing::warn!("cx.clock() needs the services the shell provides; this clock stands still");
    });
    let (shown, _) = boundaries(Timestamp::now(), precision);
    cx.signal(shown.to_zoned(TimeZone::system())).read_only()
}

impl Clocks {
    pub(crate) fn new(shared: Rc<Shared>) -> Clocks {
        Clocks {
            shared,
            zone: RefCell::new(None),
            started: RefCell::new(Vec::new()),
        }
    }

    /// The clock for `precision`, started if it is the first ask.
    fn get(self: &Rc<Self>, precision: Precision) -> ReadSignal<Zoned> {
        let started = self.started.borrow();
        if let Some(clock) = started.iter().find(|clock| clock.precision == precision) {
            return clock.signal.read_only();
        }
        drop(started);
        let (shown, deadline) = current(precision, &self.zone());
        let signal = self.shared.root.signal(shown);
        self.timer(precision, deadline);
        self.started.borrow_mut().push(Clock { precision, signal });
        signal.read_only()
    }

    /// A timer that ticks the `precision` clock at `deadline`, and then at
    /// every boundary after it.
    fn timer(
        self: &Rc<Self>,
        precision: Precision,
        deadline: Instant,
    ) -> Option<RegistrationToken> {
        let clocks = Rc::clone(self);
        self.shared
            .event_loop()?
            .insert_timer(deadline, Box::new(move || clocks.tick(precision)))
    }

    /// Shows the boundary just passed on the `precision` clock and says
    /// when to tick next. Stops once the clock's signal is disposed.
    fn tick(&self, precision: Precision) -> TimeoutAction {
        let (shown, deadline) = current(precision, &self.zone());
        let started = self.started.borrow();
        let clock = started.iter().find(|clock| clock.precision == precision);
        match clock {
            Some(clock) if clock.signal.try_set(shown) => TimeoutAction::ToInstant(deadline),
            _ => TimeoutAction::Drop,
        }
    }

    fn zone(&self) -> TimeZone {
        let localtime = self.shared.config.localtime.as_deref();
        let mut zone = self.zone.borrow_mut();
        zone.get_or_insert_with(|| system_zone(localtime)).clone()
    }
}

/// What a `precision` clock shows now in `zone`, and the instant its timer
/// should fire next.
fn current(precision: Precision, zone: &TimeZone) -> (Zoned, Instant) {
    let now = Timestamp::now();
    let (shown, next) = boundaries(now, precision);
    let wait = Duration::try_from(next.duration_since(now)).unwrap_or_default();
    // Read after the wall clock, so the deadline errs late, never early.
    (shown.to_zoned(zone.clone()), Instant::now() + wait)
}

/// The boundary `now` has just passed, and the one after it. A `now` up to
/// [`EARLY`] before a boundary counts as on it, so a timer that fires a
/// hair early shows the new value instead of waking again ms later.
fn boundaries(now: Timestamp, precision: Precision) -> (Timestamp, Timestamp) {
    let step = precision.seconds();
    let shown = (now + EARLY).as_second().div_euclid(step) * step;
    let at = |second| {
        Timestamp::from_second(second)
            .expect("the system clock is within jiff's years -9999 to 9999")
    };
    (at(shown), at(shown + step))
}

/// The system time zone, read afresh each call. jiff's
/// `TimeZone::system()` keeps its answer for 5 minutes, so the
/// `/etc/localtime` link that timedated rewrites is read here. A copied
/// file, or `TZ` (`localtime` is `None`), is left to jiff.
fn system_zone(localtime: Option<&Path>) -> TimeZone {
    let target = localtime.and_then(|path| std::fs::read_link(path).ok());
    let name = target
        .as_deref()
        .and_then(Path::to_str)
        .and_then(|target| target.rsplit_once("zoneinfo/"))
        .map(|(_, name)| name);
    match name.map(|name| jiff::tz::db().get(name)) {
        Some(Ok(zone)) => zone,
        _ => TimeZone::system(),
    }
}
