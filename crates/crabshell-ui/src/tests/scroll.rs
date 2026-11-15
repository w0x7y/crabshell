//! Scroll views and lists: offsets, clamping, the wheel and building only
//! the rows in view.

use super::{Counter, Harness, node_at, paths, pixels, rect_at, wheel};
use crate::{
  Align, Button, Column, Icon, ListView, Point, Rect, Row, ScrollView, Style, Text, Window,
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
  window.pointer_scroll(wheel(120));
  assert_eq!(offset.get(), 0.0, "nothing under the pointer yet");
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(wheel(120));
  assert_eq!(offset.get(), 48.0);
  h.frame(&window);
  assert_eq!(first_icon(&window).y, -48.0);
  window.pointer_scroll(pixels(-8.0));
  assert_eq!(offset.get(), 40.0);
  // Scrolling stops at the end instead of running past it.
  h.frame(&window);
  window.pointer_scroll(wheel(120 * 10));
  assert_eq!(offset.get(), 150.0);
  h.frame(&window);
  window.pointer_scroll(pixels(f32::NAN));
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
  window.pointer_scroll(wheel(120));
  assert_eq!((steps.get(), offset.get()), (1, 0.0));
  window.pointer_motion(Point::new(10.0, 30.0));
  window.pointer_scroll(wheel(120));
  assert_eq!((steps.get(), offset.get()), (1, 48.0));
}

/// A list of `count` numbered rows, 20 pixels each, in a 100-pixel list.
fn numbers(
  h: &mut Harness,
  count: crabshell_reactive::Signal<u32>,
) -> (Window, crabshell_reactive::Signal<f32>) {
  let offset = h.cx.signal(0.0);
  let list = ListView::new(
    move || (0..count.get()).collect::<Vec<_>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .row_height(20)
  .height(100)
  .offset(offset);
  let window = h.window(h.panel().child(list), 100.0, 200.0);
  (window, offset)
}

/// The numbers of the rows built.
fn rows_built(window: &Window) -> Vec<String> {
  let view = window.view();
  // ListView, its scroll view, the rows' column; after the spacer, rows.
  let column = node_at(view.root(), &[0, 0, 0]);
  column
    .children()
    .skip(1)
    .map(
      |row| match row.children().next().map(|text| text.content()) {
        Some(crate::Content::Text(text)) => text.content().to_owned(),
        _ => String::from("?"),
      },
    )
    .collect()
}

#[test]
fn a_list_builds_only_the_rows_in_view_and_frees_the_rest() {
  let mut h = Harness::new();
  let count = h.cx.signal(1000);
  let (window, offset) = numbers(&mut h, count);
  h.frame(&window);
  // Rows 0 to 4 show; one more is built below.
  assert_eq!(rows_built(&window), ["0", "1", "2", "3", "4", "5"]);
  let built = h.runtime.node_count();

  offset.set(10_000.0);
  h.frame(&window);
  h.frame(&window);
  // Rows 500 to 504 show, with one more each side.
  assert_eq!(
    rows_built(&window),
    ["499", "500", "501", "502", "503", "504", "505"]
  );
  let row = |index: usize| rect_at(&window, &[0, 0, 0, index + 1]);
  assert_eq!(row(1).y, 0.0, "row 500 is at the top");
  // As many rows as before, give or take the extra one above.
  assert!(
    h.runtime.node_count() <= built + 8,
    "{} > {built}",
    h.runtime.node_count()
  );
  assert!(
    paths(&window)
      .iter()
      .any(|path| path.starts_with("PanelWindow[DP-1]/ListView"))
  );
}

#[test]
fn a_list_that_shrinks_while_scrolled_shows_its_last_rows() {
  let mut h = Harness::new();
  let count = h.cx.signal(1000);
  let (window, offset) = numbers(&mut h, count);
  offset.set(10_000.0);
  h.frame(&window);
  count.set(8);
  h.frame(&window);
  h.frame(&window);
  // Eight rows scroll at most 60 pixels: rows 3 to 7 show.
  assert_eq!(rows_built(&window), ["2", "3", "4", "5", "6", "7"]);
}

#[test]
fn current_scrolls_as_little_as_needed_to_show_its_row() {
  let mut h = Harness::new();
  let current = h.cx.signal(Some(0_usize));
  let offset = h.cx.signal(0.0);
  let list = ListView::new(
    || (0..100).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .row_height(20)
  .height(100)
  .current(move || current.get())
  .offset(offset);
  let window = h.window(h.panel().child(list), 100.0, 200.0);
  current.set(Some(4));
  h.frame(&window);
  assert_eq!(offset.get(), 0.0, "row 4 already shows");
  current.set(Some(5));
  h.frame(&window);
  assert_eq!(offset.get(), 20.0, "row 5 at the bottom");
  current.set(Some(50));
  h.frame(&window);
  assert_eq!(offset.get(), 920.0);
  current.set(Some(10));
  h.frame(&window);
  assert_eq!(offset.get(), 200.0, "row 10 at the top");
  current.set(None);
  h.frame(&window);
  assert_eq!(offset.get(), 200.0);
}

#[test]
fn dropping_a_list_frees_everything_it_made() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let count = h.cx.signal(100);
  let (window, offset) = numbers(&mut h, count);
  offset.set(500.0);
  h.frame(&window);
  drop(window);
  // The two signals the test made outlive the window.
  assert_eq!(h.runtime.node_count(), before + 2);
}

#[test]
fn a_row_that_panics_as_it_is_built_is_logged_and_the_rest_show() {
  let mut h = Harness::new();
  let list = ListView::new(
    || (0..5).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| {
      assert!(n.get_untracked() != 2, "row failure");
      Text::new(move || n.get().to_string())
    },
  )
  .row_height(20)
  .height(100);
  let logs = super::capture_logs(|| {
    let window = h.window(h.panel().child(list), 100.0, 200.0);
    h.frame(&window);
    // Row 2 shows empty in its place.
    assert_eq!(rows_built(&window), ["0", "1", "?", "3", "4"]);
  });
  assert!(logs.contains("PanelWindow[DP-1]/ListView"), "{logs}");
  assert!(logs.contains("row failure"), "{logs}");
}

#[test]
fn scrolls_before_the_next_layout_add_up() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = ten_icons(&mut h, offset);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(wheel(120));
  window.pointer_scroll(wheel(120));
  assert_eq!(offset.get(), 96.0);
  // A config's own write counts too.
  offset.set(10.0);
  window.pointer_scroll(pixels(5.0));
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
  window.pointer_scroll(wheel(120));
  h.frame(&window);
}

