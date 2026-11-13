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
use crate::widgets::{Element, ElementKind, Widget};

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
  style: Option<Prop<Style>>,
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
      style: None,
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
    self.style = Some(style.into_prop());
    self
  }
}

impl Widget for TextInput {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "TextInput",
      style: self.style,
      kind: ElementKind::Input(Box::new(self.input)),
      children: Vec::new(),
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
  /// How far the text is scrolled left to keep the caret in view.
  pub(crate) scroll: f32,
  /// Where the caret and the selection are, from the last layout, in
  /// window coordinates.
  pub(crate) caret: Rect,
  /// More than one span when right-to-left and left-to-right text mix.
  pub(crate) selection: Vec<Rect>,
  /// The content box the text is cut to, from the last layout.
  pub(crate) area: Rect,
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
      scroll: 0.0,
      caret: Rect::default(),
      selection: Vec::new(),
      area: Rect::default(),
      on_accept,
    }
  }

  /// Whether the placeholder is what shows.
  pub(crate) fn showing_placeholder(&self) -> bool {
    self.edit.text.is_empty()
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
    } else {
      at.min(self.edit.text.len())
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
    let caret = self.x_at(self.shown_index(self.edit.cursor));
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
