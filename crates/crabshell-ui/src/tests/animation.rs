//! Animations: easing, the clock, and values that follow their target.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::{Harness, capture_logs};
use crate::{Animate, Color, Easing, Spring, Transition, advance_animations, animating};

fn close(a: f32, b: f32) -> bool {
  (a - b).abs() < 0.002
}

#[test]
fn easings_start_at_0_end_at_1_and_bend_their_way() {
  for easing in [
    Easing::Linear,
    Easing::InQuad,
    Easing::OutQuad,
    Easing::InOutQuad,
    Easing::InCubic,
    Easing::OutCubic,
    Easing::InOutCubic,
    Easing::Bezier(0.25, 0.1, 0.25, 1.0),
  ] {
    assert!(close(easing.value_at(0.0), 0.0), "{easing:?} at 0");
    assert!(close(easing.value_at(1.0), 1.0), "{easing:?} at 1");
    assert!(close(easing.value_at(-3.0), 0.0) && close(easing.value_at(f32::NAN), 1.0));
  }
  assert!(close(Easing::Linear.value_at(0.3), 0.3));
  assert!(Easing::InQuad.value_at(0.5) < 0.5 && Easing::OutCubic.value_at(0.5) > 0.5);
  assert!(close(Easing::InOutCubic.value_at(0.5), 0.5));
  // CSS's `ease` is cubic-bezier(0.25, 0.1, 0.25, 1): about 0.8024 halfway.
  let ease = Easing::Bezier(0.25, 0.1, 0.25, 1.0).value_at(0.5);
  assert!((ease - 0.8024).abs() < 0.001, "{ease}");
}

/// A curve's formula, as easings.net gives it.
type Formula = fn(f32) -> f32;

#[test]
fn the_named_curves_follow_their_formulas() {
  use std::f32::consts::PI;
  // Each curve with its formula from easings.net, checked at a few points.
  let curves: [(Easing, Formula); 18] = [
    (Easing::InSine, |t| 1.0 - (t * PI / 2.0).cos()),
    (Easing::OutSine, |t| (t * PI / 2.0).sin()),
    (Easing::InOutSine, |t| -((PI * t).cos() - 1.0) / 2.0),
    (Easing::InExpo, |t| 2_f32.powf(10.0 * t - 10.0)),
    (Easing::OutExpo, |t| 1.0 - 2_f32.powf(-10.0 * t)),
    (Easing::InOutExpo, |t| {
      if t < 0.5 {
        2_f32.powf(20.0 * t - 10.0) / 2.0
      } else {
        (2.0 - 2_f32.powf(-20.0 * t + 10.0)) / 2.0
      }
    }),
    (Easing::InQuart, |t| t.powi(4)),
    (Easing::OutQuart, |t| 1.0 - (1.0 - t).powi(4)),
    (Easing::InOutQuart, |t| {
      if t < 0.5 {
        8.0 * t.powi(4)
      } else {
        1.0 - (-2.0 * t + 2.0).powi(4) / 2.0
      }
    }),
    (Easing::InQuint, |t| t.powi(5)),
    (Easing::OutQuint, |t| 1.0 - (1.0 - t).powi(5)),
    (Easing::InOutQuint, |t| {
      if t < 0.5 {
        16.0 * t.powi(5)
      } else {
        1.0 - (-2.0 * t + 2.0).powi(5) / 2.0
      }
    }),
    (Easing::InCirc, |t| 1.0 - (1.0 - t * t).sqrt()),
    (Easing::OutCirc, |t| (1.0 - (t - 1.0).powi(2)).sqrt()),
    (Easing::InOutCirc, |t| {
      if t < 0.5 {
        (1.0 - (1.0 - (2.0 * t).powi(2)).sqrt()) / 2.0
      } else {
        ((1.0 - (-2.0 * t + 2.0).powi(2)).sqrt() + 1.0) / 2.0
      }
    }),
    (Easing::InBack, |t| {
      let c1 = 1.70158_f32;
      (c1 + 1.0) * t.powi(3) - c1 * t * t
    }),
    (Easing::OutBack, |t| {
      let c1 = 1.70158_f32;
      1.0 + (c1 + 1.0) * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
    }),
    (Easing::InOutBack, |t| {
      let c2 = 1.70158_f32 * 1.525;
      if t < 0.5 {
        (2.0 * t).powi(2) * ((c2 + 1.0) * 2.0 * t - c2) / 2.0
      } else {
        ((2.0 * t - 2.0).powi(2) * ((c2 + 1.0) * (t * 2.0 - 2.0) + c2) + 2.0) / 2.0
      }
    }),
  ];
  for (easing, formula) in curves {
    assert!(close(easing.value_at(0.0), 0.0), "{easing:?} at 0");
    assert!(close(easing.value_at(1.0), 1.0), "{easing:?} at 1");
    for t in [0.25, 0.5, 0.7] {
      let (got, want) = (easing.value_at(t), formula(t));
      assert!(close(got, want), "{easing:?} at {t}: {got}, not {want}");
    }
  }
  // Back pulls behind the start, or runs past the end, by about 10%.
  let lowest = (1..100)
    .map(|i| Easing::InBack.value_at(i as f32 / 100.0))
    .fold(f32::MAX, f32::min);
  let highest = (1..100)
    .map(|i| Easing::OutBack.value_at(i as f32 / 100.0))
    .fold(f32::MIN, f32::max);
  assert!((lowest + 0.1).abs() < 0.01, "{lowest}");
  assert!((highest - 1.1).abs() < 0.01, "{highest}");
  assert!(Easing::InOutBack.value_at(0.1) < 0.0 && Easing::InOutBack.value_at(0.9) > 1.0);
}

