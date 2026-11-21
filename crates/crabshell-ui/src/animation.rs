//! Animations: values that move toward a target over a [`Transition`],
//! and the clock the platform advances only while one is running, so a
//! shell where nothing moves wakes for nothing.

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::{Owner, ReadSignal, Signal, untrack};

use crate::cx::Cx;
use crate::geometry::Point;
use crate::input::panic_message;
use crate::prop::{IntoProp, Prop, PropKind};
use crate::style::Color;

/// How an animation moves from its start to its end: what fraction of
/// the way it has gone at each fraction of its time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Easing {
  /// The same speed all the way.
  Linear,
  /// Starts slow.
  InQuad,
  /// Ends slow.
  OutQuad,
  /// Starts and ends slow.
  InOutQuad,
  /// Starts slower.
  InCubic,
  /// Ends slower. The default: it settles gently, as most interface
  /// motion should.
  #[default]
  OutCubic,
  /// Starts and ends slower.
  InOutCubic,
  /// Starts slow, along a quarter sine wave.
  InSine,
  /// Ends slow, along a quarter sine wave.
  OutSine,
  /// Starts and ends slow, along half a sine wave.
  InOutSine,
  /// Starts slower than a cubic.
  InQuart,
  /// Ends slower than a cubic.
  OutQuart,
  /// Starts and ends slower than a cubic.
  InOutQuart,
  /// Starts slower still.
  InQuint,
  /// Ends slower still.
  OutQuint,
  /// Starts and ends slower still.
  InOutQuint,
  /// Barely moves, then rushes in: each tenth of the time doubles it.
  InExpo,
  /// Rushes out, then barely moves.
  OutExpo,
  /// Barely moves at both ends.
  InOutExpo,
  /// Starts slow along a quarter circle.
  InCirc,
  /// Ends slow along a quarter circle.
  OutCirc,
  /// Starts and ends slow along quarter circles.
  InOutCirc,
  /// Pulls back about 10% behind the start, then goes.
  InBack,
  /// Runs about 10% past the end, then settles back.
  OutBack,
  /// Pulls back at the start and runs past the end.
  InOutBack,
  /// A cubic Bézier from (0, 0) to (1, 1) through the control points
  /// `(x1, y1)` and `(x2, y2)`, as CSS's `cubic-bezier`. The x values are
  /// kept within 0 to 1.
  Bezier(f32, f32, f32, f32),
  /// Cubic Bézier segments joined end to end, as Qt's
  /// `Easing.BezierSpline`. Each is `[c1x, c1y, c2x, c2y, end_x, end_y]`:
  /// two control points and an end, starting from the previous end, the
  /// first from (0, 0). The last must end at (1, 1), and the ends' x
  /// values must never go back. A spline that breaks either rule, is
  /// empty, or holds a number that isn't finite moves linearly instead.
  /// A control point's x is kept within its segment.
  ///
  /// Material 3's emphasized curve:
  ///
  /// ```
  /// use crabshell_ui::{Easing, Transition};
  ///
  /// const EMPHASIZED: Easing = Easing::Spline(&[
  ///     [0.05, 0.0, 0.133333, 0.06, 0.166666, 0.4],
  ///     [0.208333, 0.82, 0.25, 1.0, 1.0, 1.0],
  /// ]);
  /// const ENTER: Transition = Transition::new(500).easing(EMPHASIZED);
  /// ```
  Spline(&'static [[f32; 6]]),
}

impl Easing {
  /// How far along the animation is, from 0 to 1 (Back, a Bézier or a
  /// spline may overshoot either end), when `t` of its time has passed.
  /// `t` is kept within 0 to 1.
  pub fn value_at(self, t: f32) -> f32 {
    use std::f32::consts::PI;
    // Back's overshoot, the usual constant: about 10%.
    const C1: f32 = 1.70158;
    const C2: f32 = C1 * 1.525;
    let t = if t.is_nan() { 1.0 } else { t.clamp(0.0, 1.0) };
    match self {
      Easing::Linear => t,
      Easing::InQuad => t * t,
      Easing::OutQuad => 1.0 - (1.0 - t) * (1.0 - t),
      Easing::InOutQuad if t < 0.5 => 2.0 * t * t,
      Easing::InOutQuad => 1.0 - (-2.0 * t + 2.0).powi(2) / 2.0,
      Easing::InCubic => t * t * t,
      Easing::OutCubic => 1.0 - (1.0 - t).powi(3),
      Easing::InOutCubic if t < 0.5 => 4.0 * t * t * t,
      Easing::InOutCubic => 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0,
      Easing::InSine => 1.0 - (t * PI / 2.0).cos(),
      Easing::OutSine => (t * PI / 2.0).sin(),
      Easing::InOutSine => (1.0 - (PI * t).cos()) / 2.0,
      Easing::InQuart => t.powi(4),
      Easing::OutQuart => 1.0 - (1.0 - t).powi(4),
      Easing::InOutQuart if t < 0.5 => 8.0 * t.powi(4),
      Easing::InOutQuart => 1.0 - (-2.0 * t + 2.0).powi(4) / 2.0,
      Easing::InQuint => t.powi(5),
      Easing::OutQuint => 1.0 - (1.0 - t).powi(5),
      Easing::InOutQuint if t < 0.5 => 16.0 * t.powi(5),
      Easing::InOutQuint => 1.0 - (-2.0 * t + 2.0).powi(5) / 2.0,
      // The formulas miss their ends by 2^-10, so the ends are exact.
      Easing::InExpo | Easing::OutExpo | Easing::InOutExpo if t == 0.0 || t == 1.0 => t,
      Easing::InExpo => 2_f32.powf(10.0 * t - 10.0),
      Easing::OutExpo => 1.0 - 2_f32.powf(-10.0 * t),
      Easing::InOutExpo if t < 0.5 => 2_f32.powf(20.0 * t - 10.0) / 2.0,
      Easing::InOutExpo => (2.0 - 2_f32.powf(-20.0 * t + 10.0)) / 2.0,
      Easing::InCirc => 1.0 - (1.0 - t * t).sqrt(),
      Easing::OutCirc => (1.0 - (t - 1.0).powi(2)).sqrt(),
      Easing::InOutCirc if t < 0.5 => (1.0 - (1.0 - (2.0 * t).powi(2)).sqrt()) / 2.0,
      Easing::InOutCirc => ((1.0 - (-2.0 * t + 2.0).powi(2)).sqrt() + 1.0) / 2.0,
      Easing::InBack => (C1 + 1.0) * t.powi(3) - C1 * t * t,
      Easing::OutBack => 1.0 + (C1 + 1.0) * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2),
      Easing::InOutBack if t < 0.5 => (2.0 * t).powi(2) * ((C2 + 1.0) * 2.0 * t - C2) / 2.0,
      Easing::InOutBack => {
        ((2.0 * t - 2.0).powi(2) * ((C2 + 1.0) * (2.0 * t - 2.0) + C2) + 2.0) / 2.0
      }
      Easing::Bezier(x1, y1, x2, y2) => bezier(t, (0.0, 0.0), [x1, y1, x2, y2, 1.0, 1.0]),
      Easing::Spline(segments) => spline(t, segments),
    }
  }
}

