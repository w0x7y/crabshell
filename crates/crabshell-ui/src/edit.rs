//! One line of editable text: the text, the caret and the selection, and
//! what each key does to them. Positions are byte offsets that always sit
//! between graphemes, so a caret never splits an accented letter or an
//! emoji.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

use crate::keyboard::{Key, KeyEvent};

/// The text being edited, with the caret and the other end of the
/// selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Edit {
  pub(crate) text: String,
  /// Where the caret is.
  pub(crate) cursor: usize,
  /// Where the selection started, if there is one.
  pub(crate) anchor: Option<usize>,
}

/// What a key did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Keyed {
  /// The text changed.
  Edited,
  /// Only the caret or the selection moved.
  Moved,
  /// Enter was pressed.
  Accept,
  /// Ctrl+C: this selected text is to be copied.
  Copy(String),
  /// Ctrl+X: this selected text was cut and is to be copied.
  Cut(String),
  /// Ctrl+V: the clipboard is to be pasted.
  Paste,
  /// The key means nothing here; others may use it.
  Unused,
}

impl Edit {
  /// Takes `text` from outside, with the caret at its end. Text that
  /// typing just wrote comes back the same and changes nothing. A field is
  /// one line, so line breaks become spaces.
  pub(crate) fn set_text(&mut self, text: String) {
    let text = if text.contains(['\n', '\r']) {
      text.replace(['\n', '\r'], " ")
    } else {
      text
    };
    if text == self.text {
      return;
    }
    self.text = text;
    self.cursor = self.text.len();
    self.anchor = None;
  }

  /// The selected bytes, if any.
  pub(crate) fn selection(&self) -> Option<Range<usize>> {
    let anchor = self.anchor.filter(|&anchor| anchor != self.cursor)?;
    Some(anchor.min(self.cursor)..anchor.max(self.cursor))
  }

  /// Replaces the selection, or inserts at the caret.
  pub(crate) fn insert(&mut self, text: &str) {
    let range = self.selection().unwrap_or(self.cursor..self.cursor);
    self.text.replace_range(range.clone(), text);
    self.cursor = range.start + text.len();
    self.anchor = None;
  }

  /// What `event` does. Ctrl moves and deletes by word, and Shift extends
  /// the selection. Copying and cutting need `copyable`, which a password
  /// isn't.
  pub(crate) fn key(&mut self, event: &KeyEvent, copyable: bool) -> Keyed {
    let modifiers = event.modifiers;
    let word = modifiers.ctrl;
    let command = modifiers.ctrl && !modifiers.alt;
    let before = self.clone();
    match &event.key {
      Key::Enter => return Keyed::Accept,
      Key::Character('c' | 'C' | 'x' | 'X') if command && !copyable => return Keyed::Moved,
      Key::Character('c' | 'C') if command => {
        return match self.selection() {
          Some(range) => Keyed::Copy(self.text[range].to_owned()),
          None => Keyed::Unused,
        };
      }
      Key::Character('x' | 'X') if command => {
        let Some(range) = self.selection() else {
          return Keyed::Unused;
        };
        let cut = self.text[range].to_owned();
        self.insert("");
        return Keyed::Cut(cut);
      }
      Key::Character('v' | 'V') if command => return Keyed::Paste,
      Key::Character('a' | 'A') if modifiers.ctrl && !modifiers.alt => {
        self.anchor = Some(0);
        self.cursor = self.text.len();
      }
      Key::Backspace => self.erase(|edit| edit.back(edit.cursor, word)),
      Key::Delete => self.erase(|edit| edit.forward(edit.cursor, word)),
      Key::Left => self.motion(modifiers.shift, |edit, at| edit.back(at, word), true),
      Key::Right => self.motion(modifiers.shift, |edit, at| edit.forward(at, word), false),
      Key::Home => self.motion(modifiers.shift, |_, _| 0, true),
      Key::End => self.motion(modifiers.shift, |edit, _| edit.text.len(), false),
      _ => match &event.text {
        Some(text) if !modifiers.ctrl && !modifiers.alt && !modifiers.logo => self.insert(text),
        _ => return Keyed::Unused,
      },
    }
    // A key that changed nothing, like Backspace at the start, was still
    // the field's.
    if self.text != before.text {
      Keyed::Edited
    } else {
      Keyed::Moved
    }
  }

  /// Deletes the selection, or from the caret to where `to` says.
  fn erase(&mut self, to: impl FnOnce(&Edit) -> usize) {
    let range = match self.selection() {
      Some(range) => range,
      None => {
        let other = to(self);
        other.min(self.cursor)..other.max(self.cursor)
      }
    };
    self.text.replace_range(range.clone(), "");
    self.cursor = range.start;
    self.anchor = None;
  }

  /// Moves the caret to where `to` says, extending the selection when
  /// `extend`. Without it, an arrow first collapses a selection to its
  /// `start` or end side.
  fn motion(&mut self, extend: bool, to: impl FnOnce(&Edit, usize) -> usize, start: bool) {
    if extend {
      self.anchor.get_or_insert(self.cursor);
      self.cursor = to(self, self.cursor);
      return;
    }
    if let Some(range) = self.selection() {
      self.cursor = if start { range.start } else { range.end };
    } else {
      self.cursor = to(self, self.cursor);
    }
    self.anchor = None;
  }

  /// The position one grapheme, or one word, before `at`.
  fn back(&self, at: usize, word: bool) -> usize {
    let before = &self.text[..at];
    if word {
      // The start of the last word before `at`, skipping spaces.
      before
        .split_word_bound_indices()
        .rev()
        .find(|(_, part)| !part.trim().is_empty())
        .map_or(0, |(index, _)| index)
    } else {
      before
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index)
    }
  }

  /// The position one grapheme, or one word, after `at`.
  fn forward(&self, at: usize, word: bool) -> usize {
    let after = &self.text[at..];
    let length = if word {
      // The end of the next word after `at`, skipping spaces.
      after
        .split_word_bound_indices()
        .find(|(_, part)| !part.trim().is_empty())
        .map_or(after.len(), |(index, part)| index + part.len())
    } else {
      after.graphemes(true).next().map_or(0, str::len)
    };
    at + length
  }
}

/// How many graphemes come before byte `at` of `text`.
pub(crate) fn graphemes_before(text: &str, at: usize) -> usize {
  text[..at].graphemes(true).count()
}

/// The byte where grapheme number `count` of `text` starts, or the end.
pub(crate) fn grapheme_start(text: &str, count: usize) -> usize {
  text
    .grapheme_indices(true)
    .nth(count)
    .map_or(text.len(), |(index, _)| index)
}

/// The word around byte `at` of `text`, or the spaces or the one mark
/// there: what a double click selects. At the end, the last one.
pub(crate) fn word_at(text: &str, at: usize) -> Range<usize> {
  text
    .split_word_bound_indices()
    .map(|(start, part)| start..start + part.len())
    .find(|range| range.contains(&at) || range.end == text.len())
    .unwrap_or(at..at)
}