/// Material 3's emphasized curve, as two Bézier segments.
const EMPHASIZED: &[[f32; 6]] = &[
  [0.05, 0.0, 0.133333, 0.06, 0.166666, 0.4],
  [0.208333, 0.82, 0.25, 1.0, 1.0, 1.0],
];

#[test]
fn a_spline_of_one_segment_is_that_bezier_and_of_two_follows_each() {
  const EASE: &[[f32; 6]] = &[[0.25, 0.1, 0.25, 1.0, 1.0, 1.0]];
  for t in [0.0, 0.1, 0.33, 0.5, 0.8, 1.0] {
    let spline = Easing::Spline(EASE).value_at(t);
    let bezier = Easing::Bezier(0.25, 0.1, 0.25, 1.0).value_at(t);
    assert!(close(spline, bezier), "at {t}: {spline} and {bezier}");
  }

  let emphasized = Easing::Spline(EMPHASIZED);
  assert!(close(emphasized.value_at(0.0), 0.0) && close(emphasized.value_at(1.0), 1.0));
  // The segments meet at (1/6, 0.4).
  assert!(close(emphasized.value_at(0.166666), 0.4));
  // Past the joint, it is the second segment scaled into its box: a
  // Bézier from (1/6, 0.4) to (1, 1).
  let (x0, y0) = (0.166666_f32, 0.4_f32);
  let scale = |x: f32, y: f32| ((x - x0) / (1.0 - x0), (y - y0) / (1.0 - y0));
  let (ax, ay) = scale(0.208333, 0.82);
  let (bx, by) = scale(0.25, 1.0);
  for t in [0.3, 0.5, 0.9] {
    let want = y0 + (1.0 - y0) * Easing::Bezier(ax, ay, bx, by).value_at((t - x0) / (1.0 - x0));
    assert!(close(emphasized.value_at(t), want), "at {t}");
  }
  // It rises fast: past halfway by a fifth of the time.
  assert!(emphasized.value_at(0.2) > 0.5);
}

#[test]
fn a_bad_spline_moves_linearly_and_never_gives_nan() {
  const EMPTY: &[[f32; 6]] = &[];
  const BACKWARD: &[[f32; 6]] = &[
    [0.1, 0.3, 0.4, 0.6, 0.5, 0.7],
    [0.4, 0.75, 0.3, 0.8, 0.2, 0.8],
    [0.5, 0.9, 0.9, 0.95, 1.0, 1.0],
  ];
  const SHORT: &[[f32; 6]] = &[[0.3, 0.0, 0.7, 1.0, 0.8, 0.9]];
  const NAN: &[[f32; 6]] = &[[f32::NAN, 0.0, 0.7, 1.0, 1.0, 1.0]];
  const INFINITE: &[[f32; 6]] = &[[0.2, f32::INFINITY, 0.7, 1.0, 1.0, 1.0]];
  for bad in [EMPTY, BACKWARD, SHORT, NAN, INFINITE] {
    for t in [0.0, 0.25, 0.5, 1.0, f32::NAN] {
      let value = Easing::Spline(bad).value_at(t);
      let want = if t.is_nan() { 1.0 } else { t };
      assert!(close(value, want), "{bad:?} at {t}: {value}");
    }
  }
  // Control points outside their segment are kept inside it, as a
  // Bézier's are: these become (0, 0) and (1, 1), a straight line.
  const WIDE: &[[f32; 6]] = &[[-2.0, 0.0, 3.0, 1.0, 1.0, 1.0]];
  for t in [0.1, 0.3, 0.6, 0.9] {
    let value = Easing::Spline(WIDE).value_at(t);
    assert!(close(value, t), "at {t}: {value}");
  }
}

#[test]
fn a_value_moves_to_its_new_target_over_the_transition_then_stops() {
  let h = Harness::new();
  let open = h.cx.signal(false);
  let width = h.cx.animate(
    move || if open.get() { 100.0 } else { 0.0 },
    Transition::new(200).easing(Easing::Linear),
  );
  h.runtime.flush();
  assert_eq!(width.get(), 0.0);
  assert!(!animating(), "nothing moves yet");

  open.set(true);
  h.runtime.flush();
  assert!(animating());
  let start = Instant::now();
  assert!(advance_animations(start));
  assert_eq!(width.get(), 0.0, "time 0 is the first tick");
  advance_animations(start + Duration::from_millis(50));
  assert!(close(width.get(), 25.0), "{}", width.get());
  assert!(!advance_animations(start + Duration::from_millis(200)));
  assert_eq!(width.get(), 100.0);
  assert!(!animating());
}

#[test]
fn a_new_target_while_moving_starts_from_where_it_is() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let value = h.cx.animate(
    move || target.get(),
    Transition::new(100).easing(Easing::Linear),
  );
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  assert!(close(value.get(), 50.0));
  target.set(0.0);
  h.runtime.flush();
  // The old animation stops; the new one goes from 50 and counts from the
  // last tick, so it has moved by the next.
  advance_animations(start + Duration::from_millis(60));
  assert!(close(value.get(), 45.0), "{}", value.get());
  advance_animations(start + Duration::from_millis(100));
  assert!(close(value.get(), 25.0), "{}", value.get());
  advance_animations(start + Duration::from_millis(150));
  assert_eq!(value.get(), 0.0);
  assert!(!animating());
}

#[test]
fn a_target_that_changes_every_tick_still_moves() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let value = h.cx.animate(
    move || target.get(),
    Transition::new(100).easing(Easing::Linear),
  );
  h.runtime.flush();
  let start = Instant::now();
  let mut last = 0.0;
  for tick in 0..10_u64 {
    target.set(100.0 + tick as f32);
    h.runtime.flush();
    advance_animations(start + Duration::from_millis(10 * tick));
    if tick > 0 {
      assert!(
        value.get() > last,
        "tick {tick}: {} after {last}",
        value.get()
      );
    }
    last = value.get();
  }
}

