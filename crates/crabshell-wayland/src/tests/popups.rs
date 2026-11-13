//! Placing a popup: the anchor rectangle `xdg_positioner` gets.

use crabshell_ui::{Edge, Rect};

use crate::popups::{Side, anchor_rect};

#[test]
fn a_popup_goes_beside_a_popup_or_a_vertical_bar_and_below_any_other_bar() {
  assert_eq!(Side::of(false, Some(Edge::Top)), Side::Below);
  assert_eq!(Side::of(false, Some(Edge::Bottom)), Side::Below);
  assert_eq!(Side::of(false, None), Side::Below);
  assert_eq!(Side::of(false, Some(Edge::Left)), Side::Beside);
  assert_eq!(Side::of(false, Some(Edge::Right)), Side::Beside);
  assert_eq!(Side::of(true, None), Side::Beside, "a submenu");
}

#[test]
fn the_anchor_covers_the_widget_in_whole_pixels() {
  let button = Rect::new(6.5, 5.0, 20.25, 20.0);
  assert_eq!(anchor_rect(button, (1920, 30)), [6, 5, 21, 20]);
}

#[test]
fn the_anchor_stays_inside_the_parent_and_is_never_empty() {
  // Hanging off the right and bottom edges.
  let over = Rect::new(1900.0, 20.0, 50.0, 30.0);
  assert_eq!(anchor_rect(over, (1920, 30)), [1900, 20, 20, 10]);
  // Wholly outside: one pixel at the nearest corner.
  let outside = Rect::new(-40.0, 50.0, 10.0, 10.0);
  assert_eq!(anchor_rect(outside, (1920, 30)), [0, 29, 1, 1]);
  // Empty, or not numbers.
  let empty = Rect::new(10.0, 10.0, 0.0, 0.0);
  assert_eq!(anchor_rect(empty, (100, 100)), [10, 10, 1, 1]);
  let nan = Rect::new(f32::NAN, f32::INFINITY, f32::NAN, 5.0);
  assert_eq!(anchor_rect(nan, (100, 100)), [0, 99, 1, 1]);
  // A parent with no size yet still gives a valid rectangle.
  assert_eq!(anchor_rect(Rect::default(), (0, 0)), [0, 0, 1, 1]);
}