/// A spline's y where its x is `x`, or `x` itself for a spline that
/// breaks the rules [`Easing::Spline`] gives.
fn spline(x: f32, segments: &[[f32; 6]]) -> f32 {
  let mut end = 0.0;
  let valid = segments.iter().all(|segment| {
    let forward = segment[4] >= end;
    end = segment[4];
    forward && segment.iter().all(|n| n.is_finite())
  }) && segments
    .last()
    .is_some_and(|last| last[4] == 1.0 && last[5] == 1.0);
  if !valid {
    return x;
  }
  let mut start = (0.0, 0.0);
  for segment in segments {
    if x <= segment[4] {
      return bezier(x, start, *segment);
    }
    start = (segment[4], segment[5]);
  }
  1.0
}

/// The y of a cubic Bézier from `start` through the control points and
/// to the end in `segment` (laid out as in [`Easing::Spline`]) at the
/// point whose x is `x`, found by bisection: x grows with the curve's
/// parameter once both control x values are kept between the ends.
fn bezier(x: f32, (x0, y0): (f32, f32), segment: [f32; 6]) -> f32 {
  let [x1, y1, x2, y2, x3, y3] = segment;
  let (x1, x2) = (x1.clamp(x0, x3), x2.clamp(x0, x3));
  let at = |p0: f32, p1: f32, p2: f32, p3: f32, s: f32| {
    let r = 1.0 - s;
    r * r * r * p0 + 3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s * p3
  };
  let (mut low, mut high) = (0.0_f32, 1.0_f32);
  for _ in 0..32 {
    let middle = (low + high) / 2.0;
    if at(x0, x1, x2, x3, middle) < x {
      low = middle;
    } else {
      high = middle;
    }
  }
  at(y0, y1, y2, y3, (low + high) / 2.0)
}

/// How long an animation takes, how it moves, and how long it waits
/// first. Copy it into styles and constants like a color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
  /// The time it takes, in milliseconds. 0 with no delay jumps straight
  /// to the end in the flush that sets the new target, so the same turn's
  /// layout and draw already show it and the clock never runs.
  pub ms: u32,
  /// How it moves.
  pub easing: Easing,
  /// How long it waits before it moves, in milliseconds.
  pub delay: u32,
  /// Whether a [`Cx::cycle`] comes back each round instead of starting
  /// over. Nothing else reads it.
  pub alternate: bool,
}

impl Transition {
  /// A transition of `ms` milliseconds, easing out, with no delay.
  pub const fn new(ms: u32) -> Transition {
    Transition {
      ms,
      easing: Easing::OutCubic,
      delay: 0,
      alternate: false,
    }
  }

  /// The same transition moving as `easing` says.
  pub const fn easing(self, easing: Easing) -> Transition {
    Transition { easing, ..self }
  }

  /// The same transition waiting `ms` milliseconds before it moves.
  pub const fn delay(self, ms: u32) -> Transition {
    Transition { delay: ms, ..self }
  }

  /// The same transition coming back after each round of a
  /// [`Cx::cycle`], so it goes 0 to 1 to 0, each way taking `ms`.
  pub const fn alternate(self) -> Transition {
    Transition {
      alternate: true,
      ..self
    }
  }

  /// Whether it takes no time at all, so the new value shows at once.
  pub(crate) fn is_instant(self) -> bool {
    self.ms == 0 && self.delay == 0
  }

  /// Whether an animation that started at `start` is still waiting out
  /// its delay at `now`, so nothing it moves has changed yet.
  pub(crate) fn waiting(self, start: Instant, now: Instant) -> bool {
    now.saturating_duration_since(start) < Duration::from_millis(self.delay.into())
  }

  /// How far along an animation that started at `start` is at `now`,
  /// eased, and whether it has ended. It stays at 0 through the delay.
  pub(crate) fn progress(self, start: Instant, now: Instant) -> (f32, bool) {
    let elapsed = now.saturating_duration_since(start).as_secs_f32() * 1000.0 - self.delay as f32;
    if elapsed < 0.0 {
      return (0.0, false);
    }
    if self.ms == 0 {
      return (1.0, true);
    }
    let t = elapsed / self.ms as f32;
    if t >= 1.0 {
      (1.0, true)
    } else {
      (self.easing.value_at(t), false)
    }
  }
}

/// The spring a [`Cx::spring`] value follows its target on, as Qt's
/// `SpringAnimation`. Time is in seconds and distance in the value's own
/// units. The target pulls the value with `stiffness` times the distance,
/// per second squared, and friction slows it as `damping` says, as a
/// ratio: 1 comes to rest as fast as it can without passing the target,
/// less overshoots and bounces, more creeps in. The default, 700 and 0.9,
/// is Material 3's default spatial spring; its expressive one is 380 and
/// 0.8.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
  /// How hard the target pulls, per second squared: higher is faster.
  pub stiffness: f32,
  /// The damping ratio: 1 never passes the target, less bounces.
  pub damping: f32,
}

impl Spring {
  /// A spring of `stiffness` and `damping` ratio.
  pub const fn new(stiffness: f32, damping: f32) -> Spring {
    Spring { stiffness, damping }
  }

  /// Whether it can move anything: a stiffness above 0, a damping of 0
  /// or more, both finite.
  fn is_valid(self) -> bool {
    self.stiffness.is_finite()
      && self.stiffness > 0.0
      && self.damping.is_finite()
      && self.damping >= 0.0
  }

