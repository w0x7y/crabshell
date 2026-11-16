//! List views whose rows differ in length: measuring each row, placing the
//! rest after it, keeping the shown rows still and finding `current`.

use std::time::Instant;

use crabshell_reactive::Signal;

use super::{Harness, node_at, pixels, rect_at, wheel};
use crate::{
  Content, Icon, ListView, NodeView, Point, Rect, Row, Style, Text, Window, advance_animations,
  animating,
};

/// A row labeled `n`, `length` pixels tall: an icon that size and its
/// number, whose text is shorter than any row here.
fn labeled(n: u32, length: u32) -> Row {
  Row::new()
    .child(Icon::new("x").size(length))
    .child(Text::new(n.to_string()))
}

/// The text of the first text node under `node`, if any.
fn label(node: NodeView<'_>) -> Option<String> {
  if let Content::Text(text) = node.content() {
    return Some(text.content().to_owned());
  }
  node.children().find_map(label)
}

/// The list's rows' column: under the panel, the list and its scroll view.
fn column(view: &crate::WindowView<'_>) -> Vec<(String, Rect)> {
  node_at(view.root(), &[0, 0, 0])
    .children()
    .skip(1)
    .map(|row| (label(row).unwrap_or_default(), row.rect()))
    .collect()
}

/// The labels and rectangles of the rows built.
fn rows(window: &Window) -> Vec<(String, Rect)> {
  column(&window.view())
}

/// Where row `n` is, if it is built.
fn row(window: &Window, n: u32) -> Option<Rect> {
  let n = n.to_string();
  rows(window)
    .into_iter()
    .find(|(label, _)| *label == n)
    .map(|(_, rect)| rect)
}

/// A 100-pixel list of `count` rows estimated at 20 pixels, row `n`
/// `length(n)` tall.
fn list(
  h: &mut Harness,
  items: Signal<Vec<u32>>,
  length: impl Fn(u32) -> u32 + Copy + 'static,
  offset: Signal<f32>,
  tune: impl FnOnce(ListView) -> ListView,
) -> Window {
  let list = ListView::new(
    move || items.get(),
    |n| *n,
    move |_, n| labeled(n.get_untracked(), length(n.get_untracked())),
  )
  .row_height(20)
  .height(100)
  .offset(offset);
  h.window(h.panel().child(tune(list)), 100.0, 200.0)
}

fn every_third_tall(n: u32) -> u32 {
  if n.is_multiple_of(3) { 40 } else { 20 }
}

#[test]
fn rows_take_their_own_height_and_the_rest_follow() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(0.0);
  let window = list(&mut h, items, every_third_tall, offset, |list| list);
  let built: Vec<(String, Rect)> = rows(&window);
  // 0 to 80 holds rows 0 to 3, and one more is built below.
  let expected = [
    (0, 0.0, 40.0),
    (1, 40.0, 20.0),
    (2, 60.0, 20.0),
    (3, 80.0, 40.0),
  ];
  for (n, y, height) in expected {
    let rect = row(&window, n).unwrap_or_else(|| panic!("row {n} in {built:?}"));
    assert_eq!((rect.y, rect.height), (y, height), "row {n}");
  }
  assert_eq!(built.len(), 5, "{built:?}");
  // 34 rows of 40 and 66 of 20 scroll at most 2680 less the 100 shown,
  // once each is measured; until then each counts 20.
  offset.set(1e9);
  for _ in 0..40 {
    h.frame(&window);
  }
  assert_eq!(offset.get(), 1e9);
  let last = row(&window, 99).expect("the last row shows");
  assert_eq!(last.bottom(), 100.0);
  assert!(!window.needs_layout(), "a list at rest lays out nothing");
}

#[test]
fn a_row_too_short_is_still_row_height_tall() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect());
  let offset = h.cx.signal(0.0);
  let window = list(&mut h, items, |_| 8, offset, |list| list);
  assert_eq!(row(&window, 1), Some(Rect::new(0.0, 20.0, 100.0, 20.0)));
}

