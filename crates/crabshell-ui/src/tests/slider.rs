//! The slider: dragging, the wheel, the keys, steps, the range and how
//! the handle and fill follow the value.

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, pixels, rect_at, wheel};
use crate::{Align, Key, KeyEvent, Modifiers, Point, PointerButton, Rect, Row, Slider, Window};

/// The horizontal body's track and handle layer, under the slider's
/// button and its cell.
const BODY: [usize; 5] = [0, 0, 0, 0, 0];

fn at(path: &[usize]) -> Vec<usize> {
  BODY.iter().chain(path).copied().collect()
}

/// A 200 by 40 window with `slider` at its top left.
fn window(h: &mut Harness, slider: Slider) -> Window {
  let row = Row::new().align(Align::Start).child(slider);
  h.window(h.panel().child(row), 200.0, 40.0)
}

/// A default-sized slider over 0 to 104: the handle's 16 pixels leave
/// 104 to travel, so the value is the handle's left edge.
fn pixels_slider(value: Signal<f32>) -> Slider {
  Slider::new(value).range(0.0, 104.0)
}

fn handle(window: &Window) -> Rect {
  rect_at(window, &at(&[1, 1]))
}

fn fill(window: &Window) -> Rect {
  rect_at(window, &at(&[0, 0]))
}

fn press(window: &Window, x: f32, y: f32) {
  window.pointer_motion(Point::new(x, y));
  window.pointer_button_at(PointerButton::Left, true, 0);
}

fn release(window: &Window) {
  window.pointer_button_at(PointerButton::Left, false, 100);
}

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

#[test]
fn the_handle_and_fill_follow_the_value_and_a_bad_value_shows_at_an_end() {
  let mut h = Harness::new();
  let value = h.cx.signal(52.0);
  let window = window(&mut h, pixels_slider(value));
  // The track is 120 by 4, centered in the handle's 16.
  assert_eq!(rect_at(&window, &at(&[0])), Rect::new(0.0, 6.0, 120.0, 4.0));
  assert_eq!(handle(&window), Rect::new(52.0, 0.0, 16.0, 16.0));
  // The fill reaches the handle's middle.
  assert_eq!(fill(&window).width, 60.0);
  for (set, x) in [(500.0, 104.0), (-3.0, 0.0), (f32::NAN, 0.0)] {
    value.set(set);
    h.frame(&window);
    assert_eq!(handle(&window).x, x, "{set}");
  }
  assert!(value.get().is_nan(), "showing it writes nothing");
}

#[test]
fn a_press_moves_the_value_there_and_a_drag_follows_within_the_range() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let dragging = h.cx.signal(false);
  let window = window(&mut h, pixels_slider(value).dragging(dragging));
  // The handle's middle goes under the pointer.
  press(&window, 60.0, 8.0);
  assert_eq!(value.get(), 52.0);
  assert!(dragging.get());
  window.pointer_motion(Point::new(30.0, 30.0));
  assert_eq!(value.get(), 22.0);
  window.pointer_motion(Point::new(300.0, 8.0));
  assert_eq!(value.get(), 104.0);
  window.pointer_motion(Point::new(-50.0, 8.0));
  assert_eq!(value.get(), 0.0);
  release(&window);
  assert!(!dragging.get());
  h.frame(&window);
  assert_eq!(handle(&window).x, 0.0);
}

#[test]
fn a_step_snaps_and_only_a_change_is_written() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let writes = Counter::default();
  let w = writes.clone();
  h.cx.effect(move || {
    value.get();
    w.hit();
  });
  let window = window(&mut h, Slider::new(value).range(0.0, 100.0).step(10.0));
  assert_eq!(writes.get(), 1);
  // 47% of the way is closer to 50 than 40.
  press(&window, 8.0 + 0.47 * 104.0, 8.0);
  h.frame(&window);
  assert_eq!(value.get(), 50.0);
  assert_eq!(writes.get(), 2);
  window.pointer_motion(Point::new(8.0 + 0.52 * 104.0, 8.0));
  h.frame(&window);
  assert_eq!(writes.get(), 2, "still 50");
  release(&window);
}