  /// Moves the value `x`, with speed `v`, toward `goal` over `seconds`,
  /// in implicit Euler steps of at most a millisecond, which stay stable
  /// whatever the spring and the step.
  pub(crate) fn advance(self, x: &mut f64, v: &mut f64, goal: f64, seconds: f64) {
    let k = f64::from(self.stiffness);
    let c = 2.0 * f64::from(self.damping) * k.sqrt();
    let steps = (seconds * 1000.0).ceil().max(1.0);
    let dt = seconds / steps;
    for _ in 0..steps as u32 {
      *v = (*v - dt * k * (*x - goal)) / (1.0 + dt * c + dt * dt * k);
      *x += dt * *v;
    }
  }
}

impl Default for Spring {
  fn default() -> Spring {
    Spring::new(700.0, 0.9)
  }
}

/// The most time one tick moves a spring: a frame that comes late moves
/// it this far and no more, so a stall can't cost a long run of steps.
const SPRING_MAX_STEP: f64 = 0.25;

/// The distance from its target a spring counts as at rest within, and the
/// speed per second below which it counts as still.
const SPRING_REST: (f64, f64) = (0.001, 0.01);

/// One step of a [`Cx::sequence`]: a move to a value over a
/// [`Transition`], or a hold that keeps the value for a while. Steps are
/// built in `const`s, so a config keeps a whole sequence in a `const`
/// array.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step<T> {
  to: Option<T>,
  transition: Transition,
}

impl<T> Step<T> {
  /// A move from wherever the value is to `value`, over `transition`,
  /// after its delay.
  pub const fn to(value: T, transition: Transition) -> Step<T> {
    Step {
      to: Some(value),
      transition,
    }
  }

  /// A pause of `ms` milliseconds that keeps the value where it is, as
  /// Qt's `PauseAnimation`.
  pub const fn hold(ms: u32) -> Step<T> {
    Step {
      to: None,
      transition: Transition::new(0).delay(ms),
    }
  }

  /// When it starts to move and when it ends, in microseconds after it
  /// begins. A hold never moves, so both are its end.
  fn times(&self) -> (u64, u64) {
    let moves = u64::from(self.transition.delay) * 1000;
    (moves, moves + u64::from(self.transition.ms) * 1000)
  }
}

impl<T: Animate> IntoProp<Vec<Step<T>>> for Vec<Step<T>> {
  fn into_prop(self) -> Prop<Vec<Step<T>>> {
    Prop(PropKind::Value(self))
  }
}

/// A fixed array of steps, such as a `const` sequence.
impl<T: Animate, const N: usize> IntoProp<Vec<Step<T>>> for [Step<T>; N] {
  fn into_prop(self) -> Prop<Vec<Step<T>>> {
    Prop(PropKind::Value(self.into()))
  }
}

/// Where a play of a sequence is at a time.
enum Spot<T> {
  /// Moving, here.
  Moving(T),
  /// Holding here until the time given, in microseconds, when the value
  /// next changes.
  Holding(T, u64),
  /// Past its last step, here.
  Ended(T),
}

/// Where a play of `steps`, begun with the value at `from`, is `at`
/// microseconds in.
fn spot<T: Animate>(steps: &[Step<T>], from: &T, at: u64) -> Spot<T> {
  let mut from = from.clone();
  let mut begins = 0;
  for (i, step) in steps.iter().enumerate() {
    let (moves, ends) = step.times();
    if at < begins + moves {
      // A pause goes on through the holds and delays right after it.
      let mut until = begins + moves;
      if step.to.is_none() {
        for next in &steps[i + 1..] {
          until += next.times().0;
          if next.to.is_some() {
            break;
          }
        }
      }
      return Spot::Holding(from, until);
    }
    if let Some(to) = &step.to {
      if at < begins + ends {
        let t = (at - begins - moves) as f32 / (ends - moves) as f32;
        return Spot::Moving(from.lerp(to, step.transition.easing.value_at(t)));
      }
      from = to.clone();
    }
    begins += ends;
  }
  Spot::Ended(from)
}

/// A value an animation can move between two of: `lerp` gives the value
/// `t` of the way from `self` to `to`, where `t` may stray a little past 0
/// or 1 for an easing that overshoots.
pub trait Animate: Clone + PartialEq + 'static {
  /// The value `t` of the way from `self` to `to`.
  fn lerp(&self, to: &Self, t: f32) -> Self;
}

impl Animate for f32 {
  fn lerp(&self, to: &f32, t: f32) -> f32 {
    self + (to - self) * t
  }
}

impl Animate for Point {
  fn lerp(&self, to: &Point, t: f32) -> Point {
    Point::new(self.x.lerp(&to.x, t), self.y.lerp(&to.y, t))
  }
}

impl Animate for Color {
  fn lerp(&self, to: &Color, t: f32) -> Color {
    let channel = |a: u8, b: u8| {
      f32::from(a)
        .lerp(&f32::from(b), t)
        .round()
        .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
      channel(self.r, to.r),
      channel(self.g, to.g),
      channel(self.b, to.b),
      channel(self.a, to.a),
    )
  }
}

/// One running animation: advanced to a time, it says whether it still
/// runs.
type Tick = Box<dyn FnMut(Instant) -> bool>;

/// A step on the clock, named for log lines and counted in its item.
type Running = (Rc<str>, Option<Busy>, Tick);

/// How many animations run inside one item of a list with an exit, and
/// the item around it, so a leaving item knows when everything in it has
/// stopped.
pub(crate) struct Activity {
  running: Cell<usize>,
  outer: Option<Rc<Activity>>,
}

impl Activity {
  pub(crate) fn new(outer: Option<Rc<Activity>>) -> Rc<Activity> {
    Rc::new(Activity {
      running: Cell::new(0),
      outer,
    })
  }

  /// Adds `by` to this item's count and every item's around it.
  fn add(&self, by: isize) {
    let mut at = Some(self);
    while let Some(activity) = at {
      activity
        .running
        .set(activity.running.get().wrapping_add_signed(by));
      at = activity.outer.as_deref();
    }
  }
}

/// One animation counted as running in its item until it is dropped.
pub(crate) struct Busy(Rc<Activity>);

impl Busy {
  pub(crate) fn new(activity: &Rc<Activity>) -> Busy {
    activity.add(1);
    Busy(Rc::clone(activity))
  }
}

impl Drop for Busy {
  fn drop(&mut self) {
    self.0.add(-1);
  }
}

/// A leaving item waiting for everything in it to stop: `done` runs at
/// the first tick that finds nothing running there, unless `owner`, its
/// exit's, went first.
struct Watch {
  owner: Owner,
  activity: Rc<Activity>,
  done: Box<dyn FnOnce()>,
}

