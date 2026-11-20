//! `Button::pressed`: held from a left press until the release, wherever
//! the pointer goes, and let go when the press is cancelled.

use crabshell_reactive::Signal;

use super::{Counter, Harness};
use crate::{
  Align, Button, Column, Drag, DragContent, Icon, Point, PointerButton, Row, ScrollView, Style,
  Window,
};

/// A 20 by 20 button at the top left of a 100 by 40 window, writing
/// `pressed`, with a click counter.
fn window(h: &mut Harness, button: Button) -> Window {
  let row = Row::new().align(Align::Start).child(button);
  h.window(h.panel().child(row), 100.0, 40.0)
}

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// How many times `signal` changed since it was made.
fn writes(h: &Harness, signal: Signal<bool>) -> Counter {
  let count = Counter::default();
  let c = count.clone();
  h.cx.effect(move || {
    signal.get();
    c.hit();
  });
  count
}

fn press(window: &Window, x: f32) {
  window.pointer_motion(Point::new(x, 10.0));
  window.pointer_button_at(PointerButton::Left, true, 0);
}

#[test]
fn it_is_held_from_the_press_to_the_release_wherever_the_pointer_goes() {
  let mut h = Harness::new();
  let pressed = h.cx.signal(false);
  let changes = writes(&h, pressed);
  let clicks = Counter::default();
  let c = clicks.clone();
  let window = window(
    &mut h,
    Button::new(square())
      .pressed(pressed)
      .on_click(move |_| c.hit()),
  );
  press(&window, 10.0);
  h.frame(&window);
  assert!(pressed.get());
  window.pointer_motion(Point::new(60.0, 10.0));
  window.pointer_motion(Point::new(15.0, 10.0));
  window.pointer_motion(Point::new(70.0, 10.0));
  h.frame(&window);
  assert!(pressed.get(), "still held off the button");
  window.pointer_button_at(PointerButton::Left, false, 100);
  h.frame(&window);
  assert!(!pressed.get());
  assert_eq!(clicks.get(), 0, "released off it");
  assert_eq!(changes.get(), 3, "the first run, the press and the release");
  // Another button's press, or this one's right button, holds nothing.
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button_at(PointerButton::Right, true, 200);
  h.frame(&window);
  assert!(!pressed.get());
  window.pointer_button_at(PointerButton::Right, false, 300);
}

#[test]
fn leaving_the_window_turning_it_off_hiding_or_removing_it_lets_go() {
  let mut h = Harness::new();
  let pressed = h.cx.signal(false);
  let enabled = h.cx.signal(true);
  let shown = h.cx.signal(true);
  let visible = h.cx.signal(true);
  let row = Row::new().align(Align::Start).children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_, _| {
      Button::new(square())
        .pressed(pressed)
        .enabled(move || enabled.get())
        .visible(move || visible.get())
        .on_click(|_| {})
    },
  );
  let window = h.window(h.panel().child(row), 100.0, 40.0);
  press(&window, 10.0);
  assert!(pressed.get_untracked());
  window.pointer_leave();
  assert!(!pressed.get_untracked(), "left the window");

  press(&window, 10.0);
  assert!(pressed.get_untracked());
  enabled.set(false);
  h.frame(&window);
  assert!(!pressed.get_untracked(), "turned off");
  window.pointer_button_at(PointerButton::Left, false, 100);
  enabled.set(true);
  h.frame(&window);

  press(&window, 10.0);
  visible.set(false);
  h.frame(&window);
  assert!(!pressed.get_untracked(), "hidden");
  window.pointer_button_at(PointerButton::Left, false, 100);
  visible.set(true);
  h.frame(&window);

  press(&window, 10.0);
  assert!(pressed.get_untracked());
  shown.set(false);
  h.frame(&window);
  assert!(!pressed.get_untracked(), "removed");
}

#[test]
fn a_scroll_view_taking_the_drag_lets_go_and_its_own_drag_keeps_it() {
  let mut h = Harness::new();
  let pressed = h.cx.signal(false);
  let dragged = h.cx.signal(false);
  let column = (0..10).fold(Column::new(), |column, n| {
    let button = match n {
      0 => Button::new(square()).pressed(pressed).on_click(|_| {}),
      _ => Button::new(square()),
    };
    column.child(button)
  });
  let view = ScrollView::new(column).style(Style::new().width(20.0).height(50.0));
  let slider = Button::new(square())
    .pressed(dragged)
    .on_drag(|_, _: Drag| {});
  let row = Row::new().align(Align::Start).child(view).child(slider);
  let window = h.window(h.panel().child(row), 100.0, 100.0);
  window.pointer_motion_at(Point::new(10.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  assert!(pressed.get_untracked());
  window.pointer_motion_at(Point::new(10.0, 2.0), 10);
  assert!(pressed.get_untracked(), "within the threshold");
  window.pointer_motion_at(Point::new(10.0, -20.0), 20);
  assert!(!pressed.get_untracked(), "the view took the drag");
  window.pointer_button_at(PointerButton::Left, false, 30);

  window.pointer_motion_at(Point::new(30.0, 10.0), 100);
  window.pointer_button_at(PointerButton::Left, true, 100);
  window.pointer_motion_at(Point::new(90.0, 60.0), 110);
  assert!(dragged.get_untracked(), "a button's own drag keeps it held");
  window.pointer_button_at(PointerButton::Left, false, 120);
  assert!(!dragged.get_untracked());
}

#[test]
fn being_held_repaints_nothing_by_itself() {
  let mut h = Harness::new();
  let pressed = h.cx.signal(false);
  let window = window(
    &mut h,
    Button::new(square()).pressed(pressed).on_click(|_| {}),
  );
  window.take_damage();
  press(&window, 10.0);
  h.frame(&window);
  assert!(pressed.get());
  assert!(!window.has_damage());
  assert!(!window.needs_layout());
  window.pointer_button_at(PointerButton::Left, false, 100);
  h.frame(&window);
  assert!(!window.has_damage());
}

#[test]
fn a_window_dropped_while_its_drag_is_out_lets_go() {
  let mut h = Harness::new();
  let pressed = h.cx.signal(false);
  let source = Button::new(square())
    .drag_content(Some(DragContent::Text("hi".into())))
    .pressed(pressed);
  let window = window(&mut h, source);
  press(&window, 10.0);
  window.pointer_motion(Point::new(10.0, -5.0));
  assert!(window.take_drag_out().is_some(), "it went out");
  window.pointer_leave();
  assert!(pressed.get_untracked(), "the platform holds the drag");
  drop(window);
  assert!(!pressed.get_untracked());
}
