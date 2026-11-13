//! Turning sctk's key events into the ui's.

use crabshell_ui::{Key, Modifiers};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use crate::keyboard::{key, key_event};

fn event(keysym: Keysym, utf8: Option<&str>) -> KeyEvent {
  KeyEvent {
    time: 0,
    raw_code: 0,
    keysym,
    utf8: utf8.map(str::to_owned),
  }
}

#[test]
fn keysyms_get_the_ui_names() {
  assert_eq!(key(Keysym::Return), Key::Enter);
  assert_eq!(key(Keysym::KP_Enter), Key::Enter);
  assert_eq!(key(Keysym::ISO_Left_Tab), Key::Tab);
  assert_eq!(key(Keysym::KP_Left), Key::Left);
  assert_eq!(key(Keysym::Page_Down), Key::PageDown);
  assert_eq!(key(Keysym::F1), Key::Function(1));
  assert_eq!(key(Keysym::F12), Key::Function(12));
  assert_eq!(key(Keysym::F35), Key::Function(35));
  assert_eq!(key(Keysym::a), Key::Character('a'));
  assert_eq!(key(Keysym::A), Key::Character('A'));
  assert_eq!(key(Keysym::space), Key::Character(' '));
  assert_eq!(key(Keysym::eacute), Key::Character('é'));
  assert_eq!(key(Keysym::Shift_L), Key::Other(Keysym::Shift_L.raw()));
}

#[test]
fn only_printable_text_is_typed() {
  let shift = Modifiers {
    shift: true,
    ..Modifiers::default()
  };
  let typed = key_event(&event(Keysym::A, Some("A")), shift, true);
  assert_eq!(typed.text.as_deref(), Some("A"));
  assert_eq!(typed.modifiers, shift);
  assert!(typed.repeat);
  // Enter types "\r", Ctrl+a types "\u{1}", and a release types nothing.
  assert_eq!(
    key_event(&event(Keysym::Return, Some("\r")), shift, false).text,
    None
  );
  assert_eq!(
    key_event(&event(Keysym::a, Some("\u{1}")), shift, false).text,
    None
  );
  assert_eq!(
    key_event(&event(Keysym::a, Some("")), shift, false).text,
    None
  );
  assert_eq!(key_event(&event(Keysym::a, None), shift, false).text, None);
}
