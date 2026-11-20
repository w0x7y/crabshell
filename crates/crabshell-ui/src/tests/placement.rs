//! Each child's own place: grow, shrink and basis, least and most sizes,
//! `align_self` and `justify_self`, and absolute positions.

use crabshell_reactive::Signal;

use super::{Counter, Harness, rect_at};
use crate::{
  Align, Button, Column, Grid, Icon, Point, PointerButton, Rect, Row, ScrollView, Stack, Style,
};

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

/// An empty box placed by `left`, `right`, `top` and `bottom`, with a
/// height when `height` is set.
fn inset(left: f32, right: f32, top: Option<f32>, bottom: f32, height: Option<f32>) -> Stack {
  let mut style = Style::new().left(left).right(right).bottom(bottom);
  if let Some(top) = top {
    style = style.top(top);
  }
  if let Some(height) = height {
    style = style.height(height);
  }
  block(style)
}

#[test]
fn right_and_bottom_are_measured_from_the_parent_not_the_window() {
  let mut h = Harness::new();
  let cell = || Stack::new().style(Style::new().width(50.0).height(20.0));
  let row = Row::new().align(Align::Start).child(cell()).child(
    cell()
      .child(Icon::new("right").size(10).style(Style::new().right(4.0)))
      .child(Icon::new("bottom").size(10).style(Style::new().bottom(2.0))),
  );
  let window = h.window(h.panel().child(row), 200.0, 200.0);
  // From the second box's right edge at 100, and its bottom at 20.
  assert_eq!(
    rect_at(&window, &[0, 1, 0]),
    Rect::new(86.0, 0.0, 10.0, 10.0)
  );
  assert_eq!(
    rect_at(&window, &[0, 1, 1]),
    Rect::new(50.0, 8.0, 10.0, 10.0)
  );
}

#[test]
fn left_and_right_stretch_a_child_across_each_box_of_a_column() {
  let mut h = Harness::new();
  // A toast with a life bar along its bottom, as rime's are.
  let toast = || {
    Stack::new()
      .style(Style::new().height(40.0))
      .child(Icon::new("x").size(10))
      .child(inset(14.0, 14.0, None, 6.0, Some(2.0)))
  };
  let column = Column::new()
    .style(Style::new().width(100.0))
    .child(toast())
    .child(toast());
  let window = h.window(h.panel().child(column), 200.0, 200.0);
  assert_eq!(
    rect_at(&window, &[0, 0, 1]),
    Rect::new(14.0, 32.0, 72.0, 2.0)
  );
  assert_eq!(
    rect_at(&window, &[0, 1, 1]),
    Rect::new(14.0, 72.0, 72.0, 2.0)
  );
}

#[test]
fn all_four_insets_size_a_child_in_a_row() {
  let mut h = Harness::new();
  let row = Row::new()
    .style(Style::new().width(100.0).height(30.0))
    .child(Icon::new("in flow").size(10))
    .child(inset(10.0, 10.0, Some(5.0), 5.0, None));
  let column = Column::new()
    .align(Align::Start)
    .child(Icon::new("above").size(10))
    .child(row);
  let window = h.window(h.panel().child(column), 200.0, 200.0);
  assert_eq!(
    rect_at(&window, &[0, 1, 1]),
    Rect::new(10.0, 15.0, 80.0, 20.0)
  );
}

#[test]
fn a_stack_places_an_absolute_child_from_its_padding_box_not_its_cell() {
  let mut h = Harness::new();
  let stack = Stack::new()
    .style(Style::new().padding(5, 5))
    .child(Icon::new("in flow").size(10))
    .child(
      Icon::new("corner")
        .size(4)
        .style(Style::new().right(1.0).bottom(1.0)),
    );
  let row = Row::new().align(Align::Start).child(stack);
  let window = h.window(h.panel().child(row), 200.0, 200.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 20.0, 20.0));
  assert_eq!(
    rect_at(&window, &[0, 0, 1]),
    Rect::new(15.0, 15.0, 4.0, 4.0)
  );
}