#[test]
fn a_zero_transition_jumps_and_a_gone_owner_stops_its_animation() {
  let h = Harness::new();
  let target = h.cx.signal(Color::rgb(0, 0, 0));
  let jump = h.cx.animate(move || target.get(), Transition::new(0));
  h.runtime.flush();
  target.set(Color::rgb(255, 255, 255));
  h.runtime.flush();
  // In the same flush, with no tick.
  assert_eq!(jump.get(), Color::rgb(255, 255, 255));
  assert!(!animating());

  let child = h.cx.child();
  let other = h.cx.signal(0.0_f32);
  let gone = child.animate(move || other.get(), Transition::new(100));
  h.runtime.flush();
  other.set(1.0);
  h.runtime.flush();
  child.owner().dispose();
  assert!(!advance_animations(Instant::now()), "its value is gone");
  assert!(gone.try_get().is_none());
}

#[test]
fn two_animations_started_together_after_a_rest_both_take_their_time() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let linear = Transition::new(100).easing(Easing::Linear);
  let first = h.cx.animate(move || target.get(), linear);
  h.runtime.flush();
  target.set(1.0);
  h.runtime.flush();
  let long_ago = Instant::now();
  advance_animations(long_ago);
  assert!(!advance_animations(long_ago + Duration::from_millis(100)));

  // A minute later, one change starts two animations in the same flush.
  let second = h.cx.animate(move || target.get() * 10.0, linear);
  target.set(0.0);
  h.runtime.flush();
  let now = long_ago + Duration::from_secs(60);
  advance_animations(now);
  advance_animations(now + Duration::from_millis(50));
  assert!(close(first.get(), 0.5), "{}", first.get());
  assert!(close(second.get(), 5.0), "{}", second.get());
}

#[test]
fn colors_blend_channel_by_channel() {
  let from = Color::rgba(0, 100, 200, 0);
  let to = Color::rgba(255, 100, 0, 255);
  assert_eq!(from.lerp(&to, 0.5), Color::rgba(128, 100, 100, 128));
  assert_eq!(
    from.lerp(&to, 1.5),
    Color::rgba(255, 100, 0, 255),
    "kept in range"
  );
}

#[derive(Clone, PartialEq)]
struct Fragile(f32);

impl Animate for Fragile {
  fn lerp(&self, _: &Fragile, _: f32) -> Fragile {
    panic!("lerp failure")
  }
}

#[test]
fn a_panicking_animation_is_logged_and_dropped() {
  let h = Harness::new();
  let target = h.cx.signal(Fragile(0.0));
  let _value = h.cx.animate(move || target.get(), Transition::new(100));
  h.runtime.flush();
  target.set(Fragile(1.0));
  h.runtime.flush();
  let logs = capture_logs(|| {
    advance_animations(Instant::now());
  });
  assert!(logs.contains("animation cx.animate at "), "{logs}");
  assert!(logs.contains("tests/animation.rs"), "{logs}");
  assert!(logs.contains("panicked and stops: lerp failure"), "{logs}");
  assert!(!animating());
}

const LINEAR: Transition = Transition::new(100).easing(Easing::Linear);

#[test]
fn a_delay_holds_the_value_then_it_moves() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let value = h.cx.animate(move || target.get(), LINEAR.delay(50));
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let start = Instant::now();
  advance_animations(start);
  assert!(advance_animations(start + Duration::from_millis(40)));
  assert_eq!(value.get(), 0.0, "still waiting");
  advance_animations(start + Duration::from_millis(100));
  assert!(close(value.get(), 50.0), "{}", value.get());
  assert!(!advance_animations(start + Duration::from_millis(150)));
  assert_eq!(value.get(), 100.0);
}

#[test]
fn a_new_target_during_the_delay_waits_its_own_delay() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let value = h.cx.animate(move || target.get(), LINEAR.delay(50));
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(30));
  target.set(200.0);
  h.runtime.flush();
  // The new one counts from the tick at 30 ms, so it waits until 80.
  advance_animations(start + Duration::from_millis(70));
  assert_eq!(value.get(), 0.0);
  advance_animations(start + Duration::from_millis(130));
  assert!(close(value.get(), 100.0), "{}", value.get());
  assert!(!advance_animations(start + Duration::from_millis(180)));
  assert_eq!(value.get(), 200.0);
}

#[test]
fn a_zero_transition_with_a_delay_jumps_once_the_delay_is_over() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let value = h
    .cx
    .animate(move || target.get(), Transition::new(0).delay(50));
  h.runtime.flush();
  target.set(1.0);
  h.runtime.flush();
  assert_eq!(value.get(), 0.0, "not in the same flush");
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(49));
  assert_eq!(value.get(), 0.0);
  assert!(!advance_animations(start + Duration::from_millis(50)));
  assert_eq!(value.get(), 1.0);
}

