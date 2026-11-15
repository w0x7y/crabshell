//! Pointer input: hover, press, click, scroll and the cursor.

use std::cell::{Cell, RefCell};

use std::rc::Rc;

use crabshell_reactive::Signal;

use super::{Counter, Harness, capture_logs, lift, pixels, rect_at, wheel};
use crate::{
  Align, Button, Color, CursorShape, Icon, Point, PointerButton, Rect, Row, Style, Text, Window,
};

const NORMAL: Style = Style::new().background(Color::rgb(1, 1, 1));
const HOVER: Style = Style::new().background(Color::rgb(2, 2, 2));
const PRESSED: Style = Style::new().background(Color::rgb(3, 3, 3));

/// The middle of the node at `path`.
fn center(window: &Window, path: &[usize]) -> Point {
  let rect = rect_at(window, path);
  Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn background(window: &Window, path: &[usize]) -> Option<Color> {
  super::node_at(window.view().root(), path)
    .style()
    .background
}

fn click(window: &Window, button: PointerButton) {
  window.pointer_button(button, true);
  window.pointer_button(button, false);
}

/// A row with two 20-pixel buttons, at x = 0 and x = 20.
fn two_buttons(h: &mut Harness, first: Button, second: Button) -> Window {
  let row = Row::new().align(Align::Start).child(first).child(second);
  h.window(h.panel().child(row), 200.0, 30.0)
}

fn square() -> Icon {
  Icon::new("x").size(20)
}

#[test]
fn hovering_a_button_shows_the_pointer_and_its_hover_style() {
  let mut h = Harness::new();
  let window = two_buttons(
    &mut h,
    Button::new(square()).style(NORMAL).hover_style(HOVER),
    Button::new(square()),
  );
  window.take_damage();
  assert_eq!(window.cursor(), CursorShape::Default);

  let cursor = window.pointer_motion(center(&window, &[0, 0]));
  assert_eq!(cursor, CursorShape::Pointer);
  assert_eq!(background(&window, &[0, 0]), HOVER.background);
  assert_eq!(window.take_damage(), vec![Rect::new(0.0, 0.0, 20.0, 20.0)]);
  assert!(!window.needs_layout());

  assert_eq!(
    window.pointer_motion(Point::new(100.0, 25.0)),
    CursorShape::Default
  );
  assert_eq!(background(&window, &[0, 0]), NORMAL.background);

  window.pointer_motion(center(&window, &[0, 0]));
  window.pointer_leave();
  assert_eq!(window.cursor(), CursorShape::Default);
  assert_eq!(background(&window, &[0, 0]), NORMAL.background);
}

#[test]
fn the_deepest_button_under_the_pointer_wins() {
  let mut h = Harness::new();
  let outer_clicks = Counter::default();
  let inner_clicks = Counter::default();
  let (outer, inner) = (outer_clicks.clone(), inner_clicks.clone());
  let row = Row::new().align(Align::Start).child(
    Button::new(
      Row::new()
        .child(square())
        .child(Button::new(square()).on_click(move |_| inner.hit())),
    )
    .on_click(move |_| outer.hit()),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);

  window.pointer_motion(center(&window, &[0, 0, 0, 1]));
  click(&window, PointerButton::Left);
  window.pointer_motion(center(&window, &[0, 0, 0, 0]));
  click(&window, PointerButton::Left);
  assert_eq!((outer_clicks.get(), inner_clicks.get()), (1, 1));
}

#[test]
fn a_click_runs_the_handler_with_the_buttons_context() {
  let mut h = Harness::new();
  let count = h.cx.signal(0);
  let seen = Rc::new(RefCell::new(None));
  let seen_in_handler = Rc::clone(&seen);
  let button =
    Button::new(Text::new(move || format!("clicked {}", count.get()))).on_click(move |cx| {
      count.update(|n| *n += 1);
      *seen_in_handler.borrow_mut() = Some(cx.owner());
    });
  let window = h.window(h.panel().child(Row::new().child(button)), 200.0, 30.0);

  window.pointer_motion(center(&window, &[0, 0]));
  click(&window, PointerButton::Left);
  h.frame(&window);
  assert_eq!(count.get(), 1);
  let view = window.view();
  let crate::Content::Text(text) = super::node_at(view.root(), &[0, 0, 0]).content() else {
    panic!("a text node has text content");
  };
  assert_eq!(text.content(), "clicked 1");
  assert!(seen.borrow().is_some());
}

#[test]
fn a_handlers_context_is_its_buttons() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let made = Rc::new(Cell::new(None));
  let slot = Rc::clone(&made);
  let row = Row::new().align(Align::Start).children_keyed(
    move || items.get(),
    |n| *n,
    move |_cx, _n| {
      let slot = Rc::clone(&slot);
      Button::new(square()).on_click(move |cx| slot.set(Some(cx.signal(7))))
    },
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  click(&window, PointerButton::Left);
  let made: Signal<i32> = made.get().expect("the handler ran");
  assert_eq!(made.try_get(), Some(7));

  // A signal made through the handler's context goes with the button,
  // not with the window.
  items.set(Vec::new());
  h.frame(&window);
  assert_eq!(made.try_get(), None);
}

#[test]
fn a_click_needs_press_and_release_on_the_same_button() {
  let mut h = Harness::new();
  let first = Counter::default();
  let second = Counter::default();
  let (a, b) = (first.clone(), second.clone());
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_click(move |_| a.hit()),
    Button::new(square()).on_click(move |_| b.hit()),
  );

  window.pointer_motion(center(&window, &[0, 0]));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_motion(center(&window, &[0, 1]));
  window.pointer_button(PointerButton::Left, false);
  assert_eq!((first.get(), second.get()), (0, 0));

  // A release without a press does nothing either.
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(second.get(), 0);
}

