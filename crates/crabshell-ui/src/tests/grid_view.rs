//! Grid views: cells of one size in as many columns as fit, built only
//! for the rows in view, with `current` and a highlight.

use std::time::{Duration, Instant};

use crabshell_reactive::Signal;

use super::{Harness, node_at};
use crate::{
  Content, GridView, NodeView, Point, Rect, Size, Style, Text, Window, advance_animations,
  animating, next_timer,
};

/// The text under `node`, if any.
fn label(node: NodeView<'_>) -> Option<String> {
  if let Content::Text(text) = node.content() {
    return Some(text.content().to_owned());
  }
  node.children().find_map(label)
}

/// The cells built, by their number, with where they are.
fn cells(window: &Window) -> Vec<(u32, Rect)> {
  let view = window.view();
  // The grid, its scroll view, its column and, after the spacer, the
  // cells' row.
  let column = node_at(view.root(), &[0, 0, 0]);
  let row = column.children().last().expect("the cells' row");
  row
    .children()
    .map(|cell| {
      let n = label(cell).and_then(|n| n.parse().ok()).expect("a number");
      (n, cell.rect())
    })
    .collect()
}

fn cell(window: &Window, n: u32) -> Option<Rect> {
  cells(window)
    .into_iter()
    .find(|&(at, _)| at == n)
    .map(|(_, rect)| rect)
}

/// A grid of `count` numbered 20 by 20 cells, 100 tall, in a window
/// `width` wide.
fn grid(
  h: &mut Harness,
  count: u32,
  width: f32,
  offset: Signal<f32>,
  tune: impl FnOnce(GridView) -> GridView,
) -> Window {
  let grid = GridView::new(
    move || (0..count).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .cell_size(20, 20)
  .height(100)
  .offset(offset);
  h.window(h.panel().child(tune(grid)), width, 200.0)
}

#[test]
fn a_grid_of_ten_thousand_builds_a_screenful_and_frees_it_all() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let offset = h.cx.signal(0.0);
  let window = grid(&mut h, 10_000, 100.0, offset, |grid| grid);
  // Five columns of 20 in 100; five rows show and one more is built.
  let built = cells(&window);
  assert_eq!(built.len(), 30);
  assert_eq!(cell(&window, 7), Some(Rect::new(40.0, 20.0, 20.0, 20.0)));
  offset.set(20_000.0);
  h.frame(&window);
  h.frame(&window);
  // Row 1000 at the top, with a row each side.
  let numbers: Vec<u32> = cells(&window).iter().map(|&(n, _)| n).collect();
  assert_eq!(numbers.first(), Some(&4995));
  assert_eq!(numbers.len(), 35);
  assert_eq!(cell(&window, 5000).map(|rect| rect.y), Some(0.0));
  assert!(!window.needs_layout(), "a grid at rest lays out nothing");
  drop(window);
  assert_eq!(h.runtime.node_count(), before + 1, "only the test's offset");
}

#[test]
fn a_grid_resized_keeps_its_first_shown_cell_in_view() {
  let mut h = Harness::new();
  let offset = h.cx.signal(205.0);
  let window = grid(&mut h, 1000, 100.0, offset, |grid| grid);
  // An offset set before the first layout is kept: row 10, cell 50.
  assert_eq!(offset.get(), 205.0);
  assert_eq!(cell(&window, 50).map(|rect| rect.y), Some(-5.0));
  window.set_size(Size::new(200.0, 200.0));
  h.frame(&window);
  // Ten columns: cell 50 is on row 5, still 5 pixels up.
  assert_eq!(offset.get(), 105.0);
  assert_eq!(cell(&window, 50).map(|rect| rect.y), Some(-5.0));
  // Too narrow for a whole cell, it keeps one column.
  window.set_size(Size::new(10.0, 200.0));
  h.frame(&window);
  assert_eq!(
    cell(&window, 50).map(|rect| (rect.x, rect.y)),
    Some((0.0, -5.0))
  );
}

#[test]
fn current_scrolls_as_little_as_needed_to_show_its_cell() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(None);
  let window = grid(&mut h, 1000, 100.0, offset, |grid| {
    grid.current(move || current.get())
  });
  current.set(Some(57));
  h.frame(&window);
  // Row 11 ends at 240.
  assert_eq!(offset.get(), 140.0);
  assert_eq!(cell(&window, 57).map(|rect| rect.bottom()), Some(100.0));
  current.set(Some(3));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  current.set(Some(5000));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0, "past the end scrolls nothing");
}

const HIGHLIGHT: Style = Style::new()
  .background(crate::Color::rgb(0x45, 0x47, 0x5a))
  .transition(crate::Transition::new(100).easing(crate::Easing::Linear));

