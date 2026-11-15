//! Animations: values that move toward a target over a [`Transition`],
//! and the clock the platform advances only while one is running, so a
//! shell where nothing moves wakes for nothing.

use std::cell::{Cell, RefCell};
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::time::Instant;

use crabshell_reactive::{ReadSignal, untrack};

use crate::cx::Cx;
use crate::geometry::Point;
use crate::input::panic_message;
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

/// How long an animation takes and how it moves. Copy it into styles and
/// constants like a color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
  /// The time it takes, in milliseconds. 0 jumps straight to the end.
  pub ms: u32,
  /// How it moves.
  pub easing: Easing,
}

impl Transition {
  /// A transition of `ms` milliseconds, easing out.
  pub const fn new(ms: u32) -> Transition {
    Transition {
      ms,
      easing: Easing::OutCubic,
    }
  }

  /// The same transition moving as `easing` says.
  pub const fn easing(self, easing: Easing) -> Transition {
    Transition { easing, ..self }
  }

  /// How far along an animation that started at `start` is at `now`,
  /// eased, and whether it has ended.
  pub(crate) fn progress(self, start: Instant, now: Instant) -> (f32, bool) {
    if self.ms == 0 {
      return (1.0, true);
    }
    let elapsed = now.saturating_duration_since(start).as_secs_f32() * 1000.0;
    let t = elapsed / self.ms as f32;
    if t >= 1.0 {
      (1.0, true)
    } else {
      (self.easing.value_at(t), false)
    }
  }
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
type Step = Box<dyn FnMut(Instant) -> bool>;

thread_local! {
  /// Everything is on one thread, so the clock is too. Each animation is
  /// named for log lines.
  static RUNNING: RefCell<Vec<(Rc<str>, Step)>> = const { RefCell::new(Vec::new()) };
  /// When the clock last ticked.
  static LAST_TICK: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Starts `step`, named `label` for log lines, at the next
/// [`advance_animations`].
pub(crate) fn start(label: Rc<str>, step: Step) {
  RUNNING.with(|running| running.borrow_mut().push((label, step)));
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
}

/// Advances every running animation to `now`, writing their values, and
/// returns whether any still runs. Each one starts at the first call after
/// it began, so `now` there is its time 0. One that panics is logged and
/// dropped.
pub fn advance_animations(now: Instant) -> bool {
  LAST_TICK.with(|last| last.set(Some(now)));
  let steps = RUNNING.with(|running| std::mem::take(&mut *running.borrow_mut()));
  let mut kept = Vec::with_capacity(steps.len());
  for (label, mut step) in steps {
    match panic::catch_unwind(AssertUnwindSafe(|| step(now))) {
      Ok(true) => kept.push((label, step)),
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
    if running.is_empty() {
      // The clock stops, so the next animation counts from its own first
      // tick, not this one.
      LAST_TICK.with(|last| last.set(None));
    }
    !running.is_empty()
  })
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

impl Cx {
  /// A value that follows `target`, moving to each new one over
  /// `transition` instead of jumping. It starts at `target`'s first value.
  /// A new target while it moves starts a new animation from where it is.
  /// The animation goes when this context's owner does.
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
    mut target: impl FnMut() -> T + 'static,
    transition: Transition,
  ) -> ReadSignal<T> {
    let label: Rc<str> = format!("cx.animate at {}", std::panic::Location::caller()).into();
    let first = untrack(&mut target);
    let value = self.signal(first.clone());
    let mut last = first;
    // Bumped by each new target, so the animation it replaces stops.
    let generation = Rc::new(Cell::new(0_u64));
    self.effect(move || {
      let to = target();
      if to == last {
        return;
      }
      last = to.clone();
      let Some(from) = untrack(|| value.try_get()) else {
        return;
      };
      let current = generation.get() + 1;
      generation.set(current);
      // No time to take: the new value shows in this flush.
      if transition.ms == 0 {
        value.try_set(to);
        return;
      }
      let generation = Rc::clone(&generation);
      let mut started = start_time();
      start(
        Rc::clone(&label),
        Box::new(move |now| {
          if generation.get() != current {
            return false;
          }
          let begun = *started.get_or_insert(now);
          let (t, ended) = transition.progress(begun, now);
          let next = if ended { to.clone() } else { from.lerp(&to, t) };
          // A value whose owner is gone stops its animation.
          value.try_set(next) && !ended
        }),
      );
    });
    value.read_only()
  }
}
