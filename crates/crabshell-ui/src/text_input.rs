//! [`TextInput`], a one-line text field.

use std::rc::Rc;

use cosmic_text::Cursor;
use crabshell_reactive::Signal;

use crate::cx::Cx;
use crate::edit::{Edit, grapheme_start, graphemes_before};
use crate::geometry::{Point, Rect};
use crate::input::Handler;
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::text::TextNode;
use crate::tree::{NodeKind, borrow};
use crate::widgets::{Common, Element, ElementKind, Widget, visibility_methods};
use crate::window::Window;

/// What a password shows for each character.
const DOT: char = '•';

/// The width a field takes when nothing else sets one, in logical pixels.
pub(crate) const DEFAULT_WIDTH: f32 = 200.0;

pub(crate) type AcceptHandler = Rc<Handler<dyn Fn(&Cx, &str)>>;

pub(crate) struct InputProps {
  pub(crate) text: Signal<String>,
  pub(crate) placeholder: Option<Prop<String>>,
  pub(crate) password: Option<Prop<bool>>,
  pub(crate) focus: Option<Prop<bool>>,
  pub(crate) on_accept: Option<AcceptHandler>,
}

/// A one-line text field. It takes the keyboard focus when clicked, when
/// Tab reaches it, or when [`focus`](Self::focus) says so, and edits the
/// text in a shared signal: typing writes it, and setting it changes what
/// the field shows. Arrows, Home and End move the caret, Shift selects,
/// Ctrl moves and deletes by word, and Ctrl+A selects everything. Keys it
/// doesn't use, such as Up, Down and Escape, go on to the window's
/// `on_key`.
///
/// ```
/// use crabshell_ui::{Cx, TextInput};
///
/// fn search(cx: &Cx) -> TextInput {
///     let query = cx.signal(String::new());
///     TextInput::new(query)
///         .placeholder("Search")
///         .on_accept(|_cx, text| println!("run {text}"))
/// }
/// ```
pub struct TextInput {
  input: InputProps,
  common: Common,
}

impl TextInput {
  /// A field editing `text`.
  pub fn new(text: Signal<String>) -> TextInput {
    TextInput {
      input: InputProps {
        text,
        placeholder: None,
        password: None,
        focus: None,
        on_accept: None,
      },
      common: Common::default(),
    }
  }

  /// Sets the text shown, dimmed, while the field is empty.
  pub fn placeholder(mut self, text: impl IntoProp<String>) -> TextInput {
    self.input.placeholder = Some(text.into_prop());
    self
  }

  /// Sets whether the field shows a dot for each character instead of
  /// the text.
  pub fn password(mut self, password: impl IntoProp<bool>) -> TextInput {
    self.input.password = Some(password.into_prop());
    self
  }

  /// Takes the keyboard focus whenever this turns true, as when a
  /// launcher opens.
  pub fn focus(mut self, focus: impl IntoProp<bool>) -> TextInput {
    self.input.focus = Some(focus.into_prop());
    self
  }

  /// Runs `handler` with the text when Enter is pressed in the field.
  pub fn on_accept(mut self, handler: impl Fn(&Cx, &str) + 'static) -> TextInput {
    self.input.on_accept = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Sets the style. A field cuts its text to the inside of its border.
  pub fn style(mut self, style: impl IntoProp<Style>) -> TextInput {
    self.common.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for TextInput {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "TextInput",
      common: self.common,
      kind: ElementKind::Input(Box::new(self.input)),
      children: Vec::new(),
    }
  }
}

/// Text an input method is composing, shown at the caret, underlined,
/// until it commits or drops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
  /// The text.
  pub text: String,
  /// Where in `text`, in bytes, its own caret or selection is, if shown.
  pub cursor: Option<(usize, usize)>,
}

/// The focused text field, as an input method needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldState {
  /// Which field it is: another number when the focus moves to another.
  pub field: u64,
  /// The field's text.
  pub text: String,
  /// The caret, in bytes.
  pub cursor: usize,
  /// The other end of the selection, or the caret without one.
  pub anchor: usize,
  /// Where the caret is drawn, in window coordinates.
  pub caret: Rect,
}

impl Window {
  /// The focused text field, while the window has the keyboard, for an
  /// input method. Never a password: input methods learn what is typed.
  pub fn field(&self) -> Option<FieldState> {
    let tree = self.tree.borrow();
    if !tree.keyboard.entered {
      return None;
    }
    let id = tree.keyboard.focused?;
    let NodeKind::Input(input) = &tree.node(id)?.kind else {
      return None;
    };
    (!input.password).then(|| FieldState {
      field: u64::from(id),
      text: input.edit.text.clone(),
      cursor: input.edit.cursor,
      anchor: input.edit.anchor.unwrap_or(input.edit.cursor),
      caret: tree.window_rect(id, input.caret),
    })
  }

