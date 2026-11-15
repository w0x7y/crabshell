//! `geometry`: a widget's laid-out rectangle, written after each layout
//! that moves it, and how a feedback through it settles.

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, rect_at};
// Through the prelude, as a config reaches it.
use crate::prelude::Rect;
use crate::{Align, Column, Icon, Row, ScrollView, Stack, Style, Text};

fn rect_signal(h: &Harness) -> Signal<Rect> {
  h.cx.signal(Rect::default())
}

/// Counts the changes `signal` notifies, from an effect that reads it.
fn watch(h: &Harness, signal: Signal<Rect>) -> Counter {
  let runs = Counter::default();
  let counter = runs.clone();
  h.cx.effect(move || {
    signal.get();
    counter.hit();
  });
  h.runtime.flush();
  runs
}

#[test]
fn a_widget_writes_its_rectangle_after_layout() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let row = Row::new()
    .padding(4, 8)
    .gap(2)
    .child(Icon::new("a").size(10))
    .child(Icon::new("b").size(12).geometry(geometry));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(geometry.get_untracked(), Rect::new(20.0, 4.0, 12.0, 22.0));
  assert_eq!(geometry.get_untracked(), rect_at(&window, &[0, 1]));
}

#[test]
fn a_layout_that_moves_nothing_writes_nothing() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let label = h.cx.signal(String::from("short"));
  let width = h.cx.signal(10);
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("a").size(move || width.get()))
    .child(Icon::new("b").size(10).geometry(geometry))
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let runs = watch(&h, geometry);

  // The text after it changes; it stays where it was.
  label.set(String::from("a good deal longer"));
  h.frame(&window);
  assert_eq!(runs.get(), 1);

  width.set(20);
  h.frame(&window);
  assert_eq!(runs.get(), 2);
  assert_eq!(geometry.get_untracked(), Rect::new(20.0, 0.0, 10.0, 10.0));
}

#[test]
fn scrolling_moves_the_rectangle_and_a_transform_doesnt() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let offset = h.cx.signal(0.0);
  let shifted = h.cx.signal(false);
  let content = Column::new().child(Icon::new("spacer").size(100)).child(
    Icon::new("a").size(10).geometry(geometry).style(move || {
      if shifted.get() {
        Style::new().translate(5.0, 5.0).scale(2.0)
      } else {
        Style::new()
      }
    }),
  );
  let scroll = ScrollView::new(content)
    .offset(offset)
    .style(Style::new().height(50.0));
  let window = h.window(h.panel().child(scroll), 200.0, 50.0);
  assert_eq!(geometry.get_untracked().y, 100.0);

  offset.set(60.0);
  h.frame(&window);
  assert_eq!(geometry.get_untracked().y, 40.0);

  let runs = watch(&h, geometry);
  shifted.set(true);
  h.frame(&window);
  assert_eq!(runs.get(), 1);
}

#[test]
fn a_hidden_widget_writes_nothing() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let shown = h.cx.signal(true);
  let first = h.cx.signal(10);
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("a").size(move || first.get()))
    .child(
      Icon::new("b")
        .size(10)
        .geometry(geometry)
        .visible(move || shown.get()),
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let runs = watch(&h, geometry);

  shown.set(false);
  h.frame(&window);
  first.set(30);
  h.frame(&window);
  assert_eq!(runs.get(), 1);
  assert_eq!(geometry.get_untracked().x, 10.0);

  shown.set(true);
  h.frame(&window);
  assert_eq!(geometry.get_untracked().x, 30.0);
}

#[test]
fn a_box_sized_from_a_rectangle_settles_in_one_layout_call() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let runs = Counter::default();
  let counter = runs.clone();
  let column = Column::new()
    .align(Align::Start)
    .child(Text::new("measure me").geometry(geometry))
    .child(Stack::new().style(move || {
      counter.hit();
      Style::new().width(geometry.get().width).height(4.0)
    }));
  let mut window = None;
  let logs = capture_logs(|| window = Some(h.window(h.panel().child(column), 200.0, 60.0)));
  assert!(!logs.contains("still moved"), "{logs}");
  let window = window.expect("the window was built");
  let text = rect_at(&window, &[0, 0]);
  assert!(text.width > 0.0);
  // The underline took the text's width within the one call, and the
  // window has nothing left to do.
  assert_eq!(rect_at(&window, &[0, 1]).width, text.width);
  assert!(!window.needs_layout());
  let settled = runs.get();

  // A layout that moves nothing writes nothing, so the box isn't asked.
  window.set_size(crate::Size::new(200.0, 80.0));
  h.frame(&window);
  assert_eq!(runs.get(), settled);
}

#[test]
fn a_feedback_that_never_settles_stops_and_is_logged() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let row = Row::new().align(Align::Start).child(
    Stack::new()
      .geometry(geometry)
      .style(move || Style::new().width(geometry.get().width + 1.0)),
  );
  let window = crate::Window::new(&h.cx, h.panel().child(row));
  window.set_size(crate::Size::new(200.0, 30.0));
  h.runtime.flush();
  let logs = capture_logs(|| window.layout(&mut h.text));
  assert!(
    logs.contains("PanelWindow[DP-1] still moved after 8 layouts that wrote `geometry`"),
    "{logs}"
  );
  // It goes on at the next layout, never on its own.
  assert!(window.needs_layout());
  // One pixel wider each layout: 1 after the first, 8 after the last.
  assert_eq!(geometry.get_untracked().width, 8.0);
}

#[test]
fn a_panicking_effect_on_a_rectangle_is_logged_and_the_rest_runs_on() {
  let mut h = Harness::new();
  let geometry = rect_signal(&h);
  let width = h.cx.signal(10);
  h.cx
    .effect(move || assert!(geometry.get().width < 15.0, "too wide"))
    .named("sizes from geometry");
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("a").size(move || width.get()).geometry(geometry))
    .child(Icon::new("b").size(10));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    width.set(20);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect sizes from geometry panicked and is disabled"),
    "{logs}"
  );
  assert_eq!(rect_at(&window, &[0, 1]).x, 20.0);
}

#[test]
fn a_disposed_signal_is_skipped() {
  let mut h = Harness::new();
  let owner = h.runtime.new_owner();
  let geometry = owner.signal(Rect::default());
  let width = h.cx.signal(10);
  let row = Row::new().child(Icon::new("a").size(move || width.get()).geometry(geometry));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  owner.dispose();
  width.set(20);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]).width, 20.0);
}

#[test]
fn a_layout_that_writes_no_rectangle_runs_no_effects() {
  let mut h = Harness::new();
  let width = h.cx.signal(10);
  let other = h.cx.signal(0);
  let runs = Counter::default();
  let counter = runs.clone();
  h.cx.effect(move || {
    other.get();
    counter.hit();
  });
  let row = Row::new().child(Icon::new("a").size(move || width.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  width.set(20);
  h.runtime.flush();
  // Queued for the event loop's next flush, not run by the layout.
  other.set(1);
  window.layout(&mut h.text);
  assert_eq!(runs.get(), 1);
}