#[test]
fn running_is_true_from_a_new_target_through_its_delay_until_it_settles() {
  use std::cell::RefCell;
  use std::rc::Rc;

  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let slide = h.cx.animation(move || target.get(), LINEAR.delay(20));
  let seen = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&seen);
  let running = slide.running();
  h.cx.effect(move || log.borrow_mut().push(running.get()));
  h.runtime.flush();
  assert_eq!(*seen.borrow(), [false]);

  target.set(100.0);
  h.runtime.flush();
  assert!(running.get(), "true before the first tick");
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(10));
  assert!(
    running.get() && slide.value().get() == 0.0,
    "and in the delay"
  );
  advance_animations(start + Duration::from_millis(70));
  // A new target mid-flight keeps it running, with no false in between.
  target.set(0.0);
  h.runtime.flush();
  advance_animations(start + Duration::from_millis(120));
  h.runtime.flush();
  assert!(running.get());
  assert!(!advance_animations(start + Duration::from_millis(190)));
  h.runtime.flush();
  assert!(!running.get());
  assert_eq!(slide.value().get(), 0.0);
  assert_eq!(*seen.borrow(), [false, true, false], "one edge each way");

  // A 0 ms transition never runs.
  let jump = h.cx.animation(move || target.get(), Transition::new(0));
  h.runtime.flush();
  target.set(5.0);
  h.runtime.flush();
  assert!(!jump.running().get() && jump.value().get() == 5.0);
}

#[test]
fn a_panicking_target_is_logged_with_its_call_site_and_only_that_animation_stops() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let broken = h.cx.animation(
    move || {
      let to = target.get();
      assert!(to < 50.0, "target failure");
      to
    },
    LINEAR,
  );
  let fine = h.cx.animation(move || target.get(), LINEAR);
  h.runtime.flush();
  target.set(10.0);
  h.runtime.flush();
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  let logs = capture_logs(|| {
    target.set(100.0);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.animation at "), "{logs}");
  assert!(logs.contains("tests/animation.rs"), "{logs}");
  assert!(
    logs.contains("panicked and stops: target failure"),
    "{logs}"
  );
  assert!(!broken.running().get(), "it stops where it was");
  advance_animations(start + Duration::from_millis(100));
  assert!(close(broken.value().get(), 5.0), "{}", broken.value().get());
  assert!(fine.running().get());
  assert!(!advance_animations(start + Duration::from_millis(150)));
  assert_eq!(fine.value().get(), 100.0);
  // Later changes leave it be.
  target.set(1.0);
  h.runtime.flush();
  assert!(!broken.running().get());
}

#[test]
fn a_panicking_lerp_is_logged_and_running_ends() {
  let h = Harness::new();
  let target = h.cx.signal(Fragile(0.0));
  let moving = h.cx.animation(move || target.get(), LINEAR);
  target.set(Fragile(100.0));
  h.runtime.flush();
  assert!(moving.running().get());
  let logs = capture_logs(|| {
    advance_animations(Instant::now());
  });
  assert!(logs.contains("animation cx.animation at "), "{logs}");
  assert!(logs.contains("panicked and stops: lerp failure"), "{logs}");
  assert!(
    !moving.running().get(),
    "an effect waiting on it still runs"
  );
  assert!(!animating());
}

#[test]
fn a_delayed_animation_writes_nothing_while_it_waits() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let moving = h.cx.animate(move || target.get(), LINEAR.delay(50));
  let reads = Rc::new(Cell::new(0));
  let counted = Rc::clone(&reads);
  h.cx.effect(move || {
    moving.get();
    counted.set(counted.get() + 1);
  });
  target.set(10.0);
  h.runtime.flush();
  let start = Instant::now();
  advance_animations(start);
  h.runtime.flush();
  let before = reads.get();
  advance_animations(start + Duration::from_millis(20));
  advance_animations(start + Duration::from_millis(40));
  h.runtime.flush();
  assert_eq!(reads.get(), before, "no reader reruns during the delay");
  advance_animations(start + Duration::from_millis(100));
  h.runtime.flush();
  assert!(reads.get() > before);
}

#[test]
fn expo_curves_end_exactly_at_0_and_1() {
  for easing in [Easing::InExpo, Easing::OutExpo, Easing::InOutExpo] {
    assert_eq!(easing.value_at(0.0), 0.0, "{easing:?}");
    assert_eq!(easing.value_at(1.0), 1.0, "{easing:?}");
  }
}

/// Ticks the clock at `ms` past `start`, returning whether it still runs.
fn tick(start: Instant, ms: u64) -> bool {
  advance_animations(start + Duration::from_millis(ms))
}

#[test]
fn a_cycle_repeats_while_running_and_rests_at_0_otherwise() {
  let h = Harness::new();
  let busy = h.cx.signal(false);
  let spin = h.cx.cycle(move || busy.get(), LINEAR);
  h.runtime.flush();
  assert!(!animating(), "a cycle that isn't running wakes nothing");
  assert_eq!(spin.get(), 0.0);

  busy.set(true);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  assert_eq!(spin.get(), 0.0);
  tick(start, 25);
  assert!(close(spin.get(), 0.25), "{}", spin.get());
  tick(start, 125);
  assert!(close(spin.get(), 0.25), "again: {}", spin.get());
  tick(start, 190);
  assert!(close(spin.get(), 0.9), "{}", spin.get());
  assert!(animating());
}

#[test]
fn an_alternating_cycle_goes_there_and_back_each_leg_taking_the_time() {
  let h = Harness::new();
  let pulse = h
    .cx
    .cycle(|| true, LINEAR.easing(Easing::InQuad).alternate());
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  assert!(close(pulse.get(), 0.25), "{}", pulse.get());
  tick(start, 100);
  assert!(close(pulse.get(), 1.0), "{}", pulse.get());
  // The way back retraces the curve.
  tick(start, 150);
  assert!(close(pulse.get(), 0.25), "{}", pulse.get());
  tick(start, 190);
  assert!(close(pulse.get(), 0.01), "{}", pulse.get());
  tick(start, 250);
  assert!(close(pulse.get(), 0.25), "up again: {}", pulse.get());
}