/// The highlight, if it shows: where it is moved to and its size.
fn highlight(window: &Window) -> Option<(Point, Size)> {
  let view = window.view();
  let first = node_at(view.root(), &[0, 0, 0]).children().next()?;
  let shape = first.children().next()?.rect();
  Some((
    first.style().translate.unwrap_or_default(),
    Size::new(shape.width, shape.height),
  ))
}

#[test]
fn the_highlight_moves_between_cells_and_jumps_when_the_columns_change() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(7));
  let window = grid(&mut h, 1000, 100.0, offset, |grid| {
    grid.current(move || current.get()).highlight(HIGHLIGHT)
  });
  assert_eq!(
    highlight(&window),
    Some((Point::new(40.0, 20.0), Size::new(20.0, 20.0)))
  );
  current.set(Some(9));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + Duration::from_millis(50));
  h.frame(&window);
  assert_eq!(
    highlight(&window).map(|(at, _)| at),
    Some(Point::new(60.0, 20.0))
  );
  advance_animations(start + Duration::from_millis(100));
  h.frame(&window);
  assert_eq!(
    highlight(&window).map(|(at, _)| at),
    Some(Point::new(80.0, 20.0))
  );
  assert!(!animating() && next_timer().is_none() && !window.needs_layout());
  // Ten columns put cell 9 on the first row, at once.
  window.set_size(Size::new(200.0, 200.0));
  h.frame(&window);
  assert_eq!(
    highlight(&window).map(|(at, _)| at),
    Some(Point::new(180.0, 0.0))
  );
  assert!(!animating());
}

#[test]
fn a_cell_size_that_panics_is_logged_with_the_grids_path_and_the_rest_runs() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let wide = h.cx.signal(20_u32);
  let logs = super::capture_logs(|| {
    let grid = GridView::new(
      || (0..50).collect::<Vec<u32>>(),
      |n| *n,
      |_, n| Text::new(move || n.get().to_string()),
    )
    .cell_size(
      move || {
        assert!(wide.get() != 30, "cell failure");
        wide.get()
      },
      20,
    )
    .height(100)
    .offset(offset);
    let window = h.window(h.panel().child(grid), 100.0, 200.0);
    wide.set(30);
    h.frame(&window);
    offset.set(40.0);
    h.frame(&window);
    assert_eq!(cell(&window, 10).map(|rect| rect.y), Some(0.0));
  });
  assert!(logs.contains("PanelWindow[DP-1]/GridView"), "{logs}");
  assert!(logs.contains("cell failure"), "{logs}");
}

#[test]
fn a_zero_cell_size_counts_as_one_pixel() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = grid(&mut h, 30, 10.0, offset, |grid| grid.cell_size(0, 0));
  // Ten columns of 1 in 10; three rows hold all 30.
  assert_eq!(cells(&window).len(), 30);
  assert_eq!(cell(&window, 13), Some(Rect::new(3.0, 1.0, 1.0, 1.0)));
}

#[test]
fn a_grid_widened_while_scrolled_past_its_new_end_keeps_its_top_left_cell() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = grid(&mut h, 1000, 100.0, offset, |grid| grid);
  // Cell 950 at the top of five columns.
  offset.set(3800.0);
  h.frame(&window);
  assert_eq!(cell(&window, 950).map(|rect| rect.y), Some(0.0));
  // Ten columns are 2000 tall, so 3800 is past the end: row 95 shows it.
  window.set_size(Size::new(200.0, 200.0));
  h.frame(&window);
  assert_eq!(offset.get(), 1900.0);
  assert_eq!(cell(&window, 950).map(|rect| rect.y), Some(0.0));
}

#[test]
fn a_width_that_keeps_the_columns_leaves_the_view_where_it_is() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(3));
  let window = grid(&mut h, 1000, 100.0, offset, |grid| {
    grid.current(move || current.get())
  });
  offset.set(1000.0);
  h.frame(&window);
  // Still five columns: the view stays away from the current cell.
  window.set_size(Size::new(110.0, 200.0));
  h.frame(&window);
  assert_eq!(offset.get(), 1000.0);
  // Ten columns show it again.
  window.set_size(Size::new(200.0, 200.0));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
}

