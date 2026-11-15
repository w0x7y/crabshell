//! `visible`: a hidden widget frees its space, isn't drawn, hit or reached
//! by Tab, lets go of the focus and the pointer, and keeps its state.

use super::{Counter, Harness, capture_logs, node_at, paths, rect_at};
use crate::{
  Align, Button, Color, Column, Grid, Icon, Image, Key, KeyEvent, ListView, Menu, Modifiers, Point,
  PointerButton, Rect, Row, ScreencopyView, ScrollView, Spacer, Stack, Style, Text, TextInput,
};

const NORMAL: Style = Style::new().background(Color::rgb(1, 1, 1));
const HOVER: Style = Style::new().background(Color::rgb(2, 2, 2));

fn tab() -> KeyEvent {
  KeyEvent {
    key: Key::Tab,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn square(name: &str) -> Icon {
  Icon::new(name).size(10)
}

#[test]
fn a_hidden_widget_frees_its_space_and_takes_it_again_when_shown() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let row = Row::new()
    .gap(2)
    .child(square("a").visible(move || shown.get()))
    .child(square("b"));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 12.0);

  shown.set(false);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]), Rect::new(0.0, 0.0, 10.0, 30.0));
  assert_eq!(paths(&window).len(), 3, "the hidden icon isn't drawn");

  shown.set(true);
  h.frame(&window);
  assert_eq!(rect_at(&window, &[0, 0]).x, 0.0);
  assert_eq!(rect_at(&window, &[0, 1]).x, 12.0);
}

#[test]
fn hiding_repaints_where_it_was_and_what_moved_and_showing_where_it_is() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let row = Row::new()
    .align(Align::Start)
    .child(square("a").visible(move || shown.get()))
    .child(Stack::new().style(NORMAL.width(10.0).height(30.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.take_damage();

  shown.set(false);
  h.frame(&window);
  let damage = window.take_damage();
  // The icon where it was, and the box at both places.
  assert!(
    damage.contains(&Rect::new(0.0, 0.0, 10.0, 10.0)),
    "{damage:?}"
  );
  assert!(
    damage.contains(&Rect::new(10.0, 0.0, 10.0, 30.0)),
    "{damage:?}"
  );
  assert!(
    damage.contains(&Rect::new(0.0, 0.0, 10.0, 30.0)),
    "{damage:?}"
  );

  // Nothing moves while it stays hidden.
  window.layout(&mut h.text);
  assert!(!window.has_damage());

  shown.set(true);
  h.frame(&window);
  let damage = window.take_damage();
  assert!(
    damage.contains(&Rect::new(0.0, 0.0, 10.0, 10.0)),
    "{damage:?}"
  );
  assert!(
    damage.contains(&Rect::new(10.0, 0.0, 10.0, 30.0)),
    "{damage:?}"
  );
}

#[test]
fn a_hidden_button_isnt_hit_and_the_one_under_it_is() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let (top, below) = (Counter::default(), Counter::default());
  let (t, b) = (top.clone(), below.clone());
  let stack = Stack::new()
    .child(Button::new(square("below")).on_click(move |_| b.hit()))
    .child(
      Button::new(square("top"))
        .visible(move || shown.get())
        .on_click(move |_| t.hit()),
    );
  let window = h.window(h.panel().child(stack), 200.0, 30.0);
  shown.set(false);
  h.frame(&window);
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  h.runtime.flush();
  assert_eq!((top.get(), below.get()), (0, 1));
}

#[test]
fn tab_skips_a_hidden_button_and_one_that_hides_drops_the_focus() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let row = Row::new()
    .align(Align::Start)
    .child(Button::new(square("a")))
    .child(Button::new(square("b")).visible(move || shown.get()))
    .child(Button::new(square("c")));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window.key(&tab());
  window.key(&tab());
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[1]")
  );

  shown.set(false);
  h.frame(&window);
  assert_eq!(window.focused(), None);
  window.key(&tab());
  window.key(&tab());
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[2]")
  );
}

#[test]
fn a_hovered_button_that_hides_unhovers() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let hovered = h.cx.signal(false);
  let row = Row::new().align(Align::Start).child(
    Button::new(square("a"))
      .style(NORMAL)
      .hover_style(HOVER)
      .hovered(hovered)
      .visible(move || shown.get()),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(5.0, 5.0));
  assert!(hovered.get_untracked());

  shown.set(false);
  h.frame(&window);
  assert!(!hovered.get_untracked());
  assert_eq!(window.cursor(), crate::CursorShape::Default);

  // Shown again away from the pointer, it isn't hovered.
  window.pointer_motion(Point::new(150.0, 25.0));
  shown.set(true);
  h.frame(&window);
  {
    let view = window.view();
    assert_eq!(
      node_at(view.root(), &[0, 0]).style().background,
      NORMAL.background
    );
  }

  // Shown again under a still pointer, the layout finds it hovered.
  shown.set(false);
  h.frame(&window);
  window.pointer_motion(Point::new(5.0, 5.0));
  shown.set(true);
  h.frame(&window);
  assert!(hovered.get_untracked());
  let view = window.view();
  assert_eq!(
    node_at(view.root(), &[0, 0]).style().background,
    HOVER.background
  );
}

#[test]
fn a_button_hidden_while_held_isnt_clicked() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let clicks = Counter::default();
  let counter = clicks.clone();
  let row = Row::new().align(Align::Start).child(
    Button::new(square("a"))
      .visible(move || shown.get())
      .on_click(move |_| counter.hit()),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.pointer_motion(Point::new(5.0, 5.0));
  window.pointer_button(PointerButton::Left, true);
  shown.set(false);
  h.frame(&window);
  shown.set(true);
  h.frame(&window);
  window.pointer_button(PointerButton::Left, false);
  h.runtime.flush();
  assert_eq!(clicks.get(), 0);
}