#[test]
fn a_row_that_grows_above_the_view_moves_nothing_shown() {
  let mut h = Harness::new();
  let tall = h.cx.signal(20);
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(100.0);
  let list = ListView::new(
    move || items.get(),
    |n| *n,
    move |_, n| {
      let n = n.get_untracked();
      Row::new()
        .child(Icon::new("x").size(move || if n == 4 { tall.get() } else { 20 }))
        .child(Text::new(n.to_string()))
    },
  )
  .row_height(20)
  .height(100)
  .offset(offset);
  let window = h.window(h.panel().child(list), 100.0, 200.0);
  assert_eq!(row(&window, 5).map(|rect| rect.y), Some(0.0));
  // Row 4 is built above the view; it grows up, not down.
  tall.set(50);
  h.frame(&window);
  assert_eq!(offset.get(), 130.0);
  assert_eq!(row(&window, 5).map(|rect| rect.y), Some(0.0));
  // A row in view pushes the ones after it down.
  tall.set(20);
  h.frame(&window);
  offset.set(80.0);
  h.frame(&window);
  tall.set(30);
  h.frame(&window);
  assert_eq!(offset.get(), 80.0);
  assert_eq!(row(&window, 5).map(|rect| rect.y), Some(30.0));
}

#[test]
fn scrolling_up_onto_rows_longer_than_the_estimate_moves_them_evenly() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(1000.0);
  let window = list(&mut h, items, |_| 40, offset, |list| list);
  // Row 49, built above the view, measured 40: row 50 stays at the top.
  assert_eq!(offset.get(), 1020.0);
  assert_eq!(row(&window, 50).map(|rect| rect.y), Some(0.0));
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(wheel(-120));
  h.frame(&window);
  // A notch shows 48 pixels more above, whatever the rows there measure.
  assert_eq!(row(&window, 50).map(|rect| rect.y), Some(48.0));
}

#[test]
fn a_fling_runs_on_across_rows_it_measures() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..1000).collect());
  let offset = h.cx.signal(20_000.0);
  let window = list(&mut h, items, |_| 40, offset, |list| list);
  window.pointer_motion(Point::new(10.0, 10.0));
  let from = top_row(&window);
  for n in 0..5 {
    window.pointer_scroll_at(pixels(-20.0), n * 10);
    h.frame(&window);
  }
  window.pointer_scroll_at(super::lift(), 45);
  let start = Instant::now();
  let mut ticks = 0;
  while animating() && ticks < 1000 {
    advance_animations(start + std::time::Duration::from_millis(16 * ticks));
    h.frame(&window);
    ticks += 1;
  }
  // 2000 pixels a second slows to rest over about 1.3 s, some 1330
  // pixels on: 33 rows of 40, with the 100 pixels of the swipe 35.
  let rows = from - top_row(&window);
  assert!((34..=36).contains(&rows), "{rows} rows in {ticks} ticks");
  assert!(!window.needs_layout());
}

/// The number of the row at the top of the view.
fn top_row(window: &Window) -> u32 {
  rows(window)
    .into_iter()
    .find(|(_, rect)| rect.y <= 0.0 && rect.bottom() > 0.0)
    .and_then(|(label, _)| label.parse().ok())
    .expect("a row at the top")
}

#[test]
fn a_current_row_not_built_yet_is_found_by_the_estimate_then_shown_whole() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(None);
  let window = list(
    &mut h,
    items,
    |_| 40,
    offset,
    |list| list.current(move || current.get()),
  );
  current.set(Some(50));
  h.frame(&window);
  assert_eq!(row(&window, 50).map(|rect| rect.bottom()), Some(100.0));
  // Back up: the row is shown at the top.
  current.set(Some(10));
  h.frame(&window);
  assert_eq!(row(&window, 10).map(|rect| rect.y), Some(0.0));
  // Rows measured later don't pull the view back to it.
  offset.set(2000.0);
  h.frame(&window);
  assert!(offset.get() >= 2000.0, "{}", offset.get());
  assert_eq!(row(&window, 10), None);
}

#[test]
fn heights_are_kept_by_key_when_the_items_move() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect::<Vec<u32>>());
  let offset = h.cx.signal(0.0);
  let window = list(
    &mut h,
    items,
    |n| if n == 0 { 40 } else { 20 },
    offset,
    |list| list.scroll_bar(Style::new()),
  );
  // 40 and nine of 20: 220 pixels, of which 100 show.
  let handle = |window: &Window| {
    node_at(window.view().root(), &[0, 0])
      .scroll_bar()
      .map(|(rect, _)| rect.height)
  };
  assert_eq!(handle(&window), Some(100.0_f32 * 100.0 / 220.0));
  // Row 0 moves to the end, out of view: it still counts 40, and row 1,
  // now first, still 20.
  items.set((1..10).chain([0]).collect());
  h.frame(&window);
  assert_eq!(handle(&window), Some(100.0_f32 * 100.0 / 220.0));
  items.set((1..10).collect());
  h.frame(&window);
  assert_eq!(handle(&window), Some(100.0_f32 * 100.0 / 180.0));
}