#[test]
fn a_current_cell_set_before_the_items_arrive_is_shown_once_they_do() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let items = h.cx.signal(Vec::<u32>::new());
  let current = h.cx.signal(Some(57));
  let grid = GridView::new(
    move || items.get(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .cell_size(20, 20)
  .height(100)
  .offset(offset)
  .current(move || current.get());
  let window = h.window(h.panel().child(grid), 100.0, 200.0);
  items.set((0..1000).collect());
  h.frame(&window);
  // Row 11 ends at 240.
  assert_eq!(offset.get(), 140.0);
  // Items that keep it in range don't pull the view back to it.
  offset.set(0.0);
  h.frame(&window);
  items.set((0..999).collect());
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
}

#[test]
fn a_current_cell_that_changes_during_a_fling_leaves_the_view_moving() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(0));
  let window = grid(&mut h, 1000, 100.0, offset, |grid| {
    grid.current(move || current.get()).highlight(HIGHLIGHT)
  });
  let top = move || Some((offset.get() / 20.0).floor() as usize * 5);
  window.pointer_motion(Point::new(10.0, 1.0));
  for n in 0..5 {
    window.pointer_scroll_at(super::pixels(10.0), n * 10);
  }
  window.pointer_scroll_at(super::lift(), 45);
  current.set(top());
  h.frame(&window);
  let start = Instant::now();
  let mut ticks = 0;
  while animating() {
    assert!(ticks < 1000);
    advance_animations(start + Duration::from_millis(16 * ticks));
    h.frame(&window);
    current.set(top());
    h.frame(&window);
    ticks += 1;
  }
  let gone = offset.get() - 50.0;
  assert!(
    (gone - 1000.0 * 1000.0 / 3000.0).abs() < 2.0,
    "{gone} in {ticks} ticks"
  );
  let index = current.get().expect("a current cell");
  let start = cell(&window, index as u32).expect("it shows").y + offset.get();
  assert_eq!(highlight(&window).map(|(at, _)| at.y), Some(start));
}

/// A horizontal grid of `count` numbered 20 by 20 cells, 100 wide, in a
/// window 200 wide and `height` tall.
fn sideways(
  h: &mut Harness,
  count: u32,
  height: f32,
  offset: Signal<f32>,
  tune: impl FnOnce(GridView) -> GridView,
) -> Window {
  let grid = GridView::new(
    move || (0..count).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .horizontal(true)
  .cell_size(20, 20)
  .height(100)
  .offset(offset);
  h.window(h.panel().child(tune(grid)), 200.0, height)
}

#[test]
fn a_horizontal_grid_fills_each_column_top_to_bottom_and_scrolls_sideways() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let offset = h.cx.signal(0.0);
  let window = sideways(&mut h, 10_000, 100.0, offset, |grid| grid);
  // Five rows of 20 in 100; five columns show and one more is built.
  assert_eq!(cells(&window).len(), 30);
  assert_eq!(cell(&window, 7), Some(Rect::new(20.0, 40.0, 20.0, 20.0)));
  offset.set(20_000.0);
  h.frame(&window);
  h.frame(&window);
  // Column 1000 at the left, with a column each side.
  let numbers: Vec<u32> = cells(&window).iter().map(|&(n, _)| n).collect();
  assert_eq!(numbers.first(), Some(&4995));
  assert_eq!(numbers.len(), 35);
  assert_eq!(
    cell(&window, 5000).map(|rect| (rect.x, rect.y)),
    Some((0.0, 0.0))
  );
  assert!(!window.needs_layout(), "a grid at rest lays out nothing");
  drop(window);
  assert_eq!(h.runtime.node_count(), before + 1, "only the test's offset");
}

#[test]
fn a_horizontal_grids_current_cell_and_highlight_go_by_column() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(7));
  let window = sideways(&mut h, 1000, 100.0, offset, |grid| {
    grid.current(move || current.get()).highlight(HIGHLIGHT)
  });
  assert_eq!(
    highlight(&window),
    Some((Point::new(20.0, 40.0), Size::new(20.0, 20.0)))
  );
  current.set(Some(57));
  h.frame(&window);
  // Column 11 ends at 240.
  assert_eq!(offset.get(), 140.0);
  assert_eq!(cell(&window, 57).map(|rect| rect.right()), Some(100.0));
  current.set(Some(3));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
}

#[test]
fn a_horizontal_grid_made_taller_keeps_its_first_shown_cell_in_view() {
  let mut h = Harness::new();
  let offset = h.cx.signal(205.0);
  let window = sideways(&mut h, 1000, 100.0, offset, |grid| grid);
  // Column 10, cell 50, 5 pixels out to the left.
  assert_eq!(cell(&window, 50).map(|rect| rect.x), Some(-5.0));
  window.set_size(Size::new(200.0, 200.0));
  h.frame(&window);
  // Ten rows: cell 50 is in column 5, still 5 pixels out.
  assert_eq!(offset.get(), 105.0);
  assert_eq!(cell(&window, 50).map(|rect| rect.x), Some(-5.0));
}
