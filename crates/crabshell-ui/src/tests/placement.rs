//! Each child's own place: grow, shrink and basis, least and most sizes,
//! `align_self` and `justify_self`, and absolute positions.

use crabshell_reactive::Signal;

use super::{Harness, rect_at};
use crate::{Align, Column, Icon, Rect, Row, ScrollView, Stack, Style};

/// An empty box with `style`, which has no size of its own.
fn block(style: Style) -> Stack {
  Stack::new().style(style)
}

#[test]
fn grow_shares_the_free_space_by_weight() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(block(Style::new().grow(1.0)))
    .child(block(Style::new().grow(3.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 50.0, 30.0));
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(50.0, 0.0, 150.0, 30.0));
}

#[test]
fn basis_and_shrink_size_children_along_the_row() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(block(Style::new().basis(80.0).shrink(0.0)))
    .child(block(Style::new().basis(80.0)));
  let window = h.window(h.panel().child(row), 100.0, 30.0);
  // The second gives up the 60 pixels that don't fit; the first keeps all.
  assert_eq!(rect_at(&window, &[0, 0]).width, 80.0);
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(80.0, 0.0, 20.0, 30.0));
}

#[test]
fn least_and_most_sizes_bound_a_child() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(block(
      Style::new().grow(1.0).max_width(60.0).max_height(10.0),
    ))
    .child(Icon::new("a").size(10).style(Style::new().min_width(30.0)))
    .child(block(Style::new().min_height(40.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 60.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(60.0, 0.0, 30.0, 30.0));
  assert_eq!(rect_at(&window, &[0, 2]).height, 40.0);
}

#[test]
fn a_childs_align_self_wins_over_its_rows_align() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(
      Icon::new("a")
        .size(10)
        .style(Style::new().align_self(Align::Center)),
    )
    .child(
      Icon::new("b")
        .size(10)
        .style(Style::new().align_self(Align::End)),
    )
    .child(Icon::new("c").size(10));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 10.0, 10.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(10.0, 20.0, 10.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 2]), Rect::new(20.0, 0.0, 10.0, 30.0));
}

#[test]
fn a_column_ignores_justify_self() {
  let mut h = Harness::new();
  let column = Column::new().child(
    Icon::new("a")
      .size(10)
      .style(Style::new().justify_self(Align::End)),
  );
  let window = h.window(h.panel().child(column), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 200.0, 10.0));
}

#[test]
fn a_stack_child_places_itself_in_the_cell() {
  let mut h = Harness::new();
  let stack = Stack::new()
    .child(
      Icon::new("a").size(10).style(
        Style::new()
          .justify_self(Align::End)
          .align_self(Align::Start),
      ),
    )
    .child(
      Icon::new("b").size(10).style(
        Style::new()
          .justify_self(Align::Center)
          .align_self(Align::End),
      ),
    );
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(190.0, 0.0, 10.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(95.0, 20.0, 10.0, 10.0));
}

#[test]
fn an_absolute_child_sits_from_the_edges_of_its_parents_padding_box() {
  let mut h = Harness::new();
  let stack = Stack::new()
    .style(
      Style::new()
        .padding(5, 5)
        .border(1.0, crate::Color::rgb(0, 0, 0)),
    )
    .child(Icon::new("in flow").size(10))
    .child(
      Icon::new("corner")
        .size(10)
        .style(Style::new().right(4.0).bottom(2.0)),
    )
    .child(
      Icon::new("at")
        .size(10)
        .style(Style::new().left(3.0).top(1.0)),
    );
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  // In the cell, inside the padding.
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(5.0, 5.0, 190.0, 20.0));
  // A border takes no layout space, so the padding box is the whole box.
  assert_eq!(
    rect_at(&window, &[0, 1]),
    Rect::new(186.0, 18.0, 10.0, 10.0)
  );
  assert_eq!(rect_at(&window, &[0, 2]), Rect::new(3.0, 1.0, 10.0, 10.0));
}

#[test]
fn an_absolute_child_no_longer_sizes_its_parent() {
  let mut h = Harness::new();
  let row = Row::new().align(Align::Start).child(
    Stack::new()
      .child(Icon::new("in flow").size(10))
      .child(Icon::new("out").size(50).style(Style::new().left(0.0))),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 10.0, 10.0));
  assert_eq!(
    rect_at(&window, &[0, 0, 1]),
    Rect::new(0.0, 0.0, 50.0, 50.0)
  );
}

#[test]
fn a_number_that_isnt_finite_counts_as_unset() {
  let mut h = Harness::new();
  let row = Row::new()
    .child(block(Style::new().grow(f32::NAN).min_width(f32::INFINITY)))
    .child(
      Icon::new("a")
        .size(10)
        .style(Style::new().left(f32::NAN).max_width(f32::NAN)),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 0]).width, 0.0);
  // Still in the row's flow.
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(0.0, 0.0, 10.0, 30.0));
}

#[test]
fn a_scroll_views_child_keeps_its_height_with_a_shrink_of_its_own() {
  let mut h = Harness::new();
  let scroll =
    ScrollView::new(block(Style::new().height(200.0).shrink(1.0))).style(Style::new().height(50.0));
  let window = h.window(h.panel().child(scroll), 200.0, 50.0);
  assert_eq!(rect_at(&window, &[0, 0]).height, 200.0);
}

#[test]
fn a_changed_grow_lays_out_again_and_repaints_what_moved() {
  let mut h = Harness::new();
  let grow: Signal<f32> = h.cx.signal(1.0);
  let row = Row::new()
    .child(block(Style::new().grow(1.0).basis(0.0)))
    .child(
      Stack::new()
        .style(move || Style::new().grow(grow.get()).basis(0.0))
        .child(Icon::new("a").size(10)),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();
  assert_eq!(rect_at(&window, &[0, 1]).x, 100.0);

  grow.set(3.0);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(50.0, 0.0, 150.0, 30.0));
  let damage = window.take_damage();
  // The icon moved from 100 to 50 and is repainted at both places.
  assert!(
    damage
      .iter()
      .any(|rect| rect.x <= 50.0 && rect.right() >= 60.0)
  );
  assert!(
    damage
      .iter()
      .any(|rect| rect.x <= 100.0 && rect.right() >= 110.0)
  );
}
