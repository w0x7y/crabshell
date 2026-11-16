//! Selecting text with the pointer, in a field or a selectable text. A
//! left press places the caret, a drag selects from there to the pointer
//! wherever it goes, a second press close in time and place selects a
//! word, and Shift+press extends the selection. The drag is the pointer's
//! own, so it follows the pointer out of the text and keeps it from
//! scrolling a view.

use std::ops::Range;

use cosmic_text::{Buffer, Cursor};

use crate::edit::{Edit, word_at};
use crate::geometry::{Point, Rect};
use crate::input::{DOUBLE_CLICK_DISTANCE, DOUBLE_CLICK_MS, distance};
use crate::keyboard::{Clipboard, Key, KeyEvent};
use crate::text::TextNode;
use crate::tree::{Change, NodeId, NodeKind, Tree};

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
  /// Whether node `id` selects text under the pointer: a field, or a
  /// selectable text.
  pub(crate) fn selects(&self, id: NodeId) -> bool {
    match self.node(id).map(|node| &node.kind) {
      Some(NodeKind::Input(_)) => true,
      Some(NodeKind::Text(text)) => text.selectable,
      _ => false,
    }
  }

  /// A left press on `on`, or on nothing: a selectable text holding the
  /// focus elsewhere lets go of it, and so of its selection.
  pub(crate) fn press_away(&mut self, on: Option<NodeId>) {
    let focused = self.keyboard.focused.filter(|&id| Some(id) != on);
    if focused.is_some_and(|id| {
      matches!(
        self.node(id).map(|node| &node.kind),
        Some(NodeKind::Text(_))
      )
    }) {
      self.focus(None);
    }
  }

  /// Drops the selection of text `id`, as it loses the focus.
  pub(crate) fn drop_selection(&mut self, id: NodeId) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Text(text) => match text.anchor.take() {
        Some(_) => Change::Paint,
        None => Change::None,
      },
      _ => Change::None,
    });
  }

  /// Makes text `id` selectable or not. Turned off, it drops the focus,
  /// and so its selection: only the focused text holds one.
  pub(crate) fn set_selectable(&mut self, id: NodeId, selectable: bool) {
    if let Some(NodeKind::Text(text)) = self.node_mut(id).map(|node| &mut node.kind) {
      text.selectable = selectable;
    }
    if !selectable && self.keyboard.focused == Some(id) {
      self.focus(None);
    }
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
      NodeKind::Text(text) if text.selectable => Some(text_hit(text, point)),
      _ => None,
    }
  }

  fn select(&mut self, id: NodeId, how: Select) {
    if let Some(NodeKind::Text(_)) = self.node(id).map(|node| &node.kind) {
      self.update(id, |node| match &mut node.kind {
        NodeKind::Text(TextNode {
          content,
          cursor,
          anchor,
          ..
        }) => {
          apply(content, cursor, anchor, how);
          Change::Paint
        }
        _ => Change::None,
      });
      return;
    }
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

/// What a selectable text copies on Ctrl+C: its selection, if any.
pub(crate) fn copy_key(text: &TextNode, event: &KeyEvent) -> Option<Clipboard> {
  let ctrl_c =
    event.modifiers.ctrl && !event.modifiers.alt && matches!(event.key, Key::Character('c' | 'C'));
  let range = ctrl_c.then(|| text_selection(text)).flatten()?;
  Some(Clipboard::Copy(text.content.get(range)?.to_owned()))
}

/// The bytes a selectable text has selected, if any.
pub(crate) fn text_selection(text: &TextNode) -> Option<Range<usize>> {
  let anchor = text
    .anchor
    .filter(|&anchor| text.selectable && anchor != text.cursor)?;
  Some(anchor.min(text.cursor)..anchor.max(text.cursor))
}

/// The cosmic-text cursor at byte `at` of the text `buffer` holds, which
/// it splits into lines at each line break.
pub(crate) fn cursor_at(buffer: &Buffer, at: usize) -> Cursor {
  let mut start = 0;
  let last = buffer.lines.len().saturating_sub(1);
  for (index, line) in buffer.lines.iter().enumerate() {
    let length = line.text().len();
    let next = start + length + line.ending().as_str().len();
    if at < next || index == last {
      return Cursor::new(index, at.saturating_sub(start).min(length));
    }
    start = next;
  }
  Cursor::new(0, 0)
}

/// The byte of the text `buffer` holds that `cursor` points at.
pub(crate) fn index_of(buffer: &Buffer, cursor: Cursor) -> usize {
  let before: usize = buffer.lines[..cursor.line.min(buffer.lines.len())]
    .iter()
    .map(|line| line.text().len() + line.ending().as_str().len())
    .sum();
  before + cursor.index
}

/// The byte of `text` nearest `point`, in window coordinates.
pub(crate) fn text_hit(text: &TextNode, point: Point) -> usize {
  let origin = text.origin;
  text
    .buffer
    .hit(point.x - origin.x, point.y - origin.y)
    .map_or(0, |cursor| index_of(&text.buffer, cursor))
}

/// Where bytes `range` of `text` show, in window coordinates: a rectangle
/// for each line they cover, or more where right-to-left and
/// left-to-right text mix.
pub(crate) fn highlights(text: &TextNode, range: Range<usize>) -> Vec<Rect> {
  let (start, end) = (
    cursor_at(&text.buffer, range.start),
    cursor_at(&text.buffer, range.end),
  );
  let origin = text.origin;
  text
    .runs()
    .filter(|run| (start.line..=end.line).contains(&run.line_i))
    .flat_map(|run| {
      run.highlight(start, end).map(move |(x, width)| {
        Rect::new(
          origin.x + x,
          origin.y + run.line_top,
          width,
          run.line_height,
        )
      })
    })
    .collect()
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
