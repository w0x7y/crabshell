//! What input methods are told about a field.

use crabshell_ui::{FieldState, Rect};

use crate::text_input::surrounding;

fn state(text: &str, cursor: usize, anchor: usize) -> FieldState {
  FieldState {
    field: 1,
    text: text.to_owned(),
    cursor,
    anchor,
    caret: Rect::default(),
  }
}

#[test]
fn short_text_goes_whole_with_its_caret() {
  assert_eq!(surrounding(&state("hello", 2, 4)), ("hello", 2, 4));
}

#[test]
fn long_text_is_cut_around_the_caret_on_character_boundaries() {
  // 3000 three-byte characters: 9000 bytes.
  let text = "你".repeat(3000);
  let cursor = 4500;
  let middle = state(&text, cursor, cursor);
  let (sent, at, anchor) = surrounding(&middle);
  assert!(sent.len() <= 4000, "{} bytes", sent.len());
  assert!(sent.chars().all(|c| c == '你'));
  assert_eq!((at, anchor), (at, at));
  assert!(
    at > 0 && (at as usize) < sent.len(),
    "the caret is inside: {at}"
  );
  // A selection longer than the cap keeps the caret's end.
  let text = format!("{}end", "你".repeat(1500));
  let all = state(&text, text.len(), 0);
  let (sent, at, anchor) = surrounding(&all);
  assert!(sent.len() <= 4000);
  assert!(sent.ends_with("end"), "the text around the caret");
  assert_eq!(at as usize, sent.len(), "the caret at the end");
  assert_eq!(anchor, 0, "the anchor as far as the text sent goes");
}
