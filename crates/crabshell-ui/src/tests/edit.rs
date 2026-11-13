//! Editing one line of text: typing, deleting, moving and selecting.

use crate::edit::{Edit, Keyed, grapheme_start, graphemes_before};
use crate::{Key, KeyEvent, Modifiers};

fn key(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn with(event: KeyEvent, ctrl: bool, shift: bool) -> KeyEvent {
  KeyEvent {
    modifiers: Modifiers {
      ctrl,
      shift,
      ..Modifiers::default()
    },
    ..event
  }
}

fn typed(text: &str) -> KeyEvent {
  KeyEvent {
    text: Some(text.to_owned()),
    ..key(Key::Character(text.chars().next().unwrap_or(' ')))
  }
}

fn at(text: &str, cursor: usize) -> Edit {
  Edit {
    text: text.to_owned(),
    cursor,
    anchor: None,
  }
}

#[test]
fn typing_inserts_at_the_caret_and_replaces_a_selection() {
  let mut edit = at("fir", 3);
  assert_eq!(edit.key(&typed("e")), Keyed::Edited);
  assert_eq!((edit.text.as_str(), edit.cursor), ("fire", 4));
  edit.cursor = 0;
  edit.key(&typed("A"));
  assert_eq!(edit.text, "Afire");
  edit.anchor = Some(1);
  edit.cursor = 5;
  edit.key(&typed("x"));
  assert_eq!((edit.text.as_str(), edit.cursor), ("Ax", 2));
  // Ctrl and Alt combinations type nothing, even with text.
  assert_eq!(edit.key(&with(typed("c"), true, false)), Keyed::Unused);
  assert_eq!(edit.text, "Ax");
}

#[test]
fn backspace_and_delete_take_a_whole_grapheme_or_word() {
  // "é" as e and a combining accent, then a flag of two code points.
  let mut edit = at("cafe\u{301} 🇫🇷", "cafe\u{301} 🇫🇷".len());
  edit.key(&key(Key::Backspace));
  assert_eq!(edit.text, "cafe\u{301} ");
  edit.key(&key(Key::Backspace));
  edit.key(&key(Key::Backspace));
  assert_eq!(edit.text, "caf");
  let mut edit = at("open the door", 0);
  edit.key(&with(key(Key::Delete), true, false));
  assert_eq!(edit.text, " the door");
  edit.cursor = edit.text.len();
  edit.key(&with(key(Key::Backspace), true, false));
  assert_eq!(edit.text, " the ");
  // A word back skips the spaces after it.
  let mut edit = at("the door  ", 10);
  edit.key(&with(key(Key::Backspace), true, false));
  assert_eq!(edit.text, "the ");
  // At the ends, nothing goes, but the key is still the field's.
  let mut edit = at("x", 0);
  assert_eq!(edit.key(&key(Key::Backspace)), Keyed::Moved);
  assert_eq!(edit.text, "x");
}

#[test]
fn arrows_move_by_grapheme_or_word_and_shift_selects() {
  let mut edit = at("ab cd", 0);
  edit.key(&key(Key::Right));
  assert_eq!(edit.cursor, 1);
  edit.key(&with(key(Key::Right), true, false));
  assert_eq!(edit.cursor, 2, "to the end of the word");
  edit.key(&with(key(Key::Right), true, false));
  assert_eq!(edit.cursor, 5);
  edit.key(&with(key(Key::Left), true, false));
  assert_eq!(edit.cursor, 3);
  edit.key(&with(key(Key::Home), false, true));
  assert_eq!(edit.selection(), Some(0..3));
  // An arrow without Shift collapses the selection to its side.
  edit.key(&key(Key::Right));
  assert_eq!((edit.cursor, edit.selection()), (3, None));
  edit.key(&with(key(Key::End), false, true));
  edit.key(&key(Key::Left));
  assert_eq!((edit.cursor, edit.selection()), (3, None));
}

#[test]
fn ctrl_a_selects_everything_and_other_keys_are_left_alone() {
  let mut edit = at("all of it", 2);
  edit.key(&with(typed("a"), true, false));
  assert_eq!(edit.selection(), Some(0..9));
  assert_eq!(edit.key(&key(Key::Enter)), Keyed::Accept);
  for unused in [Key::Up, Key::Down, Key::Escape, Key::Tab, Key::PageDown] {
    assert_eq!(edit.key(&key(unused)), Keyed::Unused);
  }
}

#[test]
fn text_from_outside_puts_the_caret_at_its_end() {
  let mut edit = at("hello", 1);
  edit.anchor = Some(0);
  edit.set_text(String::from("e\u{301}x"));
  assert_eq!((edit.cursor, edit.anchor), (4, None));
  // The same text, as typing writes back, leaves the caret alone.
  edit.cursor = 0;
  edit.set_text(String::from("e\u{301}x"));
  assert_eq!(edit.cursor, 0);
  edit.set_text(String::new());
  assert_eq!(edit.cursor, 0);
}

#[test]
fn grapheme_counts_map_between_text_and_its_password_dots() {
  let text = "e\u{301}🇫🇷a";
  assert_eq!(graphemes_before(text, text.len()), 3);
  assert_eq!(grapheme_start(text, 1), "e\u{301}".len());
  assert_eq!(grapheme_start(text, 9), text.len());
}
