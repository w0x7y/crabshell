//! The primary selection: a selection made with the pointer or the keys
//! is offered, and a middle click in a field asks to paste it there.

use crabshell_reactive::Signal;

use super::Harness;
use crate::{
  Clipboard, Key, KeyEvent, Modifiers, Point, PointerButton, Row, Style, Text, TextInput, Window,
};

/// A 200 by 30 window with the keyboard and `child` in a row.
fn window_with(h: &mut Harness, child: impl crate::Widget) -> Window {
  let window = h.window(h.panel().child(Row::new().child(child)), 200.0, 30.0);
  window.keyboard_enter();
  window
}

/// A 100-pixel field holding `text`.
fn field(text: Signal<String>) -> TextInput {
  TextInput::new(text).style(Style::new().width(100.0))
}

/// Drags the left button from `from` to `to`, 10 down.
fn drag(window: &Window, from: f32, to: f32, time: u32) {
  window.pointer_motion_at(Point::new(from, 10.0), time);
  window.pointer_button_at(PointerButton::Left, true, time);
  window.pointer_motion_at(Point::new(to, 10.0), time + 10);
  window.pointer_button_at(PointerButton::Left, false, time + 20);
}

fn key(key: Key, shift: bool) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers {
      shift,
      ..Modifiers::default()
    },
    repeat: false,
  }
}

#[test]
fn a_drag_selection_in_a_field_becomes_the_primary_selection() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = window_with(&mut h, field(text));
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Left, true, 0);
  window.pointer_motion_at(Point::new(190.0, 10.0), 10);
  assert_eq!(window.take_primary(), None, "only the release offers it");
  window.pointer_button_at(PointerButton::Left, false, 20);
  assert_eq!(
    window.take_primary(),
    Some(Clipboard::Copy(String::from("abcdef")))
  );
  assert_eq!(window.take_primary(), None, "taking it clears it");
  // A plain click selects nothing, so offers nothing.
  drag(&window, 50.0, 50.0, 1000);
  assert_eq!(window.take_primary(), None);
}

#[test]
fn a_selection_made_with_the_keys_becomes_the_primary_selection() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = window_with(&mut h, field(text).focus(true));
  h.frame(&window);
  window.key(&key(Key::Home, false));
  assert_eq!(window.take_primary(), None, "a move selects nothing");
  window.key(&key(Key::Right, true));
  window.key(&key(Key::Right, true));
  assert_eq!(
    window.take_primary(),
    Some(Clipboard::Copy(String::from("ab")))
  );
  window.key(&key(Key::Right, false));
  assert_eq!(window.take_primary(), None, "dropping it offers nothing");
}

#[test]
fn a_password_never_becomes_the_primary_selection() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("secret"));
  let window = window_with(&mut h, field(text).focus(true).password(true));
  h.frame(&window);
  drag(&window, 1.0, 190.0, 0);
  assert_eq!(window.take_primary(), None);
  window.key(&key(Key::End, false));
  window.key(&key(Key::Left, true));
  assert_eq!(window.take_primary(), None);
}

#[test]
fn a_selectable_text_dragged_becomes_the_primary_selection() {
  let mut h = Harness::new();
  let window = window_with(&mut h, Text::new("hello world").selectable(true));
  drag(&window, 1.0, 190.0, 0);
  assert_eq!(
    window.take_primary(),
    Some(Clipboard::Copy(String::from("hello world")))
  );
}

#[test]
fn a_middle_click_in_a_field_puts_the_caret_there_and_asks_to_paste() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = window_with(&mut h, field(text));
  h.frame(&window);
  assert_eq!(window.field(), None, "nothing has the focus yet");
  window.pointer_motion_at(Point::new(1.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Middle, true, 0);
  window.pointer_button_at(PointerButton::Middle, false, 10);
  assert_eq!(window.take_primary(), Some(Clipboard::Paste));
  let state = window.field().expect("the field took the focus");
  assert_eq!((state.anchor, state.cursor), (0, 0));
  window.paste("X");
  assert_eq!(text.get(), "Xabcdef");
}

#[test]
fn a_middle_click_outside_a_field_asks_for_nothing() {
  let mut h = Harness::new();
  let window = window_with(&mut h, Text::new("hello").selectable(true));
  window.pointer_motion_at(Point::new(5.0, 10.0), 0);
  window.pointer_button_at(PointerButton::Middle, true, 0);
  window.pointer_button_at(PointerButton::Middle, false, 10);
  assert_eq!(window.take_primary(), None);
}
