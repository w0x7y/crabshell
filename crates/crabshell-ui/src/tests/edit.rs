//! Editing one line of text: typing, deleting, moving and selecting.

use crate::edit::{Edit, Keyed, grapheme_start, graphemes_before, word_at};
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
    multiline: false,
  }
}

#[test]
fn typing_inserts_at_the_caret_and_replaces_a_selection() {
  let mut edit = at("fir", 3);
  assert_eq!(edit.key(&typed("e"), true), Keyed::Edited);
  assert_eq!((edit.text.as_str(), edit.cursor), ("fire", 4));
  edit.cursor = 0;
  edit.key(&typed("A"), true);
  assert_eq!(edit.text, "Afire");
  edit.anchor = Some(1);
  edit.cursor = 5;
  edit.key(&typed("x"), true);
  assert_eq!((edit.text.as_str(), edit.cursor), ("Ax", 2));
  // Ctrl and Alt combinations type nothing, even with text.
  assert_eq!(
    edit.key(&with(typed("q"), true, false), true),
    Keyed::Unused
  );
  assert_eq!(edit.text, "Ax");
}

#[test]
fn backspace_and_delete_take_a_whole_grapheme_or_word() {
  // "é" as e and a combining accent, then a flag of two code points.
  let mut edit = at("cafe\u{301} 🇫🇷", "cafe\u{301} 🇫🇷".len());
  edit.key(&key(Key::Backspace), true);
  assert_eq!(edit.text, "cafe\u{301} ");
  edit.key(&key(Key::Backspace), true);
  edit.key(&key(Key::Backspace), true);
  assert_eq!(edit.text, "caf");
  let mut edit = at("open the door", 0);
  edit.key(&with(key(Key::Delete), true, false), true);
  assert_eq!(edit.text, " the door");
  edit.cursor = edit.text.len();
  edit.key(&with(key(Key::Backspace), true, false), true);
  assert_eq!(edit.text, " the ");
  // A word back skips the spaces after it.
  let mut edit = at("the door  ", 10);
  edit.key(&with(key(Key::Backspace), true, false), true);
  assert_eq!(edit.text, "the ");
  // At the ends, nothing goes, but the key is still the field's.
  let mut edit = at("x", 0);
  assert_eq!(edit.key(&key(Key::Backspace), true), Keyed::Moved);
  assert_eq!(edit.text, "x");
}

#[test]
fn arrows_move_by_grapheme_or_word_and_shift_selects() {
  let mut edit = at("ab cd", 0);
  edit.key(&key(Key::Right), true);
  assert_eq!(edit.cursor, 1);
  edit.key(&with(key(Key::Right), true, false), true);
  assert_eq!(edit.cursor, 2, "to the end of the word");
  edit.key(&with(key(Key::Right), true, false), true);
  assert_eq!(edit.cursor, 5);
  edit.key(&with(key(Key::Left), true, false), true);
  assert_eq!(edit.cursor, 3);
  edit.key(&with(key(Key::Home), false, true), true);
  assert_eq!(edit.selection(), Some(0..3));
  // An arrow without Shift collapses the selection to its side.
  edit.key(&key(Key::Right), true);
  assert_eq!((edit.cursor, edit.selection()), (3, None));
  edit.key(&with(key(Key::End), false, true), true);
  edit.key(&key(Key::Left), true);
  assert_eq!((edit.cursor, edit.selection()), (3, None));
}

#[test]
fn ctrl_a_selects_everything_and_other_keys_are_left_alone() {
  let mut edit = at("all of it", 2);
  edit.key(&with(typed("a"), true, false), true);
  assert_eq!(edit.selection(), Some(0..9));
  assert_eq!(edit.key(&key(Key::Enter), true), Keyed::Accept);
  for unused in [Key::Up, Key::Down, Key::Escape, Key::Tab, Key::PageDown] {
    assert_eq!(edit.key(&key(unused), true), Keyed::Unused);
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

#[test]
fn ctrl_c_x_and_v_copy_cut_and_paste_a_selection() {
  let mut edit = at("cut me", 0);
  let ctrl = |letter| with(typed(letter), true, false);
  assert_eq!(
    edit.key(&ctrl("c"), true),
    Keyed::Unused,
    "nothing selected"
  );
  edit.key(&with(key(Key::End), false, true), true);
  assert_eq!(
    edit.key(&ctrl("c"), true),
    Keyed::Copy(String::from("cut me"))
  );
  assert_eq!(edit.text, "cut me");
  assert_eq!(
    edit.key(&ctrl("x"), true),
    Keyed::Cut(String::from("cut me"))
  );
  assert_eq!(edit.text, "");
  assert_eq!(edit.key(&ctrl("v"), true), Keyed::Paste);
  // A password copies nothing, and the keys stay the field's.
  let mut secret = at("pw", 0);
  secret.key(&with(key(Key::End), false, true), true);
  assert_eq!(secret.key(&ctrl("x"), false), Keyed::Moved);
  assert_eq!(secret.text, "pw");
}

#[test]
fn a_word_is_found_around_a_position() {
  assert_eq!(word_at("one two", 5), 4..7);
  assert_eq!(word_at("one two", 4), 4..7);
  assert_eq!(word_at("one two", 3), 3..4, "the space");
  assert_eq!(word_at("one two", 7), 4..7, "the end is the last word");
  assert_eq!(word_at("", 0), 0..0);
}

#[test]
fn many_lines_keep_line_breaks_and_enter_breaks_a_line() {
  let mut edit = Edit {
    multiline: true,
    ..Edit::default()
  };
  edit.set_text(String::from("a\r\nb\rc"));
  assert_eq!(edit.text, "a\nb\nc", "every break becomes \\n");
  edit.insert("x\r\ny");
  assert_eq!(edit.text, "a\nb\ncx\ny");
  let mut edit = Edit {
    multiline: true,
    ..at("ab", 1)
  };
  assert_eq!(edit.key(&key(Key::Enter), true), Keyed::Edited);
  assert_eq!((edit.text.as_str(), edit.cursor), ("a\nb", 2));
  let ctrl_enter = KeyEvent {
    modifiers: Modifiers {
      ctrl: true,
      ..Modifiers::default()
    },
    ..key(Key::Enter)
  };
  assert_eq!(edit.key(&ctrl_enter, true), Keyed::Accept);
  // One line: a break typed in becomes a space, and Enter accepts.
  let mut edit = at("ab", 1);
  edit.insert("\r\n");
  assert_eq!(edit.text, "a  b");
  assert_eq!(edit.key(&key(Key::Enter), true), Keyed::Accept);
}