#[test]
fn a_key_that_panics_is_logged_with_the_lists_path_and_the_rest_runs() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect::<Vec<u32>>());
  let shown = h.cx.signal(String::from("before"));
  let list = ListView::new(
    move || items.get(),
    |n| {
      assert!(*n != 12, "key failure");
      *n
    },
    |_, n| labeled(n.get_untracked(), 20),
  )
  .row_height(20)
  .height(100);
  let logs = super::capture_logs(|| {
    let column = crate::Column::new()
      .child(list)
      .child(Text::new(move || shown.get()));
    let window = h.window(h.panel().child(column), 100.0, 200.0);
    items.set((10..20).collect());
    h.frame(&window);
    shown.set(String::from("after"));
    h.frame(&window);
    let view = window.view();
    assert_eq!(
      label(node_at(view.root(), &[0, 1])).as_deref(),
      Some("after")
    );
  });
  assert!(logs.contains("PanelWindow[DP-1]/Column/ListView"), "{logs}");
  assert!(logs.contains("key failure"), "{logs}");
}

/// The rectangles of a horizontal list's items built, in order, under a
/// row that keeps the list its own height.
fn items_built(window: &Window) -> Vec<Rect> {
  let view = window.view();
  node_at(view.root(), &[0, 0, 0, 0])
    .children()
    .skip(1)
    .map(|item| item.rect())
    .collect()
}

/// A 100-pixel-wide horizontal list of `count` squares estimated at 20,
/// item `n` `length(n)` wide.
fn across(
  h: &mut Harness,
  count: u32,
  length: impl Fn(u32) -> u32 + Copy + 'static,
  offset: Signal<f32>,
  tune: impl FnOnce(ListView) -> ListView,
) -> Window {
  let list = ListView::new(
    move || (0..count).collect::<Vec<u32>>(),
    |n| *n,
    move |_, n| Icon::new("x").size(length(n.get_untracked())),
  )
  .horizontal(true)
  .row_height(20)
  .height(100)
  .offset(offset);
  let row = Row::new().align(crate::Align::Start).child(tune(list));
  h.window(h.panel().child(row), 200.0, 200.0)
}

#[test]
fn a_horizontal_list_runs_its_items_left_to_right_each_its_own_width() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let window = across(&mut h, 100, every_third_tall, offset, |list| list);
  let xs: Vec<(f32, f32)> = items_built(&window)
    .iter()
    .map(|rect| (rect.x, rect.width))
    .collect();
  assert_eq!(
    xs,
    [
      (0.0, 40.0),
      (40.0, 20.0),
      (60.0, 20.0),
      (80.0, 40.0),
      (120.0, 20.0)
    ]
  );
  // The list is 100 wide and as tall as its tallest item.
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 100.0, 40.0));
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_scroll(wheel(120));
  assert_eq!(offset.get(), 48.0, "a turned wheel scrolls it sideways");
  h.frame(&window);
  assert_eq!(items_built(&window)[1].x, 40.0 - 48.0);
}

#[test]
fn current_in_a_horizontal_list_scrolls_sideways_within_its_padding() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(None);
  let window = across(
    &mut h,
    100,
    |_| 20,
    offset,
    |list| {
      list
        .current(move || current.get())
        .style(Style::new().padding(0, 10))
    },
  );
  // 80 pixels show between the paddings: item 7 ends at 160.
  current.set(Some(7));
  h.frame(&window);
  assert_eq!(offset.get(), 80.0);
}