#[test]
fn right_and_middle_buttons() {
  let mut h = Harness::new();
  let left = Counter::default();
  let right = Counter::default();
  let middle = Counter::default();
  let (l, r, m) = (left.clone(), right.clone(), middle.clone());
  let window = two_buttons(
    &mut h,
    Button::new(square())
      .on_click(move |_| l.hit())
      .on_right_click(move |_| r.hit())
      .on_middle_click(move |_| m.hit()),
    Button::new(square()),
  );
  window.pointer_motion(center(&window, &[0, 0]));
  click(&window, PointerButton::Right);
  click(&window, PointerButton::Middle);
  assert_eq!((left.get(), right.get(), middle.get()), (0, 1, 1));

  // A right press doesn't end with a left release.
  window.pointer_button(PointerButton::Right, true);
  window.pointer_button(PointerButton::Left, false);
  window.pointer_button(PointerButton::Right, false);
  assert_eq!((left.get(), right.get(), middle.get()), (0, 2, 1));

  // A button without a middle handler ignores the middle button.
  window.pointer_motion(center(&window, &[0, 1]));
  click(&window, PointerButton::Middle);
  assert_eq!((left.get(), right.get(), middle.get()), (0, 2, 1));
}

#[test]
fn the_pressed_style_shows_while_held_over_the_button() {
  let mut h = Harness::new();
  let window = two_buttons(
    &mut h,
    Button::new(square())
      .style(NORMAL)
      .hover_style(HOVER)
      .pressed_style(PRESSED),
    Button::new(square()),
  );
  window.pointer_motion(center(&window, &[0, 0]));
  window.pointer_button(PointerButton::Left, true);
  assert_eq!(background(&window, &[0, 0]), PRESSED.background);
  window.pointer_motion(center(&window, &[0, 1]));
  assert_eq!(background(&window, &[0, 0]), NORMAL.background);
  window.pointer_motion(center(&window, &[0, 0]));
  assert_eq!(background(&window, &[0, 0]), PRESSED.background);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(background(&window, &[0, 0]), HOVER.background);
}

#[test]
fn a_hover_style_with_padding_needs_a_layout() {
  let mut h = Harness::new();
  let window = two_buttons(
    &mut h,
    Button::new(square()).hover_style(Style::new().padding(0, 4)),
    Button::new(square()),
  );
  window.pointer_motion(center(&window, &[0, 0]));
  assert!(window.needs_layout());
  window.layout(&mut h.text);
  assert_eq!(rect_at(&window, &[0, 0]).width, 28.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 28.0);
}

