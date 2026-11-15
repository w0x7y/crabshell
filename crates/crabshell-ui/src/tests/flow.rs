//! Rows and columns that wrap or run reversed, and siblings drawn and hit
//! in `z` order.

use super::{Counter, Harness, node_at, rect_at};
use crate::{
  Align, Button, Color, Column, Icon, Justify, Key, KeyEvent, Modifiers, Point, PointerButton,
  Rect, Row, Stack, Style,
};

fn tab() -> KeyEvent {
  KeyEvent {
    key: Key::Tab,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

#[test]
fn a_wrapping_row_starts_a_new_line_gap_apart() {
  let mut h = Harness::new();
  let row = Row::new()
    .wrap(true)
    .gap(2)
    .child(Icon::new("a").size(20))
    .child(Icon::new("b").size(20))
    .child(Icon::new("c").size(20));
  // The column gives the row its width and lets it be as tall as its lines.
  let column = Column::new()
    .align(Align::Start)
    .child(row.style(Style::new().width(50.0)));
  let window = h.window(h.panel().child(column), 200.0, 100.0);
  assert_eq!(
    rect_at(&window, &[0, 0, 0]),
    Rect::new(0.0, 0.0, 20.0, 20.0)
  );
  assert_eq!(
    rect_at(&window, &[0, 0, 1]),
    Rect::new(22.0, 0.0, 20.0, 20.0)
  );
  assert_eq!(
    rect_at(&window, &[0, 0, 2]),
    Rect::new(0.0, 22.0, 20.0, 20.0)
  );
  assert_eq!(rect_at(&window, &[0, 0]).height, 42.0);
}

#[test]
fn a_wrapping_columns_lines_pack_at_the_start() {
  let mut h = Harness::new();
  let column = Column::new()
    .wrap(true)
    .gap(2)
    .child(Icon::new("a").size(20))
    .child(Icon::new("b").size(20))
    .child(Icon::new("c").size(20));
  let window = h.window(h.panel().child(column), 200.0, 50.0);
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(0.0, 22.0, 20.0, 20.0));
  // Not stretched across the 200 pixels the column has.
  assert_eq!(rect_at(&window, &[0, 2]), Rect::new(22.0, 0.0, 20.0, 20.0));
}

#[test]
fn a_reversed_row_runs_right_to_left_from_the_right() {
  let mut h = Harness::new();
  let row = Row::new()
    .reverse(true)
    .gap(2)
    .child(Icon::new("a").size(10))
    .child(Icon::new("b").size(12));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]).x, 190.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 176.0);

  let mut h = Harness::new();
  let row = Row::new()
    .reverse(true)
    .justify(Justify::End)
    .child(Icon::new("a").size(10))
    .child(Icon::new("b").size(12));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 0.0);
  assert_eq!(rect_at(&window, &[0, 0]).x, 12.0);
}

#[test]
fn a_reversed_column_runs_bottom_to_top() {
  let mut h = Harness::new();
  let column = Column::new()
    .reverse(true)
    .child(Icon::new("a").size(10))
    .child(Icon::new("b").size(12));
  let window = h.window(h.panel().child(column), 200.0, 100.0);
  assert_eq!(rect_at(&window, &[0, 0]).y, 90.0);
  assert_eq!(rect_at(&window, &[0, 1]).y, 78.0);
}

#[test]
fn tab_keeps_child_order_in_a_reversed_row() {
  let mut h = Harness::new();
  let row = Row::new()
    .reverse(true)
    .align(Align::Start)
    .child(Button::new(Icon::new("first").size(20)))
    .child(Button::new(Icon::new("second").size(20)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window.key(&tab());
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );
}

#[test]
fn a_changed_reverse_lays_out_again() {
  let mut h = Harness::new();
  let reverse = h.cx.signal(false);
  let row = Row::new()
    .reverse(move || reverse.get())
    .child(Icon::new("a").size(10));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  reverse.set(true);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]).x, 190.0);
}

/// A 20-pixel square with `style`.
fn square(style: Style) -> Stack {
  Stack::new().style(style.width(20.0).height(20.0))
}

#[test]
fn children_are_drawn_by_z_then_in_child_order() {
  let mut h = Harness::new();
  let stack = Stack::new()
    .child(square(Style::new().z(2)))
    .child(square(Style::new()))
    .child(square(Style::new().z(-1)))
    .child(square(Style::new()));
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  let view = window.view();
  let order: Vec<&str> = node_at(view.root(), &[0])
    .children()
    .map(|child| child.path())
    .collect();
  assert_eq!(
    order,
    [
      "PanelWindow[DP-1]/Stack/Stack[2]",
      "PanelWindow[DP-1]/Stack/Stack[1]",
      "PanelWindow[DP-1]/Stack/Stack[3]",
      "PanelWindow[DP-1]/Stack/Stack[0]",
    ]
  );
}

#[test]
fn the_pointer_finds_the_highest_z_first() {
  let mut h = Harness::new();
  let (below, above) = (Counter::default(), Counter::default());
  let (b, a) = (below.clone(), above.clone());
  let stack = Stack::new()
    .child(
      Button::new(Icon::new("high").size(20))
        .style(Style::new().z(1))
        .on_click(move |_| a.hit()),
    )
    .child(Button::new(Icon::new("low").size(20)).on_click(move |_| b.hit()));
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  h.runtime.flush();
  assert_eq!((above.get(), below.get()), (1, 0));
}

#[test]
fn a_z_change_repaints_without_a_layout() {
  let mut h = Harness::new();
  let z = h.cx.signal(0);
  let stack = Stack::new()
    .child(
      square(Style::new().background(Color::rgb(1, 1, 1))).style(move || {
        Style::new()
          .background(Color::rgb(1, 1, 1))
          .width(20.0)
          .height(20.0)
          .z(z.get())
      }),
    )
    .child(square(Style::new().background(Color::rgb(2, 2, 2))));
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  window.take_damage();

  z.set(1);
  h.runtime.flush();
  assert!(!window.needs_layout());
  assert_eq!(window.take_damage(), [Rect::new(0.0, 0.0, 20.0, 20.0)]);
  let view = window.view();
  let last = node_at(view.root(), &[0])
    .children()
    .last()
    .map(|child| child.path().to_owned());
  assert_eq!(last.as_deref(), Some("PanelWindow[DP-1]/Stack/Stack[0]"));
}