thread_local! {
  /// Everything is on one thread, so the clock is too. Each animation is
  /// named for log lines.
  static RUNNING: RefCell<Vec<Running>> = const { RefCell::new(Vec::new()) };
  static WATCHES: RefCell<Vec<Watch>> = const { RefCell::new(Vec::new()) };
  /// Steps taken off the clock through a pause. The timers wake them.
  static ASLEEP: RefCell<Vec<Sleeper>> = const { RefCell::new(Vec::new()) };
  /// What the step now running asked with [`sleep_until`].
  static SLEEP: Cell<Option<Nap>> = const { Cell::new(None) };
  /// When the clock last ticked.
  static LAST_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Starts `step`, named `label` for log lines and counted in `busy`'s
/// item, at the next [`advance_animations`].
pub(crate) fn start(label: Rc<str>, busy: Option<Busy>, step: Tick) {
  RUNNING.with(|running| running.borrow_mut().push((label, busy, step)));
}

/// A pause a step asked for: until when, the owner whose going frees it,
/// and the play it belongs to, as its generation cell and the number it
/// started at, so a play replaced while asleep is freed too.
pub(crate) struct Nap {
  pub(crate) deadline: Instant,
  pub(crate) owner: Owner,
  pub(crate) play: (Rc<Cell<u64>>, u64),
}

/// A step asleep through its pause.
struct Sleeper {
  nap: Nap,
  running: Running,
}

impl Nap {
  /// Whether nothing waits for it any more.
  fn stale(&self) -> bool {
    self.owner.is_disposed() || self.play.0.get() != self.play.1
  }
}

/// Takes the step now running off the clock once it returns `true`, until
/// the nap's deadline, so a pause wakes the loop once instead of every
/// frame. It stays counted in its item, and is freed unwoken once stale.
pub(crate) fn sleep_until(nap: Nap) {
  SLEEP.with(|sleep| sleep.set(Some(nap)));
}

/// Frees the sleeping steps whose owner is gone or whose play was
/// replaced, dropping them with the table released.
fn sweep_asleep() {
  let gone = ASLEEP.with(|asleep| {
    let mut asleep = asleep.borrow_mut();
    let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *asleep)
      .into_iter()
      .partition(|sleeper| sleeper.nap.stale());
    *asleep = kept;
    gone
  });
  for Sleeper {
    running: (label, busy, step),
    ..
  } in gone
  {
    drop(busy);
    drop_isolated(&label, step);
  }
}

/// When the first sleeping step wakes, having freed the stale ones.
pub(crate) fn next_wake() -> Option<Instant> {
  sweep_asleep();
  ASLEEP.with(|asleep| {
    let asleep = asleep.borrow();
    asleep.iter().map(|sleeper| sleeper.nap.deadline).min()
  })
}

/// Puts the sleeping steps due at `now` back on the clock, having freed
/// the stale ones.
pub(crate) fn wake(now: Instant) {
  sweep_asleep();
  let due: Vec<Running> = ASLEEP.with(|asleep| {
    let mut asleep = asleep.borrow_mut();
    let (due, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut *asleep)
      .into_iter()
      .partition(|sleeper| sleeper.nap.deadline <= now);
    *asleep = kept;
    due.into_iter().map(|sleeper| sleeper.running).collect()
  });
  RUNNING.with(|running| running.borrow_mut().extend(due));
}

/// How many steps sleep, for tests.
#[cfg(test)]
pub(crate) fn asleep() -> usize {
  ASLEEP.with(|asleep| asleep.borrow().len())
}

/// Runs `done` at the first tick that finds nothing running in
/// `activity`'s item, while `owner` lasts. The clock ticks for it, so an
/// item with nothing moving is judged at once.
pub(crate) fn watch(owner: Owner, activity: Rc<Activity>, done: impl FnOnce() + 'static) {
  WATCHES.with(|watches| {
    watches.borrow_mut().push(Watch {
      owner,
      activity,
      done: Box::new(done),
    });
  });
}

/// Runs the watches whose items have stopped. Judged before the steps
/// move, so an animation started in the flush after the last tick, as
/// one that follows another's end, still counts.
fn judge_watches() {
  let watches = WATCHES.with(|watches| std::mem::take(&mut *watches.borrow_mut()));
  let mut kept = Vec::with_capacity(watches.len());
  for watch in watches {
    if watch.owner.is_disposed() {
      continue;
    }
    if watch.activity.running.get() == 0 {
      (watch.done)();
    } else {
      kept.push(watch);
    }
  }
  WATCHES.with(|watches| {
    let mut watches = watches.borrow_mut();
    kept.append(&mut watches);
    *watches = kept;
  });
}

/// When an animation starting now should count from: the clock's last
/// tick while it runs, so one that replaces another moves at the very
/// next tick, or `None` when it is still, so the first tick is time 0.
pub(crate) fn start_time() -> Option<Instant> {
  if animating() {
    LAST_TICK.with(Cell::get)
  } else {
    None
  }
}

/// Whether an animation is running, so the platform should advance the
/// clock soon. `false` means nothing moves and nothing needs waking.
pub fn animating() -> bool {
  RUNNING.with(|running| !running.borrow().is_empty())
    || WATCHES.with(|watches| !watches.borrow().is_empty())
}

/// Advances every running animation to `now`, writing their values, and
/// returns whether any still runs. Each one starts at the first call after
/// it began, so `now` there is its time 0. One that panics is logged and
/// dropped.
pub fn advance_animations(now: Instant) -> bool {
  LAST_TICK.with(|last| last.set(Some(now)));
  judge_watches();
  let steps = RUNNING.with(|running| std::mem::take(&mut *running.borrow_mut()));
  let mut kept = Vec::with_capacity(steps.len());
  for (label, busy, mut step) in steps {
    let ran = panic::catch_unwind(AssertUnwindSafe(|| step(now)));
    let sleep = SLEEP.with(Cell::take);
    match ran {
      Ok(true) => match sleep {
        Some(nap) => {
          let running = (label, busy, step);
          ASLEEP.with(|asleep| asleep.borrow_mut().push(Sleeper { nap, running }));
        }
        None => kept.push((label, busy, step)),
      },
      Ok(false) => drop_isolated(&label, step),
      Err(payload) => {
        tracing::error!(
          "animation {label} panicked and stops: {}",
          panic_message(payload.as_ref())
        );
        drop_isolated(&label, step);
      }
    }
  }
  RUNNING.with(|running| {
    // Animations started while these ran go after them.
    let mut running = running.borrow_mut();
    kept.append(&mut running);
    *running = kept;
  });
  let still = animating();
  if !still {
    // The clock stops, so the next animation counts from its own first
    // tick, not this one.
    LAST_TICK.with(|last| last.set(None));
  }
  still
}

