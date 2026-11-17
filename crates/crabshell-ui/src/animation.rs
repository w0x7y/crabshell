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
use crate::prop::IntoProp;
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
  /// The time it takes, in milliseconds. 0 jumps straight to the end.
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
type Step = Box<dyn FnMut(Instant) -> bool>;

/// A step on the clock, named for log lines and counted in its item.
type Running = (Rc<str>, Option<Busy>, Step);

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
  /// When the clock last ticked.
  static LAST_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Starts `step`, named `label` for log lines and counted in `busy`'s
/// item, at the next [`advance_animations`].
pub(crate) fn start(label: Rc<str>, busy: Option<Busy>, step: Step) {
  RUNNING.with(|running| running.borrow_mut().push((label, busy, step)));
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
    match panic::catch_unwind(AssertUnwindSafe(|| step(now))) {
      Ok(true) => kept.push((label, busy, step)),
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
fn drop_isolated(label: &str, step: Step) {
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
}