  /// Inserts `text` from an input method at the focused field's caret,
  /// replacing its selection. A password field takes nothing.
  pub fn commit_text(&self, text: &str) {
    let text = text.replace(['\n', '\r'], " ");
    self.edit_field(|edit| edit.insert(&text));
  }

  /// Pastes `text` from the clipboard into the focused field, replacing
  /// its selection; line breaks become spaces. Unlike an input method's
  /// text, a paste goes into a password field too.
  pub fn paste(&self, text: &str) {
    // An empty clipboard pastes nothing, not even over the selection.
    if text.is_empty() {
      return;
    }
    let text = text.replace(['\n', '\r'], " ");
    self.change_field(true, |edit| edit.insert(&text));
  }

  /// Deletes `before` bytes before the focused field's selection, or its
  /// caret, and `after` bytes after it, for an input method; the
  /// selection itself stays. Lengths that would split a character are
  /// ignored.
  pub fn delete_around(&self, before: usize, after: usize) {
    self.edit_field(|edit| {
      let selected = edit.selection().unwrap_or(edit.cursor..edit.cursor);
      let start = selected.start.saturating_sub(before);
      let end = selected.end.saturating_add(after).min(edit.text.len());
      if !(edit.text.is_char_boundary(start) && edit.text.is_char_boundary(end)) {
        return;
      }
      edit.text.replace_range(selected.end..end, "");
      edit.text.replace_range(start..selected.start, "");
      let removed = selected.start - start;
      edit.cursor -= removed;
      edit.anchor = edit.anchor.map(|anchor| anchor - removed);
    });
  }

  /// Shows `preedit` at the focused field's caret, replacing its
  /// selection, or clears it. A password field shows none.
  pub fn set_preedit(&self, preedit: Option<Preedit>) {
    let preedit = preedit.filter(|preedit| !preedit.text.is_empty());
    if preedit.is_some() {
      self.edit_field(|edit| {
        if edit.selection().is_some() {
          edit.insert("");
        }
      });
    }
    let mut tree = borrow(&self.tree);
    let Some(id) = tree.keyboard.focused else {
      return;
    };
    if matches!(tree.node(id).map(|node| &node.kind), Some(NodeKind::Input(input)) if input.password)
    {
      return;
    }
    tree.update_input(id, |input| {
      input.preedit = preedit;
      input.refresh();
    });
  }

  /// Changes the focused field's text with `change`, then writes it to
  /// the field's signal once the tree is released. A password field is
  /// left alone: input methods don't type in one.
  fn edit_field(&self, change: impl FnOnce(&mut Edit)) {
    self.change_field(false, change);
  }

  /// As [`edit_field`](Self::edit_field), into a password field too when
  /// `passwords`.
  fn change_field(&self, passwords: bool, change: impl FnOnce(&mut Edit)) {
    let write = {
      let mut tree = borrow(&self.tree);
      let Some(id) = tree.keyboard.focused else {
        return;
      };
      let before = match tree.node(id).map(|node| &node.kind) {
        Some(NodeKind::Input(input)) if passwords || !input.password => input.edit.text.clone(),
        _ => return,
      };
      let mut write = None;
      tree.update_input(id, |input| {
        change(&mut input.edit);
        input.refresh();
        if input.edit.text != before {
          write = Some((input.signal, input.edit.text.clone()));
        }
      });
      write
    };
    if let Some((signal, text)) = write {
      // A signal whose owner is gone just isn't written.
      signal.try_set(text);
    }
  }
}

/// A field in the tree.
pub(crate) struct InputNode {
  /// What is shown: the text, its dots, or the placeholder.
  pub(crate) text: TextNode,
  pub(crate) edit: Edit,
  /// Written as the text is edited.
  pub(crate) signal: Signal<String>,
  pub(crate) placeholder: String,
  pub(crate) password: bool,
  /// The field has the keyboard, so it shows its caret.
  pub(crate) focused: bool,
  /// The last value of its `focus` binding, which it takes the focus by
  /// whenever it is shown.
  pub(crate) wants_focus: bool,
  /// How far the text is scrolled left to keep the caret in view.
  pub(crate) scroll: f32,
  /// Where the caret and the selection are, from the last layout, in
  /// window coordinates.
  pub(crate) caret: Rect,
  /// More than one span when right-to-left and left-to-right text mix.
  pub(crate) selection: Vec<Rect>,
  /// The content box the text is cut to, from the last layout.
  pub(crate) area: Rect,
  /// What an input method is composing, shown at the caret.
  pub(crate) preedit: Option<Preedit>,
  /// The line under the preedit, from the last layout.
  pub(crate) underline: Vec<Rect>,
  /// Kept in the reactive runtime, which drops the user's closure when the
  /// field's owner goes.
  pub(crate) on_accept: Signal<Option<AcceptHandler>>,
}

