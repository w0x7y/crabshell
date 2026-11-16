//! Selecting text with the pointer. A left press places the caret, a drag
//! selects from there to the pointer wherever it goes, a second press
//! close in time and place selects a word, and Shift+press extends the
//! selection. The drag is the pointer's own, so it follows the pointer out
//! of the text and keeps it from scrolling a view.

use crate::edit::{Edit, word_at};
use crate::geometry::Point;
use crate::input::{DOUBLE_CLICK_DISTANCE, DOUBLE_CLICK_MS, distance};
use crate::tree::{NodeId, NodeKind, Tree};

/// What a press or a drag does to the selection.
#[derive(Clone, Copy)]
enum Select {
  /// Puts the caret here, with nothing selected.
  Place(usize),
  /// Moves the caret here, keeping the other end.
  Extend(usize),
  /// Selects the word here.
  Word(usize),
}

impl Tree {
  /// Whether node `id` selects text under the pointer: a field.
  pub(crate) fn selects(&self, id: NodeId) -> bool {
    matches!(
      self.node(id).map(|node| &node.kind),
      Some(NodeKind::Input(_))
    )
  }

  /// A left press at `at` on `id`, which selects text, at `time`: it
  /// takes the focus, then places the caret, or extends the selection
  /// with Shift held, or selects the word on a double click.
  pub(crate) fn press_text(&mut self, id: NodeId, at: Point, time: u32) {
    let Some(index) = self.text_index(id, at) else {
      return;
    };
    // Timed from press to press: the release comes after a drag.
    let double = self.pointer.last_click.is_some_and(|(last, then, place)| {
      last == id
        && time.wrapping_sub(then) <= DOUBLE_CLICK_MS
        && distance(place, at) <= DOUBLE_CLICK_DISTANCE
    });
    self.pointer.last_click = (!double).then_some((id, time, at));
    let how = if double {
      Select::Word(index)
    } else if self.keyboard.modifiers.shift && self.keyboard.focused == Some(id) {
      Select::Extend(index)
    } else {
      Select::Place(index)
    };
    self.focus(Some(id));
    self.select(id, how);
  }

  /// The left button held on `id` moved to `at`: the selection runs to
  /// the pointer.
  pub(crate) fn drag_text(&mut self, id: NodeId, at: Point) {
    if let Some(index) = self.text_index(id, at) {
      self.select(id, Select::Extend(index));
    }
  }

  /// The text position nearest `at`, in window coordinates, in node `id`.
  fn text_index(&self, id: NodeId, at: Point) -> Option<usize> {
    let point = self.local_point(id, at)?;
    match &self.node(id)?.kind {
      NodeKind::Input(input) => Some(input.hit(point)),
      _ => None,
    }
  }

  fn select(&mut self, id: NodeId, how: Select) {
    self.update_input(id, |input| {
      let Edit {
        text,
        cursor,
        anchor,
      } = &mut input.edit;
      apply(text, cursor, anchor, how);
      input.refresh();
    });
  }
}

/// Changes a caret and the other end of the selection in `text` as `how`
/// says.
fn apply(text: &str, cursor: &mut usize, anchor: &mut Option<usize>, how: Select) {
  match how {
    Select::Place(at) => {
      *cursor = at;
      *anchor = Some(at);
    }
    Select::Extend(at) => {
      anchor.get_or_insert(*cursor);
      *cursor = at;
    }
    Select::Word(at) => {
      let word = word_at(text, at);
      *anchor = Some(word.start);
      *cursor = word.end;
    }
  }
}
