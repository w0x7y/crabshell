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

  fn hover_harness() -> Harness {
    Harness::new()
  }
}