#[test]
fn a_stopped_cycle_finishes_its_round_then_the_clock_stops_and_a_restart_begins_at_0() {
  let h = Harness::new();
  let busy = h.cx.signal(true);
  let pulse = h.cx.cycle(move || busy.get(), LINEAR.alternate());
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  busy.set(false);
  h.runtime.flush();
  // The round trip takes 200 ms, and this one runs to its end.
  assert!(tick(start, 150));
  assert!(close(pulse.get(), 0.5), "{}", pulse.get());
  assert!(tick(start, 199));
  assert!(!tick(start, 210), "the round is over");
  assert_eq!(pulse.get(), 0.0);
  assert!(!animating());

  busy.set(true);
  h.runtime.flush();
  let later = start + Duration::from_secs(5);
  tick(later, 0);
  assert_eq!(pulse.get(), 0.0, "from the start");
  tick(later, 30);
  assert!(close(pulse.get(), 0.3), "{}", pulse.get());

  // Stopped and started again before the round ends, it never stops.
  busy.set(false);
  h.runtime.flush();
  tick(later, 60);
  busy.set(true);
  h.runtime.flush();
  tick(later, 260);
  assert!(close(pulse.get(), 0.6), "{}", pulse.get());
}

#[test]
fn a_cycle_waits_out_its_delay_once() {
  let h = Harness::new();
  let spin = h.cx.cycle(|| true, LINEAR.delay(50));
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 40);
  assert_eq!(spin.get(), 0.0);
  tick(start, 75);
  assert!(close(spin.get(), 0.25), "{}", spin.get());
  tick(start, 175);
  assert!(
    close(spin.get(), 0.25),
    "no delay between rounds: {}",
    spin.get()
  );
}

#[test]
fn frames_counts_time_while_running_and_nothing_ticks_while_not() {
  let h = Harness::new();
  let playing = h.cx.signal(false);
  let elapsed = h.cx.frames(move || playing.get());
  h.runtime.flush();
  assert!(!animating());
  assert_eq!(elapsed.get(), Duration::ZERO);

  playing.set(true);
  h.runtime.flush();
  assert!(animating());
  let start = Instant::now();
  tick(start, 0);
  assert_eq!(elapsed.get(), Duration::ZERO);
  tick(start, 16);
  assert_eq!(elapsed.get(), Duration::from_millis(16));
  assert!(tick(start, 33));
  assert_eq!(elapsed.get(), Duration::from_millis(33));

  playing.set(false);
  h.runtime.flush();
  assert!(!tick(start, 50), "stopped");
  assert_eq!(
    elapsed.get(),
    Duration::from_millis(33),
    "it keeps its last time"
  );
  assert!(!animating());

  playing.set(true);
  h.runtime.flush();
  let later = start + Duration::from_secs(3);
  tick(later, 0);
  assert_eq!(elapsed.get(), Duration::ZERO, "a new run counts from 0");
  tick(later, 10);
  assert_eq!(elapsed.get(), Duration::from_millis(10));
}

#[test]
fn a_panicking_running_closure_is_logged_and_its_cycle_or_frames_stops() {
  let h = Harness::new();
  let fail = h.cx.signal(false);
  let spin = h.cx.cycle(
    move || {
      assert!(!fail.get(), "running failure");
      true
    },
    LINEAR,
  );
  let frames = h.cx.frames(move || {
    assert!(!fail.get(), "frames failure");
    true
  });
  let fine = h.cx.cycle(|| true, LINEAR);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  let logs = capture_logs(|| {
    fail.set(true);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.cycle at "), "{logs}");
  assert!(logs.contains("animation cx.frames at "), "{logs}");
  assert!(logs.contains("tests/animation.rs"), "{logs}");
  assert!(
    logs.contains("panicked and stops: running failure"),
    "{logs}"
  );
  tick(start, 60);
  assert_eq!(frames.get(), Duration::from_millis(50), "frames stopped");
  tick(start, 110);
  assert_eq!(spin.get(), 0.0, "the cycle finished its round");
  assert!(close(fine.get(), 0.1), "the other goes on: {}", fine.get());
  fail.set(false);
  h.runtime.flush();
  tick(start, 120);
  assert_eq!(spin.get(), 0.0, "and it stays stopped");
}

#[test]
fn everything_goes_with_its_owner() {
  let h = Harness::new();
  h.runtime.flush();
  let before = h.runtime.node_count();
  let child = h.cx.child();
  let target = h.cx.signal(0.0_f32);
  let slide = child.animation(move || target.get(), LINEAR);
  let spin = child.cycle(|| true, LINEAR);
  let elapsed = child.frames(|| true);
  let follow = child
    .spring(move || target.get() * 10.0, Spring::default())
    .value();
  h.runtime.flush();
  target.set(1.0);
  h.runtime.flush();
  tick(Instant::now(), 0);
  child.owner().dispose();
  assert_eq!(h.runtime.node_count(), before + 1, "only `target` is left");
  assert!(!tick(Instant::now(), 10), "nothing runs");
  assert!(slide.value().try_get().is_none() && spin.try_get().is_none());
  assert!(elapsed.try_get().is_none() && follow.try_get().is_none());
}

/// Ticks the clock once a 60 Hz frame from `start` until it stops or
/// `ms` have passed, giving the values seen and when it stopped.
fn run_for(
  start: Instant,
  ms: u64,
  value: crabshell_reactive::ReadSignal<f32>,
) -> (Vec<f32>, Option<u64>) {
  let mut seen = Vec::new();
  for frame in 0..=ms / 16 {
    let still = tick(start, frame * 16);
    seen.push(value.get());
    if !still {
      return (seen, Some(frame * 16));
    }
  }
  (seen, None)
}

#[test]
fn a_spring_reaches_its_target_settles_and_stops_the_clock() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h.cx.spring(move || target.get(), Spring::default()).value();
  h.runtime.flush();
  assert!(!animating());
  target.set(100.0);
  h.runtime.flush();
  assert!(animating());
  let (seen, stopped) = run_for(Instant::now(), 3000, x);
  let stopped = stopped.expect("it settles");
  assert!(
    stopped < 1500,
    "a default spring settles within 1.5 s: {stopped}"
  );
  assert!(seen[5] > 30.0, "and moves briskly: {seen:?}");
  assert!(seen.iter().all(|v| *v <= 100.5), "barely past: {seen:?}");
  assert_eq!(x.get(), 100.0, "it snaps there");
  assert!(!animating());
}

#[test]
fn a_spring_runs_while_it_moves_and_rests_when_it_settles() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h.cx.spring(move || target.get(), Spring::default());
  let broken = h
    .cx
    .spring(move || target.get(), Spring::new(f32::NAN, 0.9));
  h.runtime.flush();
  assert!(!x.running().get(), "at rest from the start");
  target.set(100.0);
  h.runtime.flush();
  assert!(
    x.running().get(),
    "from the new target, before the first tick"
  );
  assert!(!broken.running().get(), "a jump never runs");
  let start = Instant::now();
  tick(start, 0);
  tick(start, 100);
  assert!(x.running().get() && x.value().get() > 0.0);
  let (_, stopped) = run_for(start + Duration::from_millis(116), 3000, x.value());
  assert!(stopped.is_some());
  assert!(!x.running().get(), "false once it settles");
  assert_eq!(x.value().get(), 100.0);
}

