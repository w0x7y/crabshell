//! Grids: equal columns, gaps, padding, spans, and freeing what they made.

use super::{Harness, paths, rect_at};
use crate::{Align, Grid, Icon, Rect, Row, Stack, Style, Text};

fn icons(grid: Grid, count: usize) -> Grid {
  (0..count).fold(grid, |grid, n| {
    grid.child(Icon::new(format!("{n}")).size(10))
  })
}

#[test]
fn children_fill_equal_columns_row_by_row() {
  let mut h = Harness::new();
  let grid = icons(Grid::new().columns(3), 5);
  let window = h.window(h.panel().child(grid), 210.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 70.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 2]), Rect::new(140.0, 0.0, 70.0, 10.0));
  // Rows are as tall as their content.
  assert_eq!(rect_at(&window, &[0, 3]), Rect::new(0.0, 10.0, 70.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 4]), Rect::new(70.0, 10.0, 70.0, 10.0));
  assert_eq!(paths(&window)[2], "PanelWindow[DP-1]/Grid/Icon[0]");
}

#[test]
fn one_column_by_default_and_zero_counts_as_one() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(icons(Grid::new(), 2)), 210.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(0.0, 10.0, 210.0, 10.0));
  let window = h.window(
    h.panel().child(icons(Grid::new().columns(0), 2)),
    210.0,
    60.0,
  );
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(0.0, 10.0, 210.0, 10.0));
}

#[test]
fn columns_stay_equal_when_a_child_is_wide() {
  let mut h = Harness::new();
  let grid = Grid::new()
    .columns(3)
    .child(Icon::new("wide").size(100))
    .child(Icon::new("b").size(10));
  let window = h.window(h.panel().child(grid), 210.0, 120.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 70.0);
}

#[test]
fn gaps_and_padding_space_the_cells() {
  let mut h = Harness::new();
  let grid = icons(Grid::new().columns(3).gap(6).row_gap(4).padding(2, 3), 4);
  let window = h.window(h.panel().child(grid), 216.0, 60.0);
  // (216 - 6 of padding - 12 of gaps) / 3 = 66.
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(3.0, 2.0, 66.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 1]).x, 75.0);
  assert_eq!(rect_at(&window, &[0, 3]), Rect::new(3.0, 16.0, 66.0, 10.0));

  let mut h = Harness::new();
  let grid = icons(Grid::new().columns(2).gap(6).column_gap(0), 3);
  let window = h.window(h.panel().child(grid), 200.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 100.0);
  assert_eq!(rect_at(&window, &[0, 2]).y, 16.0);
}

#[test]
fn a_span_takes_several_cells() {
  let mut h = Harness::new();
  let grid = Grid::new()
    .columns(3)
    .child(Icon::new("wide").size(10).style(Style::new().span(2, 1)))
    .child(Icon::new("tall").size(10).style(Style::new().span(1, 2)))
    .child(Icon::new("c").size(10))
    .child(Icon::new("d").size(10))
    .child(Icon::new("zero").size(10).style(Style::new().span(0, 0)));
  let window = h.window(h.panel().child(grid), 210.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 140.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 1]), Rect::new(140.0, 0.0, 70.0, 20.0));
  assert_eq!(rect_at(&window, &[0, 2]), Rect::new(0.0, 10.0, 70.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 3]), Rect::new(70.0, 10.0, 70.0, 10.0));
  assert_eq!(rect_at(&window, &[0, 4]), Rect::new(0.0, 20.0, 70.0, 10.0));
}

#[test]
fn a_child_places_itself_in_its_cell() {
  let mut h = Harness::new();
  let grid = Grid::new().columns(2).child(
    Icon::new("a").size(10).style(
      Style::new()
        .justify_self(Align::Center)
        .align_self(Align::End)
        .min_height(30.0),
    ),
  );
  let window = h.window(h.panel().child(grid), 200.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(45.0, 0.0, 10.0, 30.0));
}

#[test]
fn a_changed_column_count_lays_out_again() {
  let mut h = Harness::new();
  let columns = h.cx.signal(1);
  let grid = icons(Grid::new().columns(move || columns.get()), 2);
  let window = h.window(h.panel().child(grid), 200.0, 60.0);
  columns.set(2);
  h.frame(&window);
  assert_eq!(
    rect_at(&window, &[0, 1]),
    Rect::new(100.0, 0.0, 100.0, 10.0)
  );
}

#[test]
fn a_grid_in_a_row_is_as_wide_as_its_widest_cells() {
  let mut h = Harness::new();
  let grid = Grid::new()
    .columns(2)
    .child(Icon::new("a").size(30))
    .child(Icon::new("b").size(10));
  let row = Row::new().align(Align::Start).child(grid);
  let window = h.window(h.panel().child(row), 200.0, 60.0);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 60.0, 30.0));
}

#[test]
fn a_keyed_grid_frees_its_children() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::<u32>::new());
  let grid = Grid::new().columns(2).children_keyed(
    move || items.get(),
    |n| *n,
    |cx, n| {
      let extra = cx.signal(0);
      Stack::new().child(Text::new(move || format!("{} {}", n.get(), extra.get())))
    },
  );
  let window = h.window(h.panel().child(grid), 200.0, 60.0);
  let empty = h.runtime.node_count();
  items.set(vec![1, 2, 3]);
  h.frame(&window);
  assert!(h.runtime.node_count() > empty);
  items.set(Vec::new());
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), empty);
}