#[test]
fn scrolling_sums_into_whole_steps_up_positive() {
  let mut h = Harness::new();
  let steps = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&steps);
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_scroll(move |_, n| log.borrow_mut().push(n)),
    Button::new(square()),
  );
  window.pointer_motion(center(&window, &[0, 0]));

  window.pointer_scroll(wheel(120));
  window.pointer_scroll(wheel(-240));
  // A high-resolution wheel: two half notches make one step.
  window.pointer_scroll(wheel(60));
  assert_eq!(*steps.borrow(), [-1, 2]);
  window.pointer_scroll(wheel(60));
  assert_eq!(*steps.borrow(), [-1, 2, -1]);

  // Touchpad: one step per 15 pixels, the rest kept for later.
  steps.borrow_mut().clear();
  window.pointer_scroll(pixels(10.0));
  window.pointer_scroll(pixels(10.0));
  assert_eq!(*steps.borrow(), [-1]);
  window.pointer_scroll(pixels(-40.0));
  assert_eq!(*steps.borrow(), [-1, 2]);

  // Partial scrolling is forgotten when the pointer moves to another button.
  steps.borrow_mut().clear();
  window.pointer_scroll(wheel(60));
  window.pointer_motion(center(&window, &[0, 1]));
  window.pointer_motion(center(&window, &[0, 0]));
  window.pointer_scroll(wheel(60));
  assert!(steps.borrow().is_empty());
  window.pointer_scroll(pixels(f32::NAN));
  assert!(steps.borrow().is_empty());
}

#[test]
fn a_touchpad_scroll_that_ends_forgets_its_partial_step() {
  let mut h = Harness::new();
  let steps = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&steps);
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_scroll(move |_, n| log.borrow_mut().push(n)),
    Button::new(square()),
  );
  window.pointer_motion(center(&window, &[0, 0]));
  window.pointer_scroll(pixels(10.0));
  // The fingers lift: the 10 pixels don't carry into the next gesture.
  window.pointer_scroll(lift());
  window.pointer_scroll(pixels(10.0));
  assert!(steps.borrow().is_empty());
  window.pointer_scroll(pixels(5.0));
  assert_eq!(*steps.borrow(), [-1]);
  // A wheel's partial notch isn't part of a gesture and stays.
  window.pointer_scroll(wheel(60));
  window.pointer_scroll(lift());
  window.pointer_scroll(wheel(60));
  assert_eq!(*steps.borrow(), [-1, -1]);
}

#[test]
fn a_panicking_handler_is_logged_and_disabled() {
  let mut h = Harness::new();
  let calls = Counter::default();
  let others = Counter::default();
  let (c, o) = (calls.clone(), others.clone());
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_click(move |_| {
      c.hit();
      panic!("handler failure");
    }),
    Button::new(square()).on_click(move |_| o.hit()),
  );
  let logs = capture_logs(|| {
    window.pointer_motion(center(&window, &[0, 0]));
    click(&window, PointerButton::Left);
    click(&window, PointerButton::Left);
    window.pointer_motion(center(&window, &[0, 1]));
    click(&window, PointerButton::Left);
  });
  assert_eq!((calls.get(), others.get()), (1, 1));
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[0].on_click panicked and is disabled"),
    "{logs}"
  );
  assert!(logs.contains("handler failure"), "{logs}");
}

#[test]
fn a_panicking_middle_click_handler_is_logged_with_its_name() {
  let mut h = Harness::new();
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_middle_click(|_| panic!("middle failure")),
    Button::new(square()),
  );
  let logs = capture_logs(|| {
    window.pointer_motion(center(&window, &[0, 0]));
    click(&window, PointerButton::Middle);
  });
  assert!(
    logs
      .contains("handler PanelWindow[DP-1]/Row/Button[0].on_middle_click panicked and is disabled"),
    "{logs}"
  );
}

#[test]
fn a_button_that_moves_away_from_a_still_pointer_loses_hover() {
  let mut h = Harness::new();
  let shift = h.cx.signal(0);
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("x").size(move || shift.get()))
    .child(Button::new(square()).style(NORMAL).hover_style(HOVER));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  assert_eq!(background(&window, &[0, 1]), HOVER.background);

  shift.set(50);
  h.frame(&window);
  assert_eq!(window.cursor(), CursorShape::Default);
  assert_eq!(background(&window, &[0, 1]), NORMAL.background);
}

