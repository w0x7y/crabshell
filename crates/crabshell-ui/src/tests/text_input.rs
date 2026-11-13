//! Text fields: focus, typing into the shared signal, the caret, clicks,
//! placeholders, passwords and Enter.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::Signal;

use super::{Harness, capture_logs, node_at};
use crate::{
  Content, Key, KeyEvent, Modifiers, Point, PointerButton, Row, Style, TextInput, Window,
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

/// A window with one 100-pixel field editing `text`, with the keyboard.
fn field(
  h: &mut Harness,
  text: Signal<String>,
  input: impl FnOnce(TextInput) -> TextInput,
) -> Window {
  let row = Row::new().child(input(TextInput::new(text)).style(Style::new().width(100.0)));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window
}

/// What the field shows, its color's alpha, and its caret.
fn shown(window: &Window) -> (String, u8, Option<crate::Rect>) {
  match node_at(window.view().root(), &[0, 0]).content() {
    Content::Input(input) => (
      input.text.content().to_owned(),
      input.text.color().a,
      input.caret,
    ),
    _ => panic!("not a field"),
  }
}

#[test]
fn tab_focuses_the_field_and_typing_writes_the_signal() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let window = field(&mut h, text, |input| input);
  assert_eq!(shown(&window).2, None, "no caret before focus");
  window.key(&key(Key::Tab));
  for letter in ["f", "i", "r"] {
    window.key(&typed(letter));
  }
  assert_eq!(text.get(), "fir");
  h.frame(&window);
  let (content, _, caret) = shown(&window);
  assert_eq!(content, "fir");
  let caret = caret.expect("a caret while focused");
  window.key(&key(Key::Backspace));
  h.frame(&window);
  assert_eq!(text.get(), "fi");
  assert!(
    shown(&window).2.expect("caret").x < caret.x,
    "the caret moved back"
  );
}

#[test]
fn keys_the_field_doesnt_use_reach_the_window_and_enter_accepts() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("firefox"));
  let seen = Rc::new(RefCell::new(Vec::new()));
  let accepted = Rc::new(RefCell::new(Vec::new()));
  let (s, a) = (Rc::clone(&seen), Rc::clone(&accepted));
  let row = Row::new().child(
    TextInput::new(text)
      .focus(true)
      .on_accept(move |_, text| a.borrow_mut().push(text.to_owned())),
  );
  let panel = h.panel().child(row).on_key(move |_, event| {
    s.borrow_mut().push(event.key.clone());
    true
  });
  let window = h.window(panel, 200.0, 30.0);
  window.keyboard_enter();
  window.key(&key(Key::Down));
  window.key(&key(Key::Left));
  window.key(&key(Key::Escape));
  window.key(&key(Key::Enter));
  assert_eq!(*seen.borrow(), [Key::Down, Key::Escape]);
  assert_eq!(*accepted.borrow(), ["firefox"]);
}

#[test]
fn focus_shows_the_caret_only_while_the_window_has_the_keyboard() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("x"));
  let open = h.cx.signal(false);
  let row = Row::new().child(TextInput::new(text).focus(move || open.get()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  open.set(true);
  h.frame(&window);
  assert_eq!(
    shown(&window).2,
    None,
    "focused, but the keyboard is elsewhere"
  );
  window.keyboard_enter();
  assert!(shown(&window).2.is_some());
  window.keyboard_leave();
  assert_eq!(shown(&window).2, None);
}

#[test]
fn a_click_focuses_the_field_and_puts_the_caret_under_the_pointer() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text, |input| input);
  window.pointer_motion(Point::new(1.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  h.frame(&window);
  let start = shown(&window).2.expect("focused by the click");
  window.pointer_motion(Point::new(99.0, 10.0));
  window.pointer_button(PointerButton::Left, true);
  h.frame(&window);
  let end = shown(&window).2.expect("caret");
  assert!(end.x > start.x + 10.0, "{start:?} then {end:?}");
  // Typing goes in at the end.
  window.key(&typed("!"));
  assert_eq!(text.get(), "abcdef!");
  assert_eq!(window.cursor(), crate::CursorShape::Text);
}

#[test]
fn an_empty_field_shows_its_placeholder_dimmed_and_a_password_shows_dots() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let hidden = h.cx.signal(false);
  let window = field(&mut h, text, |input| {
    input.placeholder("Search").password(move || hidden.get())
  });
  let (content, alpha, _) = shown(&window);
  assert_eq!((content.as_str(), alpha), ("Search", 127));
  text.set(String::from("pa\u{301}ss"));
  h.frame(&window);
  assert_eq!(shown(&window).0, "pa\u{301}ss");
  hidden.set(true);
  h.frame(&window);
  assert_eq!(shown(&window).0, "••••", "a dot per grapheme");
}

#[test]
fn a_long_text_scrolls_to_keep_the_caret_in_the_field() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let window = field(&mut h, text, |input| input.focus(true));
  h.frame(&window);
  for _ in 0..60 {
    window.key(&typed("w"));
  }
  h.frame(&window);
  let caret = shown(&window).2.expect("caret");
  let field = node_at(window.view().root(), &[0, 0]).rect();
  assert!(
    caret.x >= field.x && caret.right() <= field.right(),
    "{caret:?} in {field:?}"
  );
  window.key(&key(Key::Home));
  h.frame(&window);
  assert_eq!(shown(&window).2.expect("caret").x, field.x);
}