#[test]
fn the_wheel_and_touchpad_move_a_step_or_a_twentieth() {
  let mut h = Harness::new();
  let stepped = h.cx.signal(50.0);
  let free = h.cx.signal(0.5);
  let row = Row::new()
    .align(Align::Start)
    .child(Slider::new(stepped).range(0.0, 100.0).step(5.0))
    .child(Slider::new(free));
  let window = h.window(h.panel().child(row), 300.0, 40.0);
  window.pointer_motion(Point::new(60.0, 8.0));
  // A notch up moves it up a step, and down moves it back.
  window.pointer_scroll(wheel(-120));
  assert_eq!(stepped.get(), 55.0);
  window.pointer_scroll(wheel(120));
  assert_eq!(stepped.get(), 50.0);
  // A touchpad's pixels add up to steps, 15 a step.
  window.pointer_scroll(pixels(-10.0));
  assert_eq!(stepped.get(), 50.0);
  window.pointer_scroll(pixels(-10.0));
  assert_eq!(stepped.get(), 55.0);
  window.pointer_motion(Point::new(180.0, 8.0));
  window.pointer_scroll(wheel(-120));
  assert!((free.get() - 0.55).abs() < 1e-6, "{}", free.get());
  window.pointer_scroll(pixels(-7.5));
  assert!((free.get() - 0.575).abs() < 1e-6, "{}", free.get());
}

#[test]
fn the_arrow_keys_move_it_while_it_has_the_focus() {
  let mut h = Harness::new();
  let value = h.cx.signal(1.0);
  let window = window(&mut h, Slider::new(value).range(0.0, 3.0).step(1.0));
  window.keyboard_enter();
  window.key(&key(Key::Tab));
  window.key(&key(Key::Right));
  assert_eq!(value.get(), 2.0);
  window.key(&key(Key::Up));
  window.key(&key(Key::Up));
  assert_eq!(value.get(), 3.0, "kept to the range");
  window.key(&key(Key::Left));
  window.key(&key(Key::Down));
  assert_eq!(value.get(), 1.0);
}

#[test]
fn a_vertical_slider_runs_from_the_bottom_up() {
  let mut h = Harness::new();
  let value = h.cx.signal(26.0);
  let vertical = h.cx.signal(false);
  let slider = pixels_slider(value).vertical(move || vertical.get());
  let row = Row::new().align(Align::Start).child(slider);
  let window = h.window(h.panel().child(row), 200.0, 200.0);
  vertical.set(true);
  h.frame(&window);
  // The vertical body is the second; its handle layer, its handle.
  let path = [0, 0, 0, 0, 0, 1, 1];
  assert_eq!(rect_at(&window, &[0, 0, 0, 0, 0, 0]).height, 120.0);
  assert_eq!(rect_at(&window, &path), Rect::new(0.0, 78.0, 16.0, 16.0));
  press(&window, 8.0, 112.0);
  assert_eq!(value.get(), 0.0);
  window.pointer_motion(Point::new(8.0, 8.0));
  assert_eq!(value.get(), 104.0);
  release(&window);
}

#[test]
fn a_disabled_slider_moves_for_nothing() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let window = window(&mut h, pixels_slider(value).enabled(false));
  press(&window, 60.0, 8.0);
  window.pointer_scroll(wheel(-120));
  release(&window);
  assert_eq!(value.get(), 0.0);
}

#[test]
fn a_panicking_range_is_logged_with_its_path_and_the_window_runs_on() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let broken = Slider::new(value).range(|| -> f32 { panic!("range failure") }, 1.0);
  let other = h.cx.signal(0.0);
  let logs = capture_logs(|| {
    let row = Row::new()
      .align(Align::Start)
      .child(broken)
      .child(pixels_slider(other));
    let window = h.window(h.panel().child(row), 300.0, 40.0);
    press(&window, 60.0, 8.0);
    release(&window);
    press(&window, 180.0, 8.0);
    release(&window);
    h.frame(&window);
  });
  assert!(logs.contains("PanelWindow[DP-1]/Row/Slider[0]"), "{logs}");
  assert!(logs.contains("range failure"), "{logs}");
  assert_eq!(other.get(), 52.0, "the other slider still works");
}

#[test]
fn dropping_a_slider_frees_everything_it_made() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let value = h.cx.signal(0.0);
  let vertical = h.cx.signal(false);
  let window = window(
    &mut h,
    pixels_slider(value)
      .step(4.0)
      .vertical(move || vertical.get()),
  );
  press(&window, 60.0, 8.0);
  window.pointer_scroll(wheel(-120));
  vertical.set(true);
  h.frame(&window);
  drop(window);
  // The two signals the test made outlive the window.
  assert_eq!(h.runtime.node_count(), before + 2);
}

#[test]
fn a_slider_removed_mid_drag_writes_dragging_false() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let dragging = h.cx.signal(false);
  let shown = h.cx.signal(true);
  let writes = Counter::default();
  let w = writes.clone();
  h.cx.effect(move || {
    dragging.get();
    w.hit();
  });
  let row = Row::new().align(Align::Start).children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| pixels_slider(value).dragging(dragging),
  );
  let window = h.window(h.panel().child(row), 200.0, 40.0);
  press(&window, 60.0, 8.0);
  h.frame(&window);
  assert!(dragging.get());
  shown.set(false);
  h.frame(&window);
  assert!(!dragging.get(), "it went while held");
  assert_eq!(writes.get(), 3);
  // A slider that goes while not held writes nothing.
  shown.set(true);
  h.frame(&window);
  shown.set(false);
  h.frame(&window);
  assert_eq!(writes.get(), 3);
}

