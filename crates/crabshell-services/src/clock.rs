//! The clock: the time, truncated to a whole second or minute.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::poll_fn;
use std::path::Path;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;
use std::task::Poll;
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};
use zbus::export::futures_core::Stream;
use zbus::{MatchRule, Message, MessageStream};

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
    /// Its timer in the event loop, set for the next boundary.
    timer: Option<RegistrationToken>,
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
        let first = started.is_empty();
        drop(started);
        if first && let Some(bus) = self.shared.config.system_bus.clone() {
            self.shared.spawn(watch(Rc::clone(self), bus));
        }
        let (shown, deadline) = current(precision, &self.zone());
        let signal = self.shared.root.signal(shown);
        let timer = self.timer(precision, deadline);
        self.started.borrow_mut().push(Clock {
            precision,
            signal,
            timer,
        });
        signal.read_only()
    }

    /// Shows the right time at once and sets every timer afresh from the
    /// wall clock. Timers count monotonic time, which stops in a suspend.
    fn resync(self: &Rc<Self>) {
        let zone = self.zone();
        let Some(event_loop) = self.shared.event_loop() else {
            return;
        };
        for clock in self.started.borrow_mut().iter_mut() {
            let (shown, deadline) = current(clock.precision, &zone);
            clock.signal.try_set(shown);
            if let Some(old) = clock.timer.take() {
                event_loop.remove(old);
            }
            clock.timer = self.timer(clock.precision, deadline);
        }
    }

    /// Acts on a signal from logind or timedated. A bad one is logged.
    fn on_message(self: &Rc<Self>, message: &Message) {
        let body = message.body();
        match message.header().member().map(|member| member.as_str()) {
            Some("PrepareForSleep") => match body.deserialize::<bool>() {
                Ok(true) => {}
                Ok(false) => self.resync(),
                Err(error) => tracing::warn!(%error, "a bad PrepareForSleep from logind"),
            },
            Some("PropertiesChanged") => {
                match body.deserialize::<(&str, HashMap<&str, zbus::Variant<'_>>, Vec<&str>)>() {
                    Ok((_, changed, invalidated))
                        if changed.contains_key("Timezone")
                            || invalidated.contains(&"Timezone") =>
                    {
                        let zone = system_zone(self.shared.config.localtime.as_deref());
                        tracing::info!(zone = zone.iana_name(), "the time zone changed");
                        *self.zone.borrow_mut() = Some(zone);
                        self.resync();
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "a bad PropertiesChanged from timedated"),
                }
            }
            _ => {}
        }
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

/// Re-syncs `clocks` when the machine resumes and when timedated changes
/// the time zone, for as long as the bus is there.
async fn watch(clocks: Rc<Clocks>, bus: zbus::Address) {
    let (mut sleep, mut zone) = match subscribe(bus).await {
        Ok(streams) => streams,
        Err(error) => {
            tracing::info!(
                %error,
                "no system bus: the clock won't re-sync after a suspend or a time zone change"
            );
            return;
        }
    };
    loop {
        let next = poll_fn(|cx| match Pin::new(&mut sleep).poll_next(cx) {
            Poll::Pending => Pin::new(&mut zone).poll_next(cx),
            ready => ready,
        });
        match next.await {
            Some(Ok(message)) => clocks.on_message(&message),
            Some(Err(error)) => tracing::warn!(%error, "a bad message on the system bus"),
            None => {
                tracing::info!("the system bus closed: the clock no longer re-syncs");
                return;
            }
        }
    }
}

/// Connects to the system bus and subscribes to logind's `PrepareForSleep`
/// and timedated's `PropertiesChanged`. Match rules only: a proxy would
/// read properties first, which starts logind and timedated if they are
/// not running.
async fn subscribe(bus: zbus::Address) -> zbus::Result<(MessageStream, MessageStream)> {
    let connection = zbus::connection::Builder::address(bus).build().await?;
    let signal = || MatchRule::builder().msg_type(zbus::message::Type::Signal);
    let sleep = signal()
        .sender("org.freedesktop.login1")
        .path("/org/freedesktop/login1")
        .interface("org.freedesktop.login1.Manager")
        .member("PrepareForSleep")
        .build()?;
    let zone = signal()
        .sender("org.freedesktop.timedate1")
        .path("/org/freedesktop/timedate1")
        .interface("org.freedesktop.DBus.Properties")
        .member("PropertiesChanged")
        .arg(0, "org.freedesktop.timedate1")
        .build()?;
    Ok((
        MessageStream::for_match_rule(sleep, &connection, None).await?,
        MessageStream::for_match_rule(zone, &connection, None).await?,
    ))
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
