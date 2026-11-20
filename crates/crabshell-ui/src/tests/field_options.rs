//! A field's `max_length` and `read_only`.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Signal;

use super::{Harness, node_at};
use crate::{
  Clipboard, Column, Content, Key, KeyEvent, Modifiers, Preedit, Style, TextArea, TextInput, Window,
};

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn typed(text: &str) -> KeyEvent {
  KeyEvent {
    text: Some(text.to_owned()),
    ..key(Key::Character(text.chars().next().unwrap_or(' ')))
  }
}

fn ctrl(letter: &str) -> KeyEvent {
  KeyEvent {
    modifiers: Modifiers {
      ctrl: true,
      ..Modifiers::default()
    },
    ..typed(letter)
  }
}

fn shift(key_: Key) -> KeyEvent {
  KeyEvent {
    modifiers: Modifiers {
      shift: true,
      ..Modifiers::default()
    },
    ..key(key_)
  }
}

/// A window with the keyboard and one focused field, `widget`, whose
/// `on_key` records the keys that reach it.
fn window(h: &mut Harness, widget: impl crate::Widget) -> (Window, Rc<RefCell<Vec<Key>>>) {
  let seen = Rc::new(RefCell::new(Vec::new()));
  let record = Rc::clone(&seen);
  let panel = h
    .panel()
    .child(Column::new().child(widget))
    .on_key(move |_, event| {
      record.borrow_mut().push(event.key.clone());
      true
    });
  let window = h.window(panel, 300.0, 100.0);
  window.keyboard_enter();
  h.frame(&window);
  (window, seen)
}

fn input(text: Signal<String>) -> TextInput {
  TextInput::new(text)
    .focus(true)
    .style(Style::new().width(200.0))
}

/// What the field shows.
fn shown(window: &Window) -> String {
  match node_at(window.view().root(), &[0, 0]).content() {
    Content::Input(input) => input.text.content().to_owned(),
    _ => panic!("not a field"),
  }
}

#[test]
fn max_length_stops_typing_pasting_and_input_methods() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let (window, seen) = window(&mut h, input(text).max_length(5));
  for letter in ["a", "b", "c", "d", "e", "f"] {
    window.key(&typed(letter));
  }
  assert_eq!(text.get(), "abcde");
  assert!(
    seen.borrow().is_empty(),
    "a key at the limit is still the field's"
  );

  window.key(&ctrl("a"));
  window.paste("123456789");
  assert_eq!(
    text.get(),
    "12345",
    "a paste replaces the selection, cut short"
  );
  window.key(&key(Key::Backspace));
  window.paste("xyz");
  assert_eq!(text.get(), "1234x");

  window.key(&ctrl("a"));
  window.key(&typed("q"));
  window.commit_text("rstuvw");
  assert_eq!(text.get(), "qrstu");
  h.frame(&window);
  assert_eq!(shown(&window), "qrstu");

  // Characters count, and a cut never splits one of several.
  window.key(&ctrl("a"));
  window.key(&key(Key::Backspace));
  window.paste("abc\u{e9}e\u{301}");
  assert_eq!(text.get(), "abc\u{e9}", "the e and its accent don't fit");
}

#[test]
fn max_length_cuts_text_set_from_outside_and_writes_it_back() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdefgh"));
  let limit = h.cx.signal(Some(3));
  let (window, _) = window(&mut h, input(text).max_length(move || limit.get()));
  assert_eq!(shown(&window), "abc");
  assert_eq!(text.get(), "abc", "the signal holds what the field does");

  text.set(String::from("123456"));
  h.frame(&window);
  assert_eq!(shown(&window), "123");
  assert_eq!(text.get(), "123");

  limit.set(Some(2));
  h.frame(&window);
  assert_eq!(text.get(), "12");
  limit.set(None);
  h.frame(&window);
  window.paste("3456");
  assert_eq!(text.get(), "123456");
}

#[test]
fn max_length_counts_a_text_areas_line_breaks() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let area = TextArea::new(text).focus(true).max_length(3);
  let (window, _) = window(&mut h, area);
  window.key(&typed("a"));
  window.key(&key(Key::Enter));
  window.key(&typed("b"));
  window.key(&key(Key::Enter));
  assert_eq!(text.get(), "a\nb");
}

#[test]
fn read_only_refuses_edits_and_passes_their_keys_on() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("hello"));
  let accepted = Rc::new(RefCell::new(Vec::new()));
  let record = Rc::clone(&accepted);
  let field = input(text)
    .read_only(true)
    .on_accept(move |_, text| record.borrow_mut().push(text.to_owned()));
  let (window, seen) = window(&mut h, field);
  for event in [typed("x"), key(Key::Backspace), key(Key::Delete), ctrl("v")] {
    assert_eq!(window.key(&event), None);
  }
  window.key(&ctrl("a"));
  assert_eq!(
    window.key(&ctrl("x")),
    None,
    "a cut neither copies nor cuts"
  );
  window.paste("pasted");
  window.commit_text("committed");
  window.delete_around(1, 1);
  window.set_preedit(Some(Preedit {
    text: String::from("ni"),
    cursor: None,
  }));
  h.frame(&window);
  assert_eq!(text.get(), "hello");
  assert_eq!(shown(&window), "hello");
  assert_eq!(
    *seen.borrow(),
    [
      Key::Character('x'),
      Key::Backspace,
      Key::Delete,
      Key::Character('v'),
      Key::Character('x'),
    ]
  );
  assert_eq!(window.field(), None, "input methods don't see it");

  window.key(&key(Key::Enter));
  assert_eq!(*accepted.borrow(), ["hello"], "Enter still accepts");

  // Text set from outside still shows.
  text.set(String::from("set"));
  h.frame(&window);
  assert_eq!(shown(&window), "set");
}

#[test]
fn read_only_still_moves_selects_and_copies() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("hello"));
  let on = h.cx.signal(true);
  let (window, seen) = window(&mut h, input(text).read_only(move || on.get()));
  window.key(&key(Key::Home));
  window.key(&shift(Key::Right));
  window.key(&shift(Key::Right));
  assert_eq!(
    window.key(&ctrl("c")),
    Some(Clipboard::Copy(String::from("he")))
  );
  window.key(&ctrl("a"));
  assert_eq!(
    window.key(&ctrl("c")),
    Some(Clipboard::Copy(String::from("hello")))
  );
  assert!(seen.borrow().is_empty());

  on.set(false);
  h.frame(&window);
  window.key(&key(Key::End));
  window.key(&typed("!"));
  assert_eq!(text.get(), "hello!", "editable again");
}

#[test]
fn a_read_only_password_still_never_copies() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("secret"));
  let (window, _) = window(&mut h, input(text).password(true).read_only(true));
  window.key(&ctrl("a"));
  assert_eq!(window.key(&ctrl("c")), None);
  assert_eq!(text.get(), "secret");
}

#[test]
fn a_read_only_text_area_passes_enter_and_tab_on() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("note"));
  let area = TextArea::new(text).focus(true).read_only(true);
  let (window, seen) = window(&mut h, area);
  window.key(&key(Key::Enter));
  window.key(&key(Key::Tab));
  window.key(&typed("x"));
  assert_eq!(text.get(), "note");
  assert_eq!(*seen.borrow(), [Key::Enter, Key::Tab, Key::Character('x')]);
}