/// Drops a finished step, and the user's values it holds, isolated.
fn drop_isolated(label: &str, step: Tick) {
  if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(step))) {
    tracing::error!(
      "dropping animation {label} panicked: {}",
      panic_message(payload.as_ref())
    );
  }
}

/// Runs `f`, a config's closure for the animation `label`, and logs a
/// panic as that animation's, giving `None`.
fn guarded<R>(label: &str, f: impl FnOnce() -> R) -> Option<R> {
  match panic::catch_unwind(AssertUnwindSafe(f)) {
    Ok(value) => Some(value),
    Err(payload) => {
      tracing::error!(
        "animation {label} panicked and stops: {}",
        panic_message(payload.as_ref())
      );
      None
    }
  }
}

/// Sets `signal` to `value` only when that changes it, since a set always
/// tells its readers.
fn set_changed<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}

/// A value moving toward its target, from [`Cx::animation`] or
/// [`Cx::spring`]: a `Copy`
/// handle to its value and to whether it moves.
pub struct Animation<T: 'static> {
  value: ReadSignal<T>,
  running: ReadSignal<bool>,
}

impl<T> Clone for Animation<T> {
  fn clone(&self) -> Self {
    *self
  }
}

impl<T> Copy for Animation<T> {}

impl<T> Animation<T> {
  /// The value, as [`Cx::animate`] gives it.
  pub fn value(&self) -> ReadSignal<T> {
    self.value
  }

  /// `true` from the moment a new target starts it, through its delay,
  /// until it settles there. It turns `false` once per animation, so an
  /// effect on it is the place for what follows.
  pub fn running(&self) -> ReadSignal<bool> {
    self.running
  }
}

impl Cx {
  /// A value that follows `target`, moving to each new one over
  /// `transition` instead of jumping. It starts at `target`'s first value.
  /// A new target while it moves starts a new animation from where it is.
  /// A 0 ms transition with no delay lands in the flush that changes the
  /// target, so the same turn's layout and draw already show it.
  /// The animation goes when this context's owner does. It is
  /// [`Cx::animation`]'s value.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Transition};
  ///
  /// fn slide(cx: &Cx) {
  ///     let open = cx.signal(false);
  ///     let offset = cx.animate(move || if open.get() { 0.0 } else { -200.0 }, Transition::new(200));
  ///     // Read `offset.get()` in a binding, such as a style's width.
  /// }
  /// ```
  #[track_caller]
  pub fn animate<T: Animate>(
    &self,
    target: impl FnMut() -> T + 'static,
    transition: impl IntoProp<Transition>,
  ) -> ReadSignal<T> {
    self.follow("cx.animate", target, transition).value
  }

  /// A value that follows `target` as [`Cx::animate`]'s does, and whether
  /// it moves. A panic in `target` is logged with this call's place, and
  /// the value stays where it was.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Transition};
  ///
  /// fn slide_in_then_focus(cx: &Cx) {
  ///     let open = cx.signal(false);
  ///     let ready = cx.signal(false);
  ///     let slide = cx.animation(move || if open.get() { 0.0 } else { -200.0 }, Transition::new(200));
  ///     cx.effect(move || {
  ///         if open.get() && !slide.running().get() {
  ///             ready.set(true);
  ///         }
  ///     });
  /// }
  /// ```
  #[track_caller]
  pub fn animation<T: Animate>(
    &self,
    target: impl FnMut() -> T + 'static,
    transition: impl IntoProp<Transition>,
  ) -> Animation<T> {
    self.follow("cx.animation", target, transition)
  }

  /// [`Cx::animation`], logged as `what` at the caller's place.
  #[track_caller]
  fn follow<T: Animate>(
    &self,
    what: &str,
    mut target: impl FnMut() -> T + 'static,
    transition: impl IntoProp<Transition>,
  ) -> Animation<T> {
    let label: Rc<str> = format!("{what} at {}", std::panic::Location::caller()).into();
    let transition = transition.into_prop().shared();
    let activity = self.activity.clone();
    let first = untrack(&mut target);
    let value = self.signal(first.clone());
    let running = self.signal(false);
    let mut last = first;
    // Bumped by each new target, so the animation it replaces stops.
    let generation = Rc::new(Cell::new(0_u64));
    let mut broken = false;
    self.effect(move || {
      if broken {
        return;
      }
      let Some(to) = guarded(&label, &mut target) else {
        broken = true;
        generation.set(generation.get() + 1);
        set_changed(running, false);
        return;
      };
      if to == last {
        return;
      }
      // Read as each move starts, so a binding can pick its curve.
      let Some(transition) = guarded(&label, || untrack(&*transition)) else {
        broken = true;
        generation.set(generation.get() + 1);
        set_changed(running, false);
        return;
      };
      last = to.clone();
      let Some(from) = untrack(|| value.try_get()) else {
        return;
      };
      let current = generation.get() + 1;
      generation.set(current);
      // No time to take: the new value shows in this flush.
      if transition.is_instant() {
        value.try_set(to);
        set_changed(running, false);
        return;
      }
      set_changed(running, true);
      let generation = Rc::clone(&generation);
      let mut started = start_time();
      let step_label = Rc::clone(&label);
      start(
        Rc::clone(&label),
        activity.as_ref().map(Busy::new),
        Box::new(move |now| {
          if generation.get() != current {
            return false;
          }
          let begun = *started.get_or_insert(now);
          if transition.waiting(begun, now) {
            // The value is still `from`; setting it would rerun its readers.
            return true;
          }
          let (t, ended) = transition.progress(begun, now);
          // `lerp` and `clone` may be a config's own; a panic ends the
          // animation with `running` false, so whatever waits on it runs.
          let Some(next) = guarded(&step_label, || {
            if ended { to.clone() } else { from.lerp(&to, t) }
          }) else {
            running.try_set(false);
            return false;
          };
          // A value whose owner is gone stops its animation.
          let kept = value.try_set(next);
          if ended {
            running.try_set(false);
          }
          kept && !ended
        }),
      );
    });
    Animation {
      value: value.read_only(),
      running: running.read_only(),
    }
  }

