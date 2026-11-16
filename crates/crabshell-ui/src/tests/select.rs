//! Selecting text with the pointer: dragging, double clicks and
//! Shift+click.

use crabshell_reactive::Signal;

use super::{Harness, node_at};
use crate::{
  Clipboard, Column, Icon, Key, KeyEvent, Modifiers, Point, PointerButton, Row, ScrollView, Style,
  TextInput, Window,
};

/// Ctrl+C, and what it copied.
fn copied(window: &Window) -> Option<String> {
  let ctrl_c = KeyEvent {
    key: Key::Character('c'),
    text: None,
    modifiers: Modifiers {
      ctrl: true,
      ..Modifiers::default()
    },
    repeat: false,
  };
  match window.key(&ctrl_c) {
    Some(Clipboard::Copy(text)) => Some(text),
    _ => None,
  }
}

/// A 200 by 30 window with the keyboard and one 100-pixel field.
fn field(h: &mut Harness, text: Signal<String>) -> Window {
  let row = Row::new().child(TextInput::new(text).style(Style::new().width(100.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window
}

/// A left click at `x`, 10 down, going down at `time` and up 10 ms later.
fn click(window: &Window, x: f32, time: u32) {
  window.pointer_motion_at(Point::new(x, 10.0), time);
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_button_at(PointerButton::Left, false, time + 10);
}

#[test]
fn dragging_in_a_field_selects_to_the_pointer_even_outside_it() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text);
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  assert_eq!(copied(&window), None, "a press only places the caret");
  // Past the field's right end, outside it.
  window.pointer_motion_at(Point::new(190.0, 25.0), 20);
  h.frame(&window);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
  // Back to the start: the selection follows the pointer.
  window.pointer_motion_at(Point::new(1.0, 10.0), 40);
  assert_eq!(copied(&window), None);
  window.pointer_motion_at(Point::new(190.0, 10.0), 60);
  window.pointer_button_at(PointerButton::Left, false, 80);
  // The release keeps the selection, and moving after it changes nothing.
  window.pointer_motion_at(Point::new(1.0, 10.0), 100);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
}

#[test]
fn dragging_a_long_field_scrolls_its_text() {
  let mut h = Harness::new();
  let text = h.cx.signal("w".repeat(60));
  let window = field(&mut h, text);
  // The caret starts at the end, so the field shows the end first.
  click(&window, 1.0, 0);
  window.key(&KeyEvent {
    key: Key::Home,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  });
  h.frame(&window);
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 1000);
  // Each move past the end selects further and the layout scrolls the
  // text after it, as the caret goes.
  for step in 1..40 {
    window.pointer_motion_at(Point::new(150.0, 10.0), 1000 + step * 10);
    h.frame(&window);
  }
  window.pointer_button_at(PointerButton::Left, false, 1500);
  assert_eq!(copied(&window), Some("w".repeat(60)));
}

#[test]
fn a_double_click_selects_the_word_under_the_pointer() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("one two"));
  let window = field(&mut h, text);
  // Find a place in "two".
  let x = (1..100)
    .map(|x| x as f32)
    .find(|&x| {
      click(&window, x, 10_000 * x as u32);
      window.field().is_some_and(|field| field.cursor == 5)
    })
    .expect("a place inside the second word");
  click(&window, x, 0);
  click(&window, x, 200);
  assert_eq!(copied(&window).as_deref(), Some("two"));
  // Too slow for a double click: the second click places the caret.
  click(&window, x, 1000);
  click(&window, x, 1600);
  assert_eq!(copied(&window), None);
}

#[test]
fn shift_click_extends_the_selection() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text);
  click(&window, 1.0, 0);
  window.set_modifiers(Modifiers {
    shift: true,
    ..Modifiers::default()
  });
  click(&window, 99.0, 1000);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
  window.set_modifiers(Modifiers::default());
  click(&window, 99.0, 2000);
  assert_eq!(copied(&window), None, "a plain click drops it");
}

#[test]
fn dragging_a_field_in_a_scroll_view_selects_instead_of_scrolling() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let offset = h.cx.signal(0.0_f32);
  let content = Column::new()
    .child(TextInput::new(text).style(Style::new().width(100.0)))
    .child(Icon::new("x").size(200));
  let view = ScrollView::new(content).offset(offset);
  let window = h.window(h.panel().child(view), 200.0, 30.0);
  window.keyboard_enter();
  let field = node_at(window.view().root(), &[0, 0, 0]).rect();
  window.pointer_motion_at(Point::new(field.x + 1.0, field.y + 5.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(field.x + 1.0, field.y + 25.0), 50);
  window.pointer_motion_at(Point::new(150.0, 29.0), 100);
  window.pointer_button_at(PointerButton::Left, false, 150);
  h.frame(&window);
  assert_eq!(offset.get(), 0.0);
  assert_eq!(copied(&window).as_deref(), Some("abcdef"));
}
