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
  /// A cubic Bézier from (0, 0) to (1, 1) through the control points
  /// `(x1, y1)` and `(x2, y2)`, as CSS's `cubic-bezier`. The x values are
  /// kept within 0 to 1.
  Bezier(f32, f32, f32, f32),
}

impl Easing {
  /// How far along the animation is, from 0 to 1 (a Bézier may overshoot
  /// either end), when `t` of its time has passed. `t` is kept within 0
  /// to 1.
  pub fn value_at(self, t: f32) -> f32 {
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
      Easing::Bezier(x1, y1, x2, y2) => bezier(t, x1.clamp(0.0, 1.0), y1, x2.clamp(0.0, 1.0), y2),
    }
  }
}

/// The y of a cubic Bézier through (0, 0), `(x1, y1)`, `(x2, y2)` and
/// (1, 1) at the point whose x is `x`, found by bisection: x grows with
/// the curve's parameter when both control x values are within 0 to 1.
fn bezier(x: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
  let at = |a: f32, b: f32, s: f32| {
    let r = 1.0 - s;
    3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
  };
  let (mut low, mut high) = (0.0_f32, 1.0_f32);
  for _ in 0..32 {
    let middle = (low + high) / 2.0;
    if at(x1, x2, middle) < x {
      low = middle;
    } else {
      high = middle;
    }
  }
  at(y1, y2, (low + high) / 2.0)
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
  /// Everything is on one thread, so the clock is too.
  static RUNNING: RefCell<Vec<Step>> = const { RefCell::new(Vec::new()) };
}

/// Starts `step` at the next [`advance_animations`].
pub(crate) fn start(step: Step) {
  RUNNING.with(|running| running.borrow_mut().push(step));
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
  let steps = RUNNING.with(|running| std::mem::take(&mut *running.borrow_mut()));
  let mut kept = Vec::with_capacity(steps.len());
  for mut step in steps {
    match panic::catch_unwind(AssertUnwindSafe(|| step(now))) {
      Ok(true) => kept.push(step),
      Ok(false) => {}
      Err(payload) => {
        tracing::error!(
          "an animation panicked and stops: {}",
          panic_message(payload.as_ref())
        )
      }
    }
  }
  RUNNING.with(|running| {
    // Animations started while these ran go after them.
    let mut running = running.borrow_mut();
    kept.append(&mut running);
    *running = kept;
    !running.is_empty()
  })
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
  pub fn animate<T: Animate>(
    &self,
    mut target: impl FnMut() -> T + 'static,
    transition: Transition,
  ) -> ReadSignal<T> {
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
      let generation = Rc::clone(&generation);
      let mut started = None;
      start(Box::new(move |now| {
        if generation.get() != current {
          return false;
        }
        let begun = *started.get_or_insert(now);
        let (t, ended) = transition.progress(begun, now);
        let next = if ended { to.clone() } else { from.lerp(&to, t) };
        // A value whose owner is gone stops its animation.
        value.try_set(next) && !ended
      }));
    });
    value.read_only()
  }
}