#[test]
fn a_transition_binding_picks_each_moves_curve() {
  let h = Harness::new();
  let open = h.cx.signal(false);
  let slow = h.cx.signal(false);
  let x = h.cx.animate(
    move || if open.get() { 100.0 } else { 0.0 },
    move || {
      if open.get() || slow.get() {
        LINEAR
      } else {
        LINEAR.delay(100)
      }
    },
  );
  h.runtime.flush();
  open.set(true);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 50);
  assert!(close(x.get(), 50.0), "in over LINEAR: {}", x.get());
  assert!(!tick(start, 100));

  open.set(false);
  h.runtime.flush();
  tick(start, 200);
  tick(start, 250);
  assert_eq!(x.get(), 100.0, "out waits its delay");
  tick(start, 350);
  assert!(close(x.get(), 50.0), "{}", x.get());
  assert!(!tick(start, 400));

  // Read untracked: a change to it alone moves nothing.
  slow.set(true);
  h.runtime.flush();
  assert!(!animating());
}

#[test]
fn a_panicking_transition_binding_is_logged_and_stops_its_animation() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let slide = h.cx.animation(
    move || target.get(),
    move || -> Transition { panic!("transition failure") },
  );
  h.runtime.flush();
  let logs = capture_logs(|| {
    target.set(100.0);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.animation at "), "{logs}");
  assert!(logs.contains("tests/animation.rs"), "{logs}");
  assert!(
    logs.contains("panicked and stops: transition failure"),
    "{logs}"
  );
  assert!(!slide.running().get() && !animating());
  assert_eq!(slide.value().get(), 0.0, "it stays where it was");
}

#[test]
fn a_spring_with_little_damping_overshoots() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h
    .cx
    .spring(move || target.get(), Spring::new(300.0, 0.3))
    .value();
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let (seen, stopped) = run_for(Instant::now(), 10_000, x);
  let highest = seen.iter().copied().fold(f32::MIN, f32::max);
  // A damping ratio of 0.3 overshoots by e^(-0.3π/√(1-0.09)), about 37%.
  assert!((highest - 137.0).abs() < 3.0, "{highest}");
  assert!(stopped.is_some(), "it still settles");
  assert_eq!(x.get(), 100.0);
}

#[test]
fn a_new_target_keeps_the_springs_velocity() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h
    .cx
    .spring(move || target.get(), Spring::new(100.0, 1.0))
    .value();
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let start = Instant::now();
  run_for(start, 64, x);
  let before = x.get();
  // Sent back to 0 while moving up, it carries on up for a moment.
  target.set(0.0);
  h.runtime.flush();
  tick(start, 80);
  assert!(x.get() > before, "{} after {before}", x.get());
  let (_, stopped) = run_for(start + Duration::from_millis(96), 3000, x);
  assert!(stopped.is_some());
  assert_eq!(x.get(), 0.0);
}

#[test]
fn a_spring_survives_a_huge_gap_between_ticks() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  // Stiff enough that a plain Euler step of a millisecond flies apart.
  let x = h
    .cx
    .spring(move || target.get(), Spring::new(10_000_000.0, 0.1))
    .value();
  h.runtime.flush();
  target.set(100.0);
  h.runtime.flush();
  let start = Instant::now();
  tick(start, 0);
  tick(start, 3_600_000);
  assert!(
    x.get().is_finite() && (-100.0..=300.0).contains(&x.get()),
    "{}",
    x.get()
  );
  let (_, stopped) = run_for(start + Duration::from_secs(3601), 30_000, x);
  assert!(stopped.is_some(), "and still settles");
}