  /// Progress from 0 to 1, eased, repeating every `transition.ms` while
  /// `running` gives `true`, for spinners and pulses. With
  /// [`Transition::alternate`] it comes back, 0 to 1 to 0, each way taking
  /// `ms`. It waits out the delay once, at the start. When `running` turns
  /// `false`, the round it is in runs to its end, then it rests at 0 and
  /// the clock stops; `true` again before then keeps it going, and after
  /// it begins at 0. A 0 ms cycle never moves. A panic in `running` is
  /// logged with this call's place and ends the cycle after its round.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Easing, Transition};
  ///
  /// const SPIN: Transition = Transition::new(1000).easing(Easing::Linear);
  /// const PULSE: Transition = Transition::new(600).easing(Easing::InOutSine).alternate();
  ///
  /// fn busy(cx: &Cx) {
  ///     let loading = cx.signal(true);
  ///     let turn = cx.cycle(move || loading.get(), SPIN); // turns: turn.get() * 360.0
  ///     let glow = cx.cycle(move || loading.get(), PULSE); // an opacity: 0.5 + glow.get() / 2.0
  /// }
  /// ```
  #[track_caller]
  pub fn cycle(
    &self,
    mut running: impl FnMut() -> bool + 'static,
    transition: Transition,
  ) -> ReadSignal<f32> {
    let label: Rc<str> = format!("cx.cycle at {}", std::panic::Location::caller()).into();
    let activity = self.activity.clone();
    let value = self.signal(0.0_f32);
    // What `running` last gave, and whether a round is on the clock.
    let wanted = Rc::new(Cell::new(false));
    let ticking = Rc::new(Cell::new(false));
    let mut broken = false;
    self.effect(move || {
      let on = !broken
        && guarded(&label, &mut running).unwrap_or_else(|| {
          broken = true;
          false
        });
      wanted.set(on);
      if !on || ticking.get() || transition.ms == 0 {
        return;
      }
      ticking.set(true);
      let (wanted, ticking) = (Rc::clone(&wanted), Rc::clone(&ticking));
      let leg = f64::from(transition.ms);
      let period = if transition.alternate { 2.0 * leg } else { leg };
      let mut started = start_time();
      // Round 0 is the delay, then 1, 2 and on.
      let mut round = 0_u64;
      start(
        Rc::clone(&label),
        activity.as_ref().map(Busy::new),
        Box::new(move |now| {
          let begun = *started.get_or_insert(now);
          let elapsed = now.saturating_duration_since(begun).as_secs_f64() * 1000.0
            - f64::from(transition.delay);
          let now_round = if elapsed < 0.0 {
            0
          } else {
            (elapsed / period) as u64 + 1
          };
          let ended = !wanted.get() && (now_round == 0 || now_round > round);
          round = now_round;
          if ended {
            ticking.set(false);
            value.try_set(0.0);
            return false;
          }
          let phase = elapsed.max(0.0) % period / leg;
          let t = if phase <= 1.0 { phase } else { 2.0 - phase };
          value.try_set(transition.easing.value_at(t as f32))
        }),
      );
    });
    value.read_only()
  }

  /// The time since `running` turned `true`, updated at each tick of the
  /// animation clock while it stays true, as Qt's `FrameAnimation` gives.
  /// Nothing ticks while it is false, and the time stays where it was
  /// until the next run starts again from 0. A panic in `running` is
  /// logged with this call's place and stops it.
  ///
  /// ```
  /// use crabshell_ui::Cx;
  ///
  /// fn wave(cx: &Cx) {
  ///     let shown = cx.signal(true);
  ///     let time = cx.frames(move || shown.get());
  ///     // A bar's height: 10.0 + 5.0 * (time.get().as_secs_f32() * 6.0).sin()
  /// }
  /// ```
  #[track_caller]
  pub fn frames(&self, mut running: impl FnMut() -> bool + 'static) -> ReadSignal<Duration> {
    let label: Rc<str> = format!("cx.frames at {}", std::panic::Location::caller()).into();
    let activity = self.activity.clone();
    let value = self.signal(Duration::ZERO);
    let wanted = Rc::new(Cell::new(false));
    let ticking = Rc::new(Cell::new(false));
    let mut broken = false;
    self.effect(move || {
      let on = !broken
        && guarded(&label, &mut running).unwrap_or_else(|| {
          broken = true;
          false
        });
      wanted.set(on);
      if !on || ticking.get() {
        return;
      }
      ticking.set(true);
      let (wanted, ticking) = (Rc::clone(&wanted), Rc::clone(&ticking));
      let mut started = start_time();
      start(
        Rc::clone(&label),
        activity.as_ref().map(Busy::new),
        Box::new(move |now| {
          if !wanted.get() {
            ticking.set(false);
            return false;
          }
          let begun = *started.get_or_insert(now);
          value.try_set(now.saturating_duration_since(begun))
        }),
      );
    });
    value.read_only()
  }

