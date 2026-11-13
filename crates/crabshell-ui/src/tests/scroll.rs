//! Scroll views: offsets, clamping and the wheel.

use super::{Counter, Harness, node_at, rect_at};
use crate::{
  Align, Button, Column, Icon, Point, Rect, Row, ScrollDelta, ScrollView, Style, Window,
};

/// A 50-pixel-tall scroll view of ten 20-pixel icons in a column.
fn ten_icons(h: &mut Harness, offset: crabshell_reactive::Signal<f32>) -> Window {
  let column = (0..10).fold(Column::new(), |column, _| {
    column.child(Icon::new("x").size(20))
  });
  let row = Row::new().align(Align::Start).child(
    ScrollView::new(column)
      .offset(offset)
      .style(Style::new().width(20.0).height(50.0)),
  );
  h.window(h.panel().child(row), 100.0, 100.0)
}

/// Where the column's first icon is.
fn first_icon(window: &Window) -> Rect {
  rect_at(window, &[0, 0, 0, 0])
}

#[test]
fn the_offset_moves_the_child_up_and_stops_at_either_end() {
  let mut h = Harness::new();
  let offset = h.cx.signal(30.0);
  let window = ten_icons(&mut h, offset);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 20.0, 50.0));
  assert_eq!(first_icon(&window).y, -30.0);
  // 200 pixels of icons in a 50-pixel box scroll at most 150.
  offset.set(1000.0);
  h.frame(&window);
  assert_eq!(first_icon(&window).y, -150.0);
  offset.set(-5.0);
  h.frame(&window);
  assert_eq!(first_icon(&window).y, 0.0);
  // What the view reaches is cut to its box.
  assert_eq!(
    node_at(window.view().root(), &[0, 0]).bounds(),
    Rect::new(0.0, 0.0, 20.0, 50.0)
  );
}

#[test]
fn the_wheel_and_touchpad_scroll_the_view_under_the_pointer() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = ten_icons(&mut h, offset);
  window.pointer_scroll(ScrollDelta::Wheel(120));
  assert_eq!(offset.get(), 0.0, "nothing under the pointer yet");
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  assert_eq!(offset.get(), 48.0);
  h.frame(&window);
  assert_eq!(first_icon(&window).y, -48.0);
  window.pointer_scroll(ScrollDelta::Pixels(-8.0));
  assert_eq!(offset.get(), 40.0);
  // Scrolling stops at the end instead of running past it.
  h.frame(&window);
  window.pointer_scroll(ScrollDelta::Wheel(120 * 10));
  assert_eq!(offset.get(), 150.0);
  h.frame(&window);
  window.pointer_scroll(ScrollDelta::Pixels(f32::NAN));
  assert_eq!(offset.get(), 150.0);
}

#[test]
fn a_button_with_on_scroll_takes_the_wheel_and_one_without_lets_it_through() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let steps = Counter::default();
  let s = steps.clone();
  let column = Column::new()
    .child(Button::new(Icon::new("x").size(20)).on_scroll(move |_, _| s.hit()))
    .child(Button::new(Icon::new("x").size(20)))
    .child(Icon::new("x").size(100));
  let scroll = ScrollView::new(column)
    .offset(offset)
    .style(Style::new().height(50.0));
  let window = h.window(h.panel().child(scroll), 100.0, 100.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  assert_eq!((steps.get(), offset.get()), (1, 0.0));
  window.pointer_motion(Point::new(10.0, 30.0));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  assert_eq!((steps.get(), offset.get()), (1, 48.0));
}

#[test]
fn scrolls_before_the_next_layout_add_up() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = ten_icons(&mut h, offset);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  assert_eq!(offset.get(), 96.0);
  // A config's own write counts too.
  offset.set(10.0);
  window.pointer_scroll(ScrollDelta::Pixels(5.0));
  assert_eq!(offset.get(), 15.0);
}

#[test]
fn scrolling_with_an_offset_whose_owner_is_gone_does_nothing() {
  let mut h = Harness::new();
  let gone = h.cx.child();
  let offset = gone.signal(0.0);
  let window = ten_icons(&mut h, offset);
  gone.owner().dispose();
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(ScrollDelta::Wheel(120));
  h.frame(&window);
}