#[test]
fn a_list_turned_sideways_measures_its_items_again() {
  let mut h = Harness::new();
  let sideways = h.cx.signal(false);
  let offset = h.cx.signal(0.0);
  // Items 40 tall and 20 wide.
  let list = ListView::new(
    || (0..10).collect::<Vec<u32>>(),
    |n| *n,
    |_, _| crate::Stack::new().style(Style::new().width(20.0).height(40.0)),
  )
  .horizontal(move || sideways.get())
  .row_height(10)
  .height(100)
  .offset(offset)
  .scroll_bar(Style::new());
  let row = Row::new().align(crate::Align::Start).child(list);
  let window = h.window(h.panel().child(row), 200.0, 200.0);
  assert_eq!(items_built(&window)[1].y, 40.0);
  // Every item measured 40 tall, then the last ones freed.
  offset.set(1000.0);
  h.frame(&window);
  offset.set(0.0);
  h.frame(&window);
  sideways.set(true);
  h.frame(&window);
  let built = items_built(&window);
  assert_eq!((built[1].x, built[1].y), (20.0, 0.0));
  // Those not built again count 10 wide, not 40: 6 of 20 and 4 of 10.
  let handle = node_at(window.view().root(), &[0, 0, 0]).scroll_bar();
  assert_eq!(
    handle.map(|(rect, _)| rect.width),
    Some(100.0_f32 * 100.0 / 160.0)
  );
  // Ten items of 20 scroll at most 100.
  offset.set(1000.0);
  h.frame(&window);
  h.frame(&window);
  assert_eq!(
    items_built(&window).last().map(|rect| rect.right()),
    Some(100.0)
  );
}

const HIGHLIGHT: Style = Style::new()
  .background(crate::Color::rgb(0x45, 0x47, 0x5a))
  .transition(crate::Transition::new(100).easing(crate::Easing::Linear));

/// The highlight under the content at `content`, if it shows: where it
/// is moved to and its box's size.
fn highlight_at(window: &Window, content: &[usize]) -> Option<(Point, crate::Size)> {
  let view = window.view();
  let first = node_at(view.root(), content).children().next()?;
  // The spacer has no children; the highlight has its box.
  let shape = first.children().next()?.rect();
  Some((
    first.style().translate.unwrap_or_default(),
    crate::Size::new(shape.width, shape.height),
  ))
}

/// A vertical list's highlight.
fn highlight(window: &Window) -> Option<(Point, crate::Size)> {
  highlight_at(window, &[0, 0, 0])
}

fn at(x: f32, y: f32) -> Option<Point> {
  Some(Point::new(x, y))
}

#[test]
fn the_highlight_moves_to_the_new_current_row_over_its_transition() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(1));
  let window = list(&mut h, items, every_third_tall, offset, |list| {
    list.current(move || current.get()).highlight(HIGHLIGHT)
  });
  // Row 1 is 20 tall, after row 0's 40; the box spans the row.
  assert_eq!(
    highlight(&window),
    Some((Point::new(0.0, 40.0), crate::Size::new(100.0, 20.0)))
  );
  current.set(Some(3));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + std::time::Duration::from_millis(50));
  assert!(
    !window.tree.borrow().needs_layout,
    "a move places the window again without laying it out"
  );
  h.frame(&window);
  // Halfway from 40 to row 3's 80, with row 3's height at once.
  assert_eq!(
    highlight(&window),
    Some((Point::new(0.0, 60.0), crate::Size::new(100.0, 40.0)))
  );
  advance_animations(start + std::time::Duration::from_millis(100));
  h.frame(&window);
  assert_eq!(highlight(&window).map(|(at, _)| at), at(0.0, 80.0));
  assert!(
    !animating() && crate::next_timer().is_none() && !window.needs_layout(),
    "a highlight at rest wakes nothing"
  );
}

#[test]
fn no_current_row_or_no_style_draws_no_highlight() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(None);
  let style = h.cx.signal(Some(HIGHLIGHT));
  let window = list(
    &mut h,
    items,
    |_| 20,
    offset,
    |list| {
      list
        .current(move || current.get())
        .highlight(move || style.get())
    },
  );
  assert_eq!(highlight(&window), None);
  current.set(Some(2));
  h.frame(&window);
  // It appears where the row is, without sliding there.
  assert_eq!(highlight(&window).map(|(at, _)| at), at(0.0, 40.0));
  assert!(!animating());
  style.set(None);
  h.frame(&window);
  assert_eq!(highlight(&window), None);
  style.set(Some(HIGHLIGHT));
  current.set(Some(12));
  h.frame(&window);
  assert_eq!(highlight(&window), None, "past the end");
}