  /// A value that follows `target` on `spring`, as Qt's
  /// `SpringAnimation`: a new target while it moves keeps its speed, so a
  /// value chasing a moving target never stops dead. It starts at
  /// `target`'s first value and comes to rest exactly on the target, when
  /// the clock stops. A target that isn't finite, or a spring that isn't
  /// valid (see [`Spring`]), jumps there at once. A panic in `target` is
  /// logged with this call's place, and the value stops where it is. The
  /// [`Animation`]'s `running()` is true from a new target until it comes
  /// to rest, and never for a jump.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Spring};
  ///
  /// fn indicator(cx: &Cx) {
  ///     let hovered = cx.signal(0_usize);
  ///     let x = cx.spring(move || hovered.get() as f32 * 40.0, Spring::new(380.0, 0.8));
  ///     // Read `x.value().get()` in a binding, such as a style's padding.
  /// }
  /// ```
  #[track_caller]
  pub fn spring(
    &self,
    mut target: impl FnMut() -> f32 + 'static,
    spring: Spring,
  ) -> Animation<f32> {
    let label: Rc<str> = format!("cx.spring at {}", std::panic::Location::caller()).into();
    let activity = self.activity.clone();
    let first = untrack(&mut target);
    let value = self.signal(first);
    let running = self.signal(false);
    let mut last = first;
    // Where the step on the clock heads, and whether there is one. A goal
    // that isn't finite stops it.
    let goal = Rc::new(Cell::new(first));
    let ticking = Rc::new(Cell::new(false));
    let mut broken = false;
    self.effect(move || {
      if broken {
        return;
      }
      let Some(to) = guarded(&label, &mut target) else {
        broken = true;
        goal.set(f32::NAN);
        set_changed(running, false);
        return;
      };
      if to == last {
        return;
      }
      last = to;
      goal.set(to);
      if !to.is_finite() || !spring.is_valid() {
        value.try_set(to);
        // One on the clock stops at its next tick.
        set_changed(running, false);
        return;
      }
      if ticking.get() {
        // Still on the clock after a jump, it moves again.
        set_changed(running, true);
        return;
      }
      let Some(from) = untrack(|| value.try_get()) else {
        return;
      };
      ticking.set(true);
      set_changed(running, true);
      let (goal, ticking) = (Rc::clone(&goal), Rc::clone(&ticking));
      let (mut x, mut v) = (f64::from(from), 0.0);
      let mut previous = start_time();
      start(
        Rc::clone(&label),
        activity.as_ref().map(Busy::new),
        Box::new(move |now| {
          let to = f64::from(goal.get());
          if !to.is_finite() {
            ticking.set(false);
            set_changed(running, false);
            return false;
          }
          let seconds = previous.map_or(0.0, |previous| {
            now
              .saturating_duration_since(previous)
              .as_secs_f64()
              .min(SPRING_MAX_STEP)
          });
          previous = Some(now);
          spring.advance(&mut x, &mut v, to, seconds);
          let (near, slow) = SPRING_REST;
          // A value that started out not finite lands at once.
          let rest = !(x.is_finite() && v.is_finite()) || ((x - to).abs() < near && v.abs() < slow);
          if rest {
            x = to;
          }
          let kept = value.try_set(x as f32);
          if rest || !kept {
            ticking.set(false);
            set_changed(running, false);
          }
          kept && !rest
        }),
      );
    });
    Animation {
      value: value.read_only(),
      running: running.read_only(),
    }
  }

  /// A value that plays `steps` one after another each time `trigger`'s
  /// value changes, as Qt's `SequentialAnimation`: a shake on a wrong
  /// password, bumped by a counter, or a pause and then a slide. It starts
  /// at `initial`, and `trigger`'s first value plays nothing. Each step
  /// moves from wherever the value is, so a new trigger mid-play starts
  /// over from there, and once played the value rests at the last step's
  /// target. `steps` is a value or a closure, read untracked as each play
  /// starts; an empty list plays nothing. The steps at the start that take
  /// no time, 0 ms with no delay, land in the flush where `trigger`
  /// changes, so the same turn's layout and draw already show the value
  /// they leave and the next step moves from there; a play of no time at
  /// all ends in that flush, and the clock never runs.
  ///
  /// The [`Animation`]'s `running()` is true from the trigger until the
  /// last step ends, holds included, so an effect on it runs what comes
  /// after. A hold, or a delay, takes the value off the animation clock:
  /// the loop sleeps until it ends instead of waking each frame. A panic in
  /// `trigger` is logged with this call's place and stops the sequence
  /// where it is for good; one in `steps` or a config's own `lerp`,
  /// `clone` or `PartialEq` stops that play.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Easing, Step, Transition};
  ///
  /// const SHAKE: [Step<f32>; 3] = [
  ///     Step::to(-14.0, Transition::new(70).easing(Easing::OutQuad)),
  ///     Step::to(12.0, Transition::new(90).easing(Easing::InOutQuad)),
  ///     Step::to(0.0, Transition::new(95).easing(Easing::OutQuad)),
  /// ];
  ///
  /// fn pill(cx: &Cx) {
  ///     let wrong = cx.signal(0_u32); // bumped on each wrong password
  ///     let shake = cx.sequence(0.0, move || wrong.get(), SHAKE);
  ///     // Read `shake.value().get()` in a binding, such as a translate.
  /// }
  /// ```
  #[track_caller]
  pub fn sequence<T: Animate, K: PartialEq + 'static>(
    &self,
    initial: T,
    mut trigger: impl FnMut() -> K + 'static,
    steps: impl IntoProp<Vec<Step<T>>>,
  ) -> Animation<T> {
    let player = self.player("cx.sequence", initial, steps);
    let mut last = None;
    let mut broken = false;
    let animation = player.animation();
    self.effect(move || {
      if broken {
        return;
      }
      let Some(now) = guarded(&player.label, &mut trigger) else {
        broken = true;
        player.stop();
        return;
      };
      if last.as_ref().is_some_and(|last| *last != now) {
        player.play(false);
      }
      last = Some(now);
    });
    animation
  }

  /// A value that plays `steps` once, starting as soon as it is made: an
  /// entrance that plays each time its widget is built, such as a card
  /// that waits its turn with a [`Step::hold`] and then fades in. It
  /// starts at `initial`, and once played it rests at the last step's
  /// target. `steps` is a value or a closure, read untracked once, now; an
  /// empty list plays nothing. Steps at the start that take no time land
  /// at once, as in [`Cx::sequence`]. Holds sleep off the clock as in
  /// [`Cx::sequence`], and the [`Animation`]'s `running()` is true until
  /// the last step ends. To play again on a change, use
  /// [`Cx::sequence`]. A panic in `steps` or a config's own `lerp` is
  /// logged with this call's place and stops the play.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Easing, Step, Transition};
  ///
  /// const FADE: Transition = Transition::new(200).easing(Easing::OutCubic);
  ///
  /// fn card(cx: &Cx, index: u32) {
  ///     let opacity = cx.sequence_now(0.0, [Step::hold(index * 40), Step::to(1.0, FADE)]);
  ///     // Read `opacity.value().get()` in a binding.
  /// }
  /// ```
  #[track_caller]
  pub fn sequence_now<T: Animate>(
    &self,
    initial: T,
    steps: impl IntoProp<Vec<Step<T>>>,
  ) -> Animation<T> {
    let player = self.player("cx.sequence_now", initial, steps);
    player.play(false);
    player.animation()
  }

  /// A value that plays `steps` over and over while `running` gives
  /// `true`, each round with its holds, as a `SequentialAnimation` with
  /// infinite loops: a pulse while something loads. It starts at
  /// `initial`; round 0 moves from where the value is, and each later
  /// round from the last step's target. When `running` turns `false`, the
  /// round it is in runs to its end and the value rests at the last step's
  /// target; `true` again before then keeps it going, and after it starts
  /// again at round 0. Round 0's steps at the start that take no time land
  /// in the flush where `running` turns `true`, as in [`Cx::sequence`]; a
  /// later round's land at its first tick. A round of no time plays once,
  /// in that flush. Holds sleep off the clock as in [`Cx::sequence`]. A
  /// panic in `running` is logged with
  /// this call's place and ends it after its round.
  ///
  /// ```
  /// use crabshell_ui::{Cx, Easing, Step, Transition};
  ///
  /// const SINE: Transition = Transition::new(1100).easing(Easing::InOutSine);
  ///
  /// fn tile(cx: &Cx, index: u32) {
  ///     let loading = cx.signal(true);
  ///     let steps = [Step::hold(index * 140), Step::to(0.4, SINE), Step::to(1.0, SINE)];
  ///     let opacity = cx.sequence_loop(1.0, move || loading.get(), steps);
  /// }
  /// ```
  #[track_caller]
  pub fn sequence_loop<T: Animate>(
    &self,
    initial: T,
    mut running: impl FnMut() -> bool + 'static,
    steps: impl IntoProp<Vec<Step<T>>>,
  ) -> Animation<T> {
    let player = self.player("cx.sequence_loop", initial, steps);
    let mut broken = false;
    let animation = player.animation();
    self.effect(move || {
      let on = !broken
        && guarded(&player.label, &mut running).unwrap_or_else(|| {
          broken = true;
          false
        });
      player.wanted.set(on);
      if on && untrack(|| player.running.try_get()) == Some(false) {
        player.play(true);
      }
    });
    animation
  }

  /// The state [`Cx::sequence`], [`Cx::sequence_now`] and
  /// [`Cx::sequence_loop`] share, logged as
  /// `what` at the caller's place.
  #[track_caller]
  fn player<T: Animate>(
    &self,
    what: &str,
    initial: T,
    steps: impl IntoProp<Vec<Step<T>>>,
  ) -> Rc<Player<T>> {
    Rc::new(Player {
      label: format!("{what} at {}", std::panic::Location::caller()).into(),
      steps: steps.into_prop().shared(),
      value: self.signal(initial),
      running: self.signal(false),
      generation: Rc::new(Cell::new(0)),
      wanted: Rc::new(Cell::new(false)),
      owner: self.owner(),
      activity: self.activity.clone(),
    })
  }
}

