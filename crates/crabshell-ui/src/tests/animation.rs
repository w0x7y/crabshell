//! Animations: easing, the clock, and values that follow their target.

use std::time::{Duration, Instant};

use super::{Harness, capture_logs};
use crate::{Animate, Color, Easing, Transition, advance_animations, animating};

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
  // The old animation stops; the new one starts at 50 on its first tick.
  let later = start + Duration::from_millis(60);
  advance_animations(later);
  assert!(close(value.get(), 50.0), "{}", value.get());
  advance_animations(later + Duration::from_millis(50));
  assert!(close(value.get(), 25.0), "{}", value.get());
  advance_animations(later + Duration::from_millis(100));
  assert_eq!(value.get(), 0.0);
  assert!(!animating());
}

#[test]
fn a_zero_transition_jumps_and_a_gone_owner_stops_its_animation() {
  let h = Harness::new();
  let target = h.cx.signal(Color::rgb(0, 0, 0));
  let jump = h.cx.animate(move || target.get(), Transition::new(0));
  h.runtime.flush();
  target.set(Color::rgb(255, 255, 255));
  h.runtime.flush();
  assert!(!advance_animations(Instant::now()));
  assert_eq!(jump.get(), Color::rgb(255, 255, 255));

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
  assert!(
    logs.contains("an animation panicked and stops: lerp failure"),
    "{logs}"
  );
  assert!(!animating());
}