#[test]
fn a_hidden_widget_keeps_its_bindings_and_state() {
  let mut h = Harness::new();
  let shown = h.cx.signal(true);
  let label = h.cx.signal(String::from("before"));
  let typed = h.cx.signal(String::from("kept"));
  let column = Column::new()
    .visible(move || shown.get())
    .child(Text::new(move || label.get()))
    .child(TextInput::new(typed));
  let window = h.window(h.panel().child(column), 200.0, 60.0);
  let count = h.runtime.node_count();

  shown.set(false);
  h.frame(&window);
  label.set(String::from("after"));
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), count);

  shown.set(true);
  h.frame(&window);
  let view = window.view();
  let crate::Content::Text(text) = node_at(view.root(), &[0, 0]).content() else {
    panic!("the text shows again");
  };
  assert_eq!(text.content(), "after");
  assert_eq!(typed.get_untracked(), "kept");
}

#[test]
fn a_hidden_child_is_freed_with_its_list_item() {
  let mut h = Harness::new();
  let items = h.cx.signal(Vec::<u32>::new());
  let row = Row::new().children_keyed(
    move || items.get(),
    |n| *n,
    |cx, n| {
      let shown = cx.signal(false);
      Stack::new()
        .visible(move || shown.get())
        .child(Text::new(move || n.get().to_string()))
    },
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let empty = h.runtime.node_count();
  items.set(vec![1, 2]);
  h.frame(&window);
  items.set(Vec::new());
  h.frame(&window);
  assert_eq!(h.runtime.node_count(), empty);
}

#[test]
fn every_widget_can_be_hidden() {
  let mut h = Harness::new();
  let typed = h.cx.signal(String::new());
  let row = Row::new()
    .child(Text::new("a").visible(false))
    .child(square("b").visible(false))
    .child(Image::new("/nowhere.png").visible(false))
    .child(Button::new(Text::new("c")).visible(false))
    .child(ScrollView::new(Text::new("d")).visible(false))
    .child(ListView::new(|| vec![1], |n| *n, |_, _| Text::new("e")).visible(false))
    .child(TextInput::new(typed).visible(false))
    .child(ScreencopyView::new(crate::CaptureSource::Output("DP-1".into())).visible(false))
    .child(Menu::new(Vec::new()).visible(false))
    .child(Grid::new().visible(false))
    .child(Spacer::new().visible(false))
    .child(Stack::new().visible(false))
    .child(Column::new().visible(false))
    .child(Row::new().visible(false));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(
    paths(&window),
    ["PanelWindow[DP-1]", "PanelWindow[DP-1]/Row"]
  );
  assert!(window.captures().is_empty());
}

#[test]
fn a_panicking_visible_binding_is_logged_with_its_path_and_the_rest_runs_on() {
  let mut h = Harness::new();
  let fail = h.cx.signal(false);
  let label = h.cx.signal(String::from("fine"));
  let row = Row::new()
    .child(square("a").visible(move || {
      assert!(!fail.get(), "no visibility");
      true
    }))
    .child(Text::new(move || label.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    fail.set(true);
    h.frame(&window);
  });
  assert!(
    logs.contains("effect PanelWindow[DP-1]/Row/Icon[0].visible panicked and is disabled"),
    "{logs}"
  );
  label.set(String::from("still fine"));
  h.frame(&window);
  let view = window.view();
  let crate::Content::Text(text) = node_at(view.root(), &[0, 1]).content() else {
    panic!("the text is still there");
  };
  assert_eq!(text.content(), "still fine");
}

#[test]
fn a_hidden_field_takes_the_focus_only_once_shown() {
  let mut h = Harness::new();
  let open = h.cx.signal(false);
  let text = h.cx.signal(String::new());
  let column = Column::new()
    .visible(move || open.get())
    .child(TextInput::new(text).focus(true));
  let window = h.window(h.panel().child(column), 200.0, 60.0);
  window.keyboard_enter();
  assert_eq!(
    window.focused(),
    None,
    "a hidden field can't take the focus"
  );
  assert!(window.field().is_none(), "nor reach an input method");

  // Each time it shows, its `focus`, still true, takes the focus.
  for _ in 0..2 {
    open.set(true);
    h.frame(&window);
    assert_eq!(
      window.focused().as_deref(),
      Some("PanelWindow[DP-1]/Column/TextInput[0]")
    );
    open.set(false);
    h.frame(&window);
    assert_eq!(window.focused(), None);
  }
}

#[test]
fn a_field_whose_focus_turned_false_while_hidden_stays_unfocused_when_shown() {
  let mut h = Harness::new();
  let open = h.cx.signal(false);
  let wants = h.cx.signal(true);
  let text = h.cx.signal(String::new());
  let column = Column::new()
    .visible(move || open.get())
    .child(TextInput::new(text).focus(move || wants.get()));
  let window = h.window(h.panel().child(column), 200.0, 60.0);
  wants.set(false);
  open.set(true);
  h.frame(&window);
  assert_eq!(window.focused(), None);
}

#[test]
fn a_field_still_hidden_inside_what_shows_leaves_the_focus_to_the_next() {
  let mut h = Harness::new();
  let open = h.cx.signal(false);
  let (first, second) = (h.cx.signal(String::new()), h.cx.signal(String::new()));
  let column = Column::new()
    .visible(move || open.get())
    .child(TextInput::new(first).focus(true).visible(false))
    .child(TextInput::new(second).focus(true));
  let window = h.window(h.panel().child(column), 200.0, 60.0);
  open.set(true);
  h.frame(&window);
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Column/TextInput[1]")
  );
}