/// A sequence's value, its steps, and the play on the clock.
struct Player<T: 'static> {
  label: Rc<str>,
  steps: Rc<dyn Fn() -> Vec<Step<T>>>,
  value: Signal<T>,
  running: Signal<bool>,
  /// Bumped by each new play, so the one it replaces stops.
  generation: Rc<Cell<u64>>,
  /// What a loop's `running` last gave.
  wanted: Rc<Cell<bool>>,
  owner: Owner,
  activity: Option<Rc<Activity>>,
}

impl<T: Animate> Player<T> {
  fn animation(&self) -> Animation<T> {
    Animation {
      value: self.value.read_only(),
      running: self.running.read_only(),
    }
  }

  /// Stops the play on the clock where it is.
  fn stop(&self) {
    self.generation.set(self.generation.get() + 1);
    set_changed(self.running, false);
  }

  /// Starts a play of the steps from where the value is, looping while
  /// `wanted` if `looping`.
  fn play(&self, looping: bool) {
    self.stop();
    let label = Rc::clone(&self.label);
    // Read as each play starts, so a binding can pick the steps; the
    // clones are a config's own.
    let Some((steps, origin, last)) = guarded(&label, || {
      let steps = untrack(&*self.steps);
      let origin = untrack(|| self.value.try_get())?;
      let last = steps.iter().rev().find_map(|step| step.to.clone());
      let last = last.unwrap_or_else(|| origin.clone());
      // The steps that take no time land in this flush, so a layout or
      // draw in this turn already sees them; the clock's first tick, a
      // frame later, would show the old value once.
      let leading = steps
        .iter()
        .take_while(|step| step.transition.is_instant())
        .filter_map(|step| step.to.clone())
        .last();
      if let Some(leading) = leading {
        set_changed(self.value, leading);
      }
      Some((steps, origin, last))
    })
    .flatten() else {
      return;
    };
    let length: u64 = steps.iter().map(|step| step.times().1).sum();
    // A play of no time has ended already, with nothing for the clock.
    if length == 0 {
      return;
    }
    set_changed(self.running, true);
    let current = self.generation.get();
    let (generation, wanted) = (Rc::clone(&self.generation), Rc::clone(&self.wanted));
    let (value, running, owner) = (self.value, self.running, self.owner);
    let step_label = Rc::clone(&label);
    let mut started = start_time();
    let mut round = 0;
    start(
      label,
      self.activity.as_ref().map(Busy::new),
      Box::new(move |now| {
        if generation.get() != current {
          return false;
        }
        let begun = *started.get_or_insert(now);
        let at = now.saturating_duration_since(begun).as_micros();
        let at = u64::try_from(at).unwrap_or(u64::MAX);
        let (now_round, within) = if looping {
          (at / length, at % length)
        } else {
          (0, at)
        };
        let ended = now_round > round && !wanted.get();
        round = now_round;
        let spot = guarded(&step_label, || {
          if ended {
            return Spot::Ended(last.clone());
          }
          spot(&steps, if round == 0 { &origin } else { &last }, within)
        });
        let (next, sleep) = match spot {
          Some(Spot::Moving(next)) => (next, None),
          Some(Spot::Holding(next, until)) => (next, Some(round * length + until)),
          Some(Spot::Ended(next)) => {
            // A config's own `PartialEq` runs in the write.
            guarded(&step_label, || set_changed(value, next));
            running.try_set(false);
            return false;
          }
          // A panicking `lerp` ends the play with `running` false, so
          // whatever waits on it runs.
          None => {
            running.try_set(false);
            return false;
          }
        };
        // A value whose owner is gone stops its sequence.
        if untrack(|| value.try_with(|_| ())).is_none() {
          return false;
        }
        if guarded(&step_label, || set_changed(value, next)).is_none() {
          running.try_set(false);
          return false;
        }
        if let Some(until) = sleep {
          sleep_until(Nap {
            deadline: begun + Duration::from_micros(until),
            owner,
            play: (Rc::clone(&generation), current),
          });
        }
        true
      }),
    );
  }
}