#[test]
fn a_step_can_be_bound() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let step = h.cx.signal(10.0);
  let window = window(
    &mut h,
    Slider::new(value)
      .range(0.0, 100.0)
      .step(move || step.get()),
  );
  press(&window, 8.0 + 0.47 * 104.0, 8.0);
  assert_eq!(value.get(), 50.0);
  step.set(25.0);
  h.frame(&window);
  window.pointer_motion(Point::new(8.0 + 0.62 * 104.0, 8.0));
  assert_eq!(value.get(), 50.0, "62 is nearer 50 than 75");
  window.pointer_motion(Point::new(8.0 + 0.64 * 104.0, 8.0));
  assert_eq!(value.get(), 75.0);
  release(&window);
}

#[test]
fn a_limit_caps_a_drag_the_wheel_and_the_keys_and_the_track_keeps_the_range() {
  let mut h = Harness::new();
  let value = h.cx.signal(0.0);
  let limit = h.cx.signal(Some(50.0));
  let window = window(
    &mut h,
    pixels_slider(value).step(1.0).limit(move || limit.get()),
  );
  press(&window, 60.0, 8.0);
  assert_eq!(value.get(), 50.0, "52 is past the cap");
  window.pointer_motion(Point::new(300.0, 8.0));
  assert_eq!(value.get(), 50.0);
  h.frame(&window);
  assert_eq!(handle(&window).x, 50.0, "drawn along the whole range");
  window.pointer_scroll(wheel(-120));
  assert_eq!(value.get(), 50.0);
  window.pointer_motion(Point::new(30.0, 8.0));
  assert_eq!(value.get(), 22.0);
  release(&window);
  window.keyboard_enter();
  window.key(&key(Key::Tab));
  // A cap lowered under the value moves nothing until the value moves.
  limit.set(Some(10.0));
  h.frame(&window);
  assert_eq!(value.get(), 22.0);
  window.key(&key(Key::Right));
  assert_eq!(value.get(), 10.0, "a move lands at or under the cap");
  // None, NaN or a cap past the end is the end.
  for cap in [None, Some(f32::NAN), Some(500.0)] {
    limit.set(cap);
    h.frame(&window);
    press(&window, 119.0, 8.0);
    assert_eq!(value.get(), 104.0, "{cap:?}");
    release(&window);
    value.set(0.0);
  }
}

#[test]
fn a_value_set_from_outside_past_the_limit_shows_where_it_is() {
  let mut h = Harness::new();
  let value = h.cx.signal(80.0);
  let window = window(&mut h, pixels_slider(value).limit(Some(50.0)));
  assert_eq!(handle(&window).x, 80.0);
  assert_eq!(value.get(), 80.0, "showing it writes nothing");
}

#[test]
fn a_wheel_step_moves_the_wheel_and_keys_while_a_drag_keeps_the_step() {
  let mut h = Harness::new();
  let value = h.cx.signal(50.0);
  let window = window(
    &mut h,
    Slider::new(value)
      .range(0.0, 100.0)
      .step(1.0)
      .wheel_step(5.0),
  );
  window.pointer_motion(Point::new(60.0, 8.0));
  window.pointer_scroll(wheel(-120));
  assert_eq!(value.get(), 55.0);
  // A touchpad adds up to whole wheel steps.
  window.pointer_scroll(pixels(-10.0));
  assert_eq!(value.get(), 55.0);
  window.pointer_scroll(pixels(-10.0));
  assert_eq!(value.get(), 60.0);
  window.keyboard_enter();
  window.key(&key(Key::Tab));
  window.key(&key(Key::Left));
  assert_eq!(value.get(), 55.0);
  press(&window, 8.0 + 0.473 * 104.0, 8.0);
  assert_eq!(value.get(), 47.0, "a drag snaps to the step");
  release(&window);
  // A wheel step that isn't a number above 0 falls back to the step.
  let bad = h.cx.signal(5.0);
  let window = self::window(
    &mut h,
    Slider::new(bad).range(0.0, 10.0).step(1.0).wheel_step(-2.0),
  );
  window.pointer_motion(Point::new(60.0, 8.0));
  window.pointer_scroll(wheel(-120));
  assert_eq!(bad.get(), 6.0);
}