#[test]
fn a_removed_button_under_the_pointer_is_forgotten() {
  let mut h = Harness::new();
  let shown = h.cx.signal(vec![1]);
  let clicks = Counter::default();
  let counted = clicks.clone();
  let row = Row::new().align(Align::Start).children_keyed(
    move || shown.get(),
    |n| *n,
    move |_cx, _n| {
      let counted = counted.clone();
      Button::new(square()).on_click(move |_| counted.hit())
    },
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  assert_eq!(window.cursor(), CursorShape::Pointer);

  shown.set(Vec::new());
  h.frame(&window);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(window.cursor(), CursorShape::Default);
  assert_eq!(clicks.get(), 0);
}

#[test]
fn a_hover_style_that_shrinks_the_button_away_settles() {
  let mut h = Harness::new();
  let row = Row::new().align(Align::Start).child(
    Button::new(square())
      .style(Style::new().padding(0, 10))
      .hover_style(Style::new().padding(0, 0)),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  // Inside the 40-pixel button, outside the 20 pixels it shrinks to on
  // hover. Checking hover again after that layout would undo it, and so on.
  window.pointer_motion(Point::new(35.0, 10.0));
  let mut layouts = 0;
  while window.needs_layout() && layouts < 10 {
    window.layout(&mut h.text);
    layouts += 1;
  }
  assert!(
    layouts <= 2,
    "still dirty after {layouts} layouts with a still pointer"
  );
}

#[test]
fn a_handler_may_remove_its_own_button_and_flush() {
  let mut h = Harness::new();
  let items = h.cx.signal(vec![1]);
  let ran = Counter::default();
  let counted = ran.clone();
  let row = Row::new().align(Align::Start).children_keyed(
    move || items.get(),
    |n| *n,
    move |_cx, _n| {
      let counted = counted.clone();
      Button::new(square()).on_click(move |_| {
        items.set(Vec::new());
        crabshell_reactive::Runtime::current().flush();
        counted.hit();
      })
    },
  );
  let before = h.runtime.node_count();
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  click(&window, PointerButton::Left);
  h.frame(&window);
  assert_eq!(ran.get(), 1);
  assert_eq!(
    super::node_at(window.view().root(), &[0])
      .children()
      .count(),
    0
  );
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn a_handler_may_dispose_its_windows_owner() {
  let mut h = Harness::new();
  let window_cx = h.cx.child();
  let before = h.runtime.node_count();
  let owner = window_cx.owner();
  let row = Row::new()
    .align(Align::Start)
    .child(Button::new(square()).on_click(move |_| owner.dispose()));
  let window = crate::Window::new(&window_cx, h.panel().child(row));
  window.set_size(crate::Size::new(200.0, 30.0));
  h.frame(&window);
  window.pointer_motion(Point::new(10.0, 10.0));
  click(&window, PointerButton::Left);
  h.frame(&window);
  // The button is gone from the runtime; another click finds nothing.
  window.pointer_motion(Point::new(11.0, 10.0));
  click(&window, PointerButton::Left);
  drop(window);
  assert!(h.runtime.node_count() < before);
}

#[test]
fn sibling_buttons_have_contexts_of_their_own() {
  let mut h = Harness::new();
  let owners = Rc::new(RefCell::new(Vec::new()));
  let (first, second) = (Rc::clone(&owners), Rc::clone(&owners));
  let window = two_buttons(
    &mut h,
    Button::new(square()).on_click(move |cx| first.borrow_mut().push(cx.owner())),
    Button::new(square()).on_click(move |cx| second.borrow_mut().push(cx.owner())),
  );
  window.pointer_motion(center(&window, &[0, 0]));
  click(&window, PointerButton::Left);
  window.pointer_motion(center(&window, &[0, 1]));
  click(&window, PointerButton::Left);
  let owners = owners.borrow();
  assert_eq!(owners.len(), 2);
  assert_ne!(owners[0], owners[1]);
}

#[test]
fn a_clip_hides_its_children_from_the_pointer_under_its_border() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let c = clicks.clone();
  // A button filling a clipping row whose 4-pixel border covers its
  // edge.
  let row = Row::new()
    .align(Align::Start)
    .style(Style::new().border(4.0, Color::rgb(9, 9, 9)).clip(true))
    .child(Button::new(Icon::new("x").size(30)).on_click(move |_| c.hit()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 2.0));
  click(&window, PointerButton::Left);
  assert_eq!(clicks.get(), 0, "a click on the border");
  window.pointer_motion(Point::new(10.0, 10.0));
  click(&window, PointerButton::Left);
  assert_eq!(clicks.get(), 1);
}

#[test]
fn hovered_is_true_while_the_pointer_is_over_the_button_or_inside_it() {
  let mut h = Harness::new();
  let (outer, inner) = (h.cx.signal(false), h.cx.signal(false));
  let row = Row::new().align(Align::Start).child(
    Button::new(
      Row::new()
        .child(square())
        .child(Button::new(square()).hovered(inner)),
    )
    .hovered(outer),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!((outer.get(), inner.get()), (false, false));

  window.pointer_motion(center(&window, &[0, 0, 0, 0]));
  assert_eq!((outer.get(), inner.get()), (true, false));
  // Over the inner button, the outer one still has the pointer inside.
  window.pointer_motion(center(&window, &[0, 0, 0, 1]));
  assert_eq!((outer.get(), inner.get()), (true, true));
  window.pointer_motion(center(&window, &[0, 0, 0, 0]));
  assert_eq!((outer.get(), inner.get()), (true, false));
  window.pointer_motion(Point::new(100.0, 25.0));
  assert_eq!((outer.get(), inner.get()), (false, false));

  window.pointer_motion(center(&window, &[0, 0, 0, 1]));
  window.pointer_leave();
  assert_eq!((outer.get(), inner.get()), (false, false));
}

#[test]
fn hovered_changes_only_when_the_pointer_crosses_the_edge() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let runs = Counter::default();
  let counted = runs.clone();
  h.cx.effect(move || {
    hovered.get();
    counted.hit();
  });
  let window = two_buttons(
    &mut h,
    Button::new(square()).hovered(hovered),
    Button::new(square()),
  );
  let before = runs.get();
  window.pointer_motion(Point::new(5.0, 5.0));
  h.frame(&window);
  window.pointer_motion(Point::new(6.0, 6.0));
  h.frame(&window);
  assert_eq!(runs.get(), before + 1);
}

#[test]
fn a_button_that_moves_away_from_a_still_pointer_is_no_longer_hovered() {
  let mut h = Harness::new();
  let (shift, hovered) = (h.cx.signal(0), h.cx.signal(false));
  let row = Row::new()
    .align(Align::Start)
    .child(Icon::new("x").size(move || shift.get()))
    .child(Button::new(square()).hovered(hovered));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  assert!(hovered.get());

  shift.set(50);
  h.frame(&window);
  assert!(!hovered.get());
}

#[test]
fn a_hovered_button_that_is_removed_is_no_longer_hovered() {
  let mut h = Harness::new();
  let (shown, hovered) = (h.cx.signal(true), h.cx.signal(false));
  let row = Row::new().align(Align::Start).children_keyed(
    move || if shown.get() { vec![()] } else { Vec::new() },
    |_| (),
    move |_cx, _| Button::new(square()).hovered(hovered),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(10.0, 10.0));
  assert!(hovered.get());

  shown.set(false);
  h.frame(&window);
  assert!(!hovered.get());
}

#[test]
fn a_hovered_buttons_window_that_goes_is_no_longer_hovered() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let window = two_buttons(
    &mut h,
    Button::new(square()).hovered(hovered),
    Button::new(square()),
  );
  window.pointer_motion(Point::new(5.0, 5.0));
  assert!(hovered.get());
  drop(window);
  assert!(!hovered.get());
}

#[test]
fn buttons_sharing_a_hovered_signal_keep_it_true_while_any_has_the_pointer() {
  let mut h = Harness::new();
  let hovered = h.cx.signal(false);
  let runs = Counter::default();
  let counted = runs.clone();
  h.cx.effect(move || {
    hovered.get();
    counted.hit();
  });
  let row = Row::new().align(Align::Start).child(
    Button::new(
      Row::new()
        .child(square())
        .child(Button::new(square()).hovered(hovered)),
    )
    .hovered(hovered),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let before = runs.get();
  window.pointer_motion(center(&window, &[0, 0, 0, 1]));
  h.frame(&window);
  window.pointer_motion(center(&window, &[0, 0, 0, 0]));
  h.frame(&window);
  assert!(hovered.get(), "still inside the outer button");
  assert_eq!(runs.get(), before + 1, "one write, on entering");
  window.pointer_motion(Point::new(100.0, 25.0));
  assert!(!hovered.get());
}