#[test]
fn a_grid_and_its_cells_hold_their_absolute_children() {
  let mut h = Harness::new();
  let grid = Grid::new()
    .columns(2)
    .padding(5, 5)
    .style(Style::new().width(110.0))
    .child(Icon::new("a").size(10))
    .child(Stack::new().child(Icon::new("b").size(10)).child(inset(
      14.0,
      14.0,
      None,
      0.0,
      Some(2.0),
    )))
    .child(
      Icon::new("corner")
        .size(4)
        .style(Style::new().right(0.0).bottom(0.0)),
    );
  let window = h.window(h.panel().child(Column::new().child(grid)), 200.0, 200.0);
  // The second cell runs from 55 to 105.
  assert_eq!(
    rect_at(&window, &[0, 0, 1]),
    Rect::new(55.0, 5.0, 50.0, 10.0)
  );
  assert_eq!(
    rect_at(&window, &[0, 0, 1, 1]),
    Rect::new(69.0, 13.0, 22.0, 2.0)
  );
  // The grid's padding box, 110 by 20.
  assert_eq!(
    rect_at(&window, &[0, 0, 2]),
    Rect::new(106.0, 16.0, 4.0, 4.0)
  );
}

#[test]
fn negative_insets_widen_a_button_only_around_its_parent() {
  let mut h = Harness::new();
  let near = Counter::default();
  let far = Counter::default();
  let (near_hit, far_hit) = (near.clone(), far.clone());
  // A hit area 6 pixels past a glyph on every side, as rime's islands
  // have, and another button further along the row.
  let glyph = Stack::new().child(Icon::new("glyph").size(19)).child(
    Button::new(Stack::new())
      .style(Style::new().left(-6.0).right(-6.0).top(-6.0).bottom(-6.0))
      .on_click(move |_| near_hit.hit()),
  );
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("before").size(10))
    .child(glyph)
    .child(Stack::new().style(Style::new().width(50.0)))
    .child(Button::new(Icon::new("far").size(20)).on_click(move |_| far_hit.hit()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(
    rect_at(&window, &[0, 1, 1]),
    Rect::new(4.0, -6.0, 31.0, 31.0)
  );

  let click = |x: f32, time: u32| {
    window.pointer_motion(Point::new(x, 10.0));
    window.pointer_button_at(PointerButton::Left, true, time);
    window.pointer_button_at(PointerButton::Left, false, time + 50);
  };
  click(85.0, 1000);
  assert_eq!((near.get(), far.get()), (0, 1));
  click(32.0, 2000);
  assert_eq!((near.get(), far.get()), (1, 1));
}

#[test]
fn an_absolute_child_follows_its_parent_when_the_parent_moves() {
  let mut h = Harness::new();
  let above = h.cx.signal(10);
  let geometry = h.cx.signal(Rect::default());
  let mapped = h.cx.signal(Rect::default());
  let bar = Stack::new()
    .style(
      Style::new()
        .left(0.0)
        .right(0.0)
        .bottom(0.0)
        .height(2.0)
        .background(crate::Color::rgb(0, 0, 255)),
    )
    .geometry(geometry)
    .mapped_geometry(mapped);
  let item = Stack::new()
    .style(Style::new().width(60.0).height(20.0))
    .child(bar);
  let column = Column::new()
    .align(Align::Start)
    .child(Icon::new("above").size(move || above.get()))
    .child(item);
  let window = h.window(h.panel().child(column), 200.0, 200.0);
  let before = Rect::new(0.0, 28.0, 60.0, 2.0);
  assert_eq!(geometry.get_untracked(), before);
  assert_eq!(mapped.get_untracked(), before);
  window.take_damage();

  above.set(30);
  h.frame(&window);
  let after = Rect::new(0.0, 48.0, 60.0, 2.0);
  assert_eq!(rect_at(&window, &[0, 1, 0]), after);
  assert_eq!(geometry.get_untracked(), after);
  assert_eq!(mapped.get_untracked(), after);
  let damage = window.take_damage();
  for moved in [before, after] {
    assert!(
      damage.iter().any(|rect| rect.x <= moved.x
        && rect.y <= moved.y
        && rect.right() >= moved.right()
        && rect.bottom() >= moved.bottom()),
      "{moved:?} not in {damage:?}"
    );
  }
}