#[test]
fn the_wheel_scrolls_a_list_and_current_allows_for_its_padding() {
  let mut h = Harness::new();
  let current = h.cx.signal(None::<usize>);
  let offset = h.cx.signal(0.0);
  let list = ListView::new(
    || (0..100).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .row_height(20)
  .height(100)
  .current(move || current.get())
  .offset(offset)
  .style(Style::new().padding(10, 0));
  let window = h.window(h.panel().child(list), 100.0, 200.0);
  window.pointer_motion(Point::new(10.0, 50.0));
  window.pointer_scroll(wheel(120));
  assert_eq!(offset.get(), 48.0);
  // 80 pixels show inside the padding: row 7 ends at 160.
  offset.set(0.0);
  current.set(Some(7));
  h.frame(&window);
  assert_eq!(offset.get(), 80.0);
}

#[test]
fn current_allows_for_padding_that_differs_above_and_below() {
  let mut h = Harness::new();
  let current = h.cx.signal(None::<usize>);
  let offset = h.cx.signal(0.0);
  let list = ListView::new(
    || (0..100).collect::<Vec<u32>>(),
    |n| *n,
    |_, n| Text::new(move || n.get().to_string()),
  )
  .row_height(20)
  .height(100)
  .current(move || current.get())
  .offset(offset)
  .style(Style::new().padding_top(10).padding_bottom(30));
  let window = h.window(h.panel().child(list), 100.0, 200.0);
  // 60 pixels show between the paddings: row 7 ends at 160.
  current.set(Some(7));
  h.frame(&window);
  assert_eq!(offset.get(), 100.0);
}