#[test]
fn a_panicking_on_accept_is_logged_with_its_path() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let window = field(&mut h, text, |input| {
    input.focus(true).on_accept(|_, _| panic!("accept failure"))
  });
  h.frame(&window);
  let logs = capture_logs(|| window.key(&key(Key::Enter)));
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/TextInput[0].on_accept panicked and is disabled"),
    "{logs}"
  );
  // The field still edits.
  window.key(&typed("a"));
  assert_eq!(text.get(), "a");
}

#[test]
fn dropping_a_field_frees_what_it_made() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let before = h.runtime.node_count();
  let window = field(&mut h, text, |input| input.on_accept(|_, _| {}));
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn in_right_to_left_text_the_caret_starts_on_the_right() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("שלום"));
  let window = field(&mut h, text, |input| input.focus(true));
  h.frame(&window);
  // At the end of the text, which is its left side.
  let end = shown(&window).2.expect("caret");
  window.key(&key(Key::Home));
  h.frame(&window);
  let start = shown(&window).2.expect("caret");
  assert!(
    end.x < start.x,
    "end {end:?} should be left of start {start:?}"
  );
}

#[test]
fn enter_in_a_field_without_on_accept_goes_to_the_window() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let entered = super::Counter::default();
  let e = entered.clone();
  let row = Row::new().child(TextInput::new(text).focus(true));
  let panel = h.panel().child(row).on_key(move |_, event| {
    if event.key == Key::Enter {
      e.hit();
    }
    true
  });
  let window = h.window(panel, 200.0, 30.0);
  window.keyboard_enter();
  window.key(&key(Key::Enter));
  assert_eq!(entered.get(), 1);
}

#[test]
fn line_breaks_from_outside_become_spaces() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::from("a\nb\r\nc"));
  let window = field(&mut h, text, |input| input);
  assert_eq!(shown(&window).0, "a b  c");
}

#[test]
fn an_input_method_composes_commits_and_deletes_around_the_caret() {
  use crate::Preedit;

  let mut h = Harness::new();
  let text = h.cx.signal(String::from("ab"));
  let window = field(&mut h, text, |input| input.focus(true));
  h.frame(&window);
  let state = window.field().expect("a focused field");
  assert_eq!(
    (state.text.as_str(), state.cursor, state.anchor),
    ("ab", 2, 2)
  );

  window.set_preedit(Some(Preedit {
    text: String::from("ni"),
    cursor: Some((1, 1)),
  }));
  h.frame(&window);
  assert_eq!(shown(&window).0, "abni");
  assert_eq!(text.get(), "ab", "composing writes nothing yet");
  let underline = match node_at(window.view().root(), &[0, 0]).content() {
    Content::Input(input) => input.underline.to_vec(),
    _ => panic!("not a field"),
  };
  assert_eq!(underline.len(), 1);

  window.set_preedit(None);
  window.commit_text("你");
  assert_eq!(text.get(), "ab你");
  // Lengths are in bytes: one byte would split the character, so it is
  // left alone; its three bytes go.
  window.delete_around(1, 0);
  assert_eq!(text.get(), "ab你");
  window.delete_around(3, 0);
  assert_eq!(text.get(), "ab");
}

#[test]
fn input_methods_see_no_field_without_the_keyboard_or_in_a_password() {
  let mut h = Harness::new();
  let text = h.cx.signal(String::new());
  let hidden = h.cx.signal(false);
  let row = Row::new().child(
    TextInput::new(text)
      .focus(true)
      .password(move || hidden.get()),
  );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  assert_eq!(window.field(), None, "the keyboard is elsewhere");
  window.keyboard_enter();
  assert!(window.field().is_some());
  hidden.set(true);
  h.frame(&window);
  assert_eq!(window.field(), None);
}

#[test]
fn input_methods_count_from_the_selection_and_replace_it_with_a_preedit() {
  use crate::Preedit;

  let mut h = Harness::new();
  let text = h.cx.signal(String::from("abcdef"));
  let window = field(&mut h, text, |input| input.focus(true));
  h.frame(&window);
  // Select "cd", then delete one byte either side of it.
  window.key(&key(Key::Left));
  window.key(&key(Key::Left));
  let shift = |key_: Key| KeyEvent {
    modifiers: Modifiers {
      shift: true,
      ..Modifiers::default()
    },
    ..key(key_)
  };
  window.key(&shift(Key::Left));
  window.key(&shift(Key::Left));
  window.delete_around(1, 1);
  assert_eq!(text.get(), "acdf");
  let state = window.field().expect("a field");
  assert_eq!((state.cursor, state.anchor), (1, 3), "the selection stays");
  // A preedit replaces the selection.
  window.set_preedit(Some(Preedit {
    text: String::from("x"),
    cursor: None,
  }));
  h.frame(&window);
  assert_eq!(text.get(), "af");
  assert_eq!(shown(&window).0, "axf");
  assert_eq!(
    shown(&window).2,
    None,
    "no caret when the input method hides it"
  );
}

#[test]
fn input_methods_leave_a_password_field_alone() {
  use crate::Preedit;

  let mut h = Harness::new();
  let text = h.cx.signal(String::from("pw"));
  let window = field(&mut h, text, |input| input.focus(true).password(true));
  h.frame(&window);
  window.commit_text("x");
  assert_eq!(text.get(), "pw");
  window.delete_around(1, 0);
  assert_eq!(text.get(), "pw");
  window.set_preedit(Some(Preedit {
    text: String::from("y"),
    cursor: None,
  }));
  h.frame(&window);
  assert_eq!(text.get(), "pw");
  assert_eq!(shown(&window).0, "••");
}