#[test]
fn a_target_or_spring_that_isnt_finite_jumps_and_stops() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h.cx.spring(move || target.get(), Spring::default()).value();
  let stiff = h
    .cx
    .spring(move || target.get(), Spring::new(f32::NAN, 0.9))
    .value();
  h.runtime.flush();
  target.set(10.0);
  h.runtime.flush();
  assert_eq!(stiff.get(), 10.0, "a broken spring jumps");
  let start = Instant::now();
  tick(start, 0);
  tick(start, 16);
  target.set(f32::INFINITY);
  h.runtime.flush();
  assert_eq!(x.get(), f32::INFINITY, "at once");
  assert!(!tick(start, 32), "and nothing moves");
  assert_eq!(x.get(), f32::INFINITY);

  target.set(5.0);
  h.runtime.flush();
  assert!(animating(), "a finite target moves again");
  let (_, stopped) = run_for(start + Duration::from_millis(48), 1000, x);
  assert!(
    stopped.is_some(),
    "landing at once from where it can't move"
  );
  assert_eq!(x.get(), 5.0);
}

#[test]
fn a_panicking_spring_target_is_logged_and_stops_where_it_is() {
  let h = Harness::new();
  let target = h.cx.signal(0.0_f32);
  let x = h
    .cx
    .spring(
      move || {
        let to = target.get();
        assert!(to < 50.0, "spring failure");
        to
      },
      Spring::default(),
    )
    .value();
  h.runtime.flush();
  target.set(10.0);
  h.runtime.flush();
  let start = Instant::now();
  run_for(start, 48, x);
  let logs = capture_logs(|| {
    target.set(100.0);
    h.runtime.flush();
  });
  assert!(logs.contains("animation cx.spring at "), "{logs}");
  assert!(logs.contains("tests/animation.rs"), "{logs}");
  assert!(
    logs.contains("panicked and stops: spring failure"),
    "{logs}"
  );
  let held = x.get();
  assert!(!tick(start, 64), "stopped");
  assert_eq!(x.get(), held);
}

mod fades {
  use std::time::{Duration, Instant};

  use super::super::Harness;
  use crate::{
    Align, Button, Color, Easing, Icon, Point, Row, Style, Text, Transition, Window,
    advance_animations, animating,
  };

  const FADE: Transition = Transition::new(100).easing(Easing::Linear);
  const NORMAL: Style = Style::new()
    .background(Color::rgb(0, 0, 0))
    .transition(FADE);
  const HOVER: Style = Style::new().background(Color::rgb(200, 100, 0));

  fn background(window: &Window) -> Option<Color> {
    super::super::node_at(window.view().root(), &[0, 0])
      .style()
      .background
  }

  fn hover_button(h: &mut Harness) -> Window {
    let row = Row::new().align(Align::Start).child(
      Button::new(Icon::new("x").size(20))
        .style(NORMAL)
        .hover_style(HOVER),
    );
    h.window(h.panel().child(row), 200.0, 30.0)
  }