impl InputNode {
  pub(crate) fn new(signal: Signal<String>, on_accept: Signal<Option<AcceptHandler>>) -> InputNode {
    InputNode {
      text: TextNode::new(),
      edit: Edit::default(),
      signal,
      placeholder: String::new(),
      password: false,
      focused: false,
      wants_focus: false,
      scroll: 0.0,
      caret: Rect::default(),
      selection: Vec::new(),
      area: Rect::default(),
      preedit: None,
      underline: Vec::new(),
      on_accept,
    }
  }

  /// Whether the placeholder is what shows.
  pub(crate) fn showing_placeholder(&self) -> bool {
    self.edit.text.is_empty() && self.preedit.is_none()
  }

  /// The preedit's length in bytes, 0 without one.
  fn preedit_len(&self) -> usize {
    self
      .preedit
      .as_ref()
      .map_or(0, |preedit| preedit.text.len())
  }

  /// Puts what should show into the text node. Returns whether that
  /// changed, which needs a layout.
  pub(crate) fn refresh(&mut self) -> bool {
    let shown = if self.showing_placeholder() {
      self.placeholder.clone()
    } else if self.password {
      DOT
        .to_string()
        .repeat(graphemes_before(&self.edit.text, self.edit.text.len()))
    } else if let Some(preedit) = &self.preedit {
      let (before, after) = self.edit.text.split_at(self.edit.cursor);
      format!("{before}{}{after}", preedit.text)
    } else {
      self.edit.text.clone()
    };
    self.text.set_content(shown)
  }

  /// The byte in what is shown that matches byte `at` of the text.
  fn shown_index(&self, at: usize) -> usize {
    if self.showing_placeholder() {
      0
    } else if self.password {
      graphemes_before(&self.edit.text, at) * DOT.len_utf8()
    } else if at > self.edit.cursor {
      at + self.preedit_len()
    } else {
      at
    }
  }

  /// The byte of the text that matches byte `at` of what is shown.
  fn text_index(&self, at: usize) -> usize {
    if self.showing_placeholder() {
      0
    } else if self.password {
      grapheme_start(&self.edit.text, at / DOT.len_utf8())
    } else if at <= self.edit.cursor {
      at
    } else {
      // Inside the preedit is at the caret; past it, shifted back.
      let after = at.saturating_sub(self.preedit_len()).max(self.edit.cursor);
      after.min(self.edit.text.len())
    }
  }

  /// How far from the line's left edge the caret before byte `at` of
  /// what is shown sits. In right-to-left text, the start is on the right.
  fn x_at(&self, at: usize) -> f32 {
    self
      .text
      .runs()
      .next()
      .and_then(|run| run.cursor_position(&Cursor::new(0, at)))
      .unwrap_or(0.0)
  }

  /// Scrolls the text to keep the caret inside `content`, then places the
  /// caret and the selection. Called after the text node's own layout.
  pub(crate) fn place(&mut self, content: Rect) {
    let at = self.shown_index(self.edit.cursor);
    let caret = match &self.preedit {
      Some(preedit) => {
        at + preedit
          .cursor
          .map_or(preedit.text.len(), |(start, _)| start)
      }
      None => at,
    };
    let caret = self.x_at(caret);
    let width = self.text.runs().next().map_or(0.0, |run| run.line_w);
    // The caret's own pixel counts as text, so it shows at the end.
    let room = (content.width - 1.0).max(0.0);
    let mut scroll = self.scroll.min((width - room).max(0.0));
    if caret - scroll > room {
      scroll = caret - room;
    }
    if caret < scroll {
      scroll = caret;
    }
    self.scroll = scroll.max(0.0);
    self.text.origin.x = content.x - self.scroll;
    let origin = self.text.origin;
    let height = self
      .text
      .runs()
      .next()
      .map_or(content.height, |run| run.line_height);
    self.caret = Rect::new(origin.x + caret, origin.y, 1.0, height);
    self.area = content;
    self.selection = match (self.edit.selection(), self.text.runs().next()) {
      (Some(range), Some(run)) => {
        let start = Cursor::new(0, self.shown_index(range.start));
        let end = Cursor::new(0, self.shown_index(range.end));
        run
          .highlight(start, end)
          .map(|(x, width)| Rect::new(origin.x + x, origin.y, width, height))
          .collect()
      }
      _ => Vec::new(),
    };
    self.underline = match (&self.preedit, self.text.runs().next()) {
      (Some(preedit), Some(run)) => run
        .highlight(Cursor::new(0, at), Cursor::new(0, at + preedit.text.len()))
        .map(|(x, width)| Rect::new(origin.x + x, origin.y + height - 1.0, width, 1.0))
        .collect(),
      _ => Vec::new(),
    };
  }

  /// The text position nearest `point`, in window coordinates.
  pub(crate) fn hit(&self, point: Point) -> usize {
    let origin = self.text.origin;
    let at = self
      .text
      .buffer
      .hit(point.x - origin.x, point.y - origin.y)
      .map_or(0, |cursor| cursor.index);
    self.text_index(at)
  }
}