#[test]
fn a_row_measured_late_moves_the_highlight_at_once() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..100).collect());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(1));
  // Every row is 40, twice the estimate.
  let window = list(
    &mut h,
    items,
    |_| 40,
    offset,
    |list| list.current(move || current.get()).highlight(HIGHLIGHT),
  );
  assert_eq!(highlight(&window).map(|(at, _)| at), at(0.0, 40.0));
  // Far off, found by the estimate, then measured in the same layout:
  // it is there before the first tick, not on its way to the estimate.
  current.set(Some(50));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  h.frame(&window);
  let shown = row(&window, 50).expect("row 50 shows").y + offset.get();
  assert_eq!(highlight(&window).map(|(at, _)| at), at(0.0, shown));
  assert!(shown > 1000.0, "past the estimate's 1000: {shown}");
}

#[test]
fn the_highlight_follows_its_row_when_the_items_change_mid_move() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect::<Vec<u32>>());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(0));
  let window = list(&mut h, items, every_third_tall, offset, |list| {
    list.current(move || current.get()).highlight(HIGHLIGHT)
  });
  current.set(Some(4));
  h.frame(&window);
  let start = Instant::now();
  advance_animations(start);
  advance_animations(start + std::time::Duration::from_millis(50));
  h.frame(&window);
  // On the way to row 4 at 120, two rows go: index 4 is row 6, at 100,
  // and the move turns there.
  let before = highlight(&window).map(|(at, _)| at.y);
  items.set((2..10).collect());
  h.frame(&window);
  advance_animations(start + std::time::Duration::from_millis(66));
  h.frame(&window);
  let turning = highlight(&window).map(|(at, _)| at.y);
  assert!(
    turning > before && turning < Some(100.0),
    "{before:?} {turning:?}"
  );
  advance_animations(start + std::time::Duration::from_millis(250));
  h.frame(&window);
  assert_eq!(highlight(&window).map(|(at, _)| at), at(0.0, 100.0));
  items.set((0..3).collect());
  h.frame(&window);
  assert_eq!(highlight(&window), None, "its row is gone");
}

#[test]
fn a_horizontal_lists_highlight_moves_sideways_and_is_as_tall_as_the_list() {
  let mut h = Harness::new();
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(1));
  let window = across(&mut h, 100, every_third_tall, offset, |list| {
    list.current(move || current.get()).highlight(HIGHLIGHT)
  });
  assert_eq!(
    highlight_at(&window, &[0, 0, 0, 0]),
    Some((Point::new(40.0, 0.0), crate::Size::new(20.0, 40.0)))
  );
}

#[test]
fn a_highlight_that_panics_is_logged_with_the_lists_path_and_the_list_runs_on() {
  let mut h = Harness::new();
  let items = h.cx.signal((0..10).collect());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(1));
  let logs = super::capture_logs(|| {
    let window = list(
      &mut h,
      items,
      |_| 20,
      offset,
      |list| {
        list.current(move || current.get()).highlight(move || {
          assert!(current.get() != Some(3), "highlight failure");
          Some(HIGHLIGHT)
        })
      },
    );
    current.set(Some(3));
    h.frame(&window);
    current.set(Some(8));
    h.frame(&window);
    assert_eq!(offset.get(), 80.0, "current still scrolls");
    assert_eq!(row(&window, 8).map(|rect| rect.bottom()), Some(100.0));
  });
  assert!(logs.contains("PanelWindow[DP-1]/ListView"), "{logs}");
  assert!(logs.contains("highlight failure"), "{logs}");
}

#[test]
fn a_current_row_set_before_the_items_arrive_is_shown_once_they_do() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::new());
  let offset = h.cx.signal(0.0);
  let current = h.cx.signal(Some(50));
  let window = list(
    &mut h,
    items,
    |_| 20,
    offset,
    |list| list.current(move || current.get()),
  );
  items.set((0..100).collect());
  h.frame(&window);
  assert_eq!(row(&window, 50).map(|rect| rect.bottom()), Some(100.0));
  // Items that keep it in range don't pull the view back to it.
  offset.set(0.0);
  h.frame(&window);
  items.set((0..90).collect());
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  // Out of range and back in, it is shown again.
  items.set((0..10).collect());
  h.frame(&window);
  items.set((0..100).collect());
  h.frame(&window);
  assert_eq!(row(&window, 50).map(|rect| rect.bottom()), Some(100.0));
}