  #[test]
  fn a_hover_style_fades_in_over_the_transition_repainting_each_tick() {
    let mut h = hover_harness();
    let window = hover_button(&mut h);
    window.take_damage();
    window.pointer_motion(Point::new(10.0, 10.0));
    assert!(animating());
    assert_eq!(background(&window), Some(Color::rgb(0, 0, 0)), "not yet");
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(50));
    assert_eq!(background(&window), Some(Color::rgb(100, 50, 0)));
    assert!(!window.take_damage().is_empty(), "each tick repaints");
    assert!(!advance_animations(start + Duration::from_millis(100)));
    assert_eq!(background(&window), Some(Color::rgb(200, 100, 0)));
    assert!(!window.needs_layout(), "a fade is paint only");
  }

  #[test]
  fn leaving_mid_fade_fades_back_from_where_it_was() {
    let mut h = hover_harness();
    let window = hover_button(&mut h);
    window.pointer_motion(Point::new(10.0, 10.0));
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(50));
    window.pointer_leave();
    // The way back starts at the halfway color and counts from that tick,
    // with NORMAL's transition.
    advance_animations(start + Duration::from_millis(100));
    assert_eq!(background(&window), Some(Color::rgb(50, 25, 0)));
    assert!(!advance_animations(start + Duration::from_millis(150)));
    assert_eq!(background(&window), Some(Color::rgb(0, 0, 0)));
  }

  #[test]
  fn without_a_transition_or_before_the_first_layout_a_style_changes_at_once() {
    let mut h = hover_harness();
    let plain = h.cx.signal(false);
    let row = Row::new().child(
      Button::new(Icon::new("x").size(20)).style(
        move || {
          if plain.get() { Style::new() } else { NORMAL }
        },
      ),
    );
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    assert!(!animating(), "the first style shows at once");
    plain.set(true);
    h.frame(&window);
    assert!(!animating(), "changing to a style with no transition");
    assert_eq!(background(&window), None);
  }

  #[test]
  fn a_fade_waits_out_its_delay() {
    let mut h = hover_harness();
    let row = Row::new().align(Align::Start).child(
      Button::new(Icon::new("x").size(20))
        .style(NORMAL)
        .hover_style(HOVER.transition(FADE.delay(50))),
    );
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.pointer_motion(Point::new(10.0, 10.0));
    let start = Instant::now();
    advance_animations(start);
    window.take_damage();
    advance_animations(start + Duration::from_millis(40));
    assert_eq!(background(&window), Some(Color::rgb(0, 0, 0)), "waiting");
    assert!(
      window.take_damage().is_empty(),
      "nothing repaints while it waits"
    );
    advance_animations(start + Duration::from_millis(100));
    assert_eq!(background(&window), Some(Color::rgb(100, 50, 0)));
  }

  #[test]
  fn a_zero_fade_with_a_delay_changes_once_the_delay_is_over() {
    let mut h = hover_harness();
    let row = Row::new().align(Align::Start).child(
      Button::new(Icon::new("x").size(20))
        .style(NORMAL)
        .hover_style(HOVER.transition(Transition::new(0).delay(50))),
    );
    let window = h.window(h.panel().child(row), 200.0, 30.0);
    window.pointer_motion(Point::new(10.0, 10.0));
    assert_eq!(
      background(&window),
      Some(Color::rgb(0, 0, 0)),
      "not at once"
    );
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(40));
    assert_eq!(background(&window), Some(Color::rgb(0, 0, 0)));
    assert!(!advance_animations(start + Duration::from_millis(50)));
    assert_eq!(background(&window), Some(Color::rgb(200, 100, 0)));
  }

  #[test]
  fn a_window_dropped_mid_fade_stops_it() {
    let mut h = hover_harness();
    let window = hover_button(&mut h);
    window.pointer_motion(Point::new(10.0, 10.0));
    drop(window);
    assert!(!advance_animations(Instant::now()));
  }

  #[test]
  fn a_background_fading_out_is_repainted_where_a_layout_moves_it() {
    let mut h = hover_harness();
    let shaded = h.cx.signal(true);
    let label = h.cx.signal(String::from("ab"));
    let row = Row::new()
      .align(Align::Start)
      .child(Text::new(move || label.get()))
      .child(
        Row::new()
          .style(move || {
            if shaded.get() {
              NORMAL
            } else {
              Style::new().transition(FADE)
            }
          })
          .padding(6, 6)
          .child(Icon::new("x").size(8)),
      );
    let window = h.window(h.panel().child(row), 300.0, 30.0);
    shaded.set(false);
    h.runtime.flush();
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(50));
    window.take_damage();

    // Mid-fade, the label grows and pushes the fading box along.
    label.set(String::from("a much longer label"));
    h.frame(&window);
    let moved = super::super::rect_at(&window, &[0, 1]);
    let damage = window.take_damage();
    assert!(damage.contains(&moved), "{moved:?} missing from {damage:?}");
  }

  #[test]
  fn a_fading_shadow_keeps_both_ends_inside_the_painted_area() {
    use crate::Shadow;

    let mut h = hover_harness();
    let lifted = h.cx.signal(false);
    let shadow = |y: f32| Shadow::new(0.0, y, 0.0, Color::rgb(0, 0, 0));
    let row = Row::new().padding(30, 30).align(Align::Start).child(
      Row::new()
        .style(move || {
          let y = if lifted.get() { 10.0 } else { -10.0 };
          NORMAL.shadow(shadow(y))
        })
        .padding(6, 6),
    );
    let window = h.window(h.panel().child(row), 300.0, 90.0);
    let rect = super::super::rect_at(&window, &[0, 0]);
    lifted.set(true);
    h.frame(&window);
    assert!(animating(), "a shadow fades too");
    let view = window.view();
    let bounds = super::super::node_at(view.root(), &[0, 0]).bounds();
    let both = shadow(-10.0).reach(rect).union(shadow(10.0).reach(rect));
    assert_eq!(bounds, rect.union(both));
  }

  const SLOW: Transition = Transition::new(200).easing(Easing::Linear);

  fn fading_button(h: &mut Harness, normal: Style) -> Window {
    let row = Row::new().align(Align::Start).child(
      Button::new(Icon::new("x").size(20))
        .style(normal)
        .hover_style(HOVER),
    );
    h.window(h.panel().child(row), 200.0, 30.0)
  }

  #[test]
  fn a_fade_back_to_the_base_style_takes_transition_out() {
    let mut h = hover_harness();
    let window = fading_button(&mut h, NORMAL.transition_out(SLOW));
    window.pointer_motion(Point::new(10.0, 10.0));
    let start = Instant::now();
    advance_animations(start);
    assert!(!advance_animations(start + Duration::from_millis(100)));
    assert_eq!(
      background(&window),
      Some(Color::rgb(200, 100, 0)),
      "in over FADE"
    );

    window.pointer_leave();
    advance_animations(start + Duration::from_millis(200));
    advance_animations(start + Duration::from_millis(300));
    assert_eq!(
      background(&window),
      Some(Color::rgb(100, 50, 0)),
      "out over SLOW"
    );
    assert!(!advance_animations(start + Duration::from_millis(400)));
    assert_eq!(background(&window), Some(Color::rgb(0, 0, 0)));
  }

  #[test]
  fn a_fade_out_cut_short_fades_back_in_from_where_it_was() {
    let mut h = hover_harness();
    let window = fading_button(&mut h, NORMAL.transition_out(SLOW));
    window.pointer_motion(Point::new(10.0, 10.0));
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(100));
    window.pointer_leave();
    advance_animations(start + Duration::from_millis(100));
    advance_animations(start + Duration::from_millis(200));
    assert_eq!(background(&window), Some(Color::rgb(100, 50, 0)));
    // Back in over FADE, from the halfway color.
    window.pointer_motion(Point::new(10.0, 10.0));
    advance_animations(start + Duration::from_millis(250));
    assert_eq!(background(&window), Some(Color::rgb(150, 75, 0)));
  }

  #[test]
  fn transition_out_alone_fades_only_the_way_back() {
    let mut h = hover_harness();
    let normal = Style::new()
      .background(Color::rgb(0, 0, 0))
      .transition_out(SLOW);
    let window = fading_button(&mut h, normal);
    window.pointer_motion(Point::new(10.0, 10.0));
    assert!(!animating(), "in at once");
    assert_eq!(background(&window), Some(Color::rgb(200, 100, 0)));
    window.pointer_leave();
    assert!(animating(), "out over SLOW");
    let start = Instant::now();
    advance_animations(start);
    advance_animations(start + Duration::from_millis(100));
    assert_eq!(background(&window), Some(Color::rgb(100, 50, 0)));
  }

  fn hover_harness() -> Harness {
    Harness::new()
  }
}
