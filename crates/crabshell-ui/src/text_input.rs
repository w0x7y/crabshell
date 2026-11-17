//! [`TextInput`], a one-line text field, and the field node it shares
//! with [`TextArea`](crate::TextArea).

use std::rc::Rc;

use cosmic_text::{Affinity, FontSystem, LayoutRun};
use crabshell_reactive::Signal;

use crate::cx::Cx;
use crate::edit::{Edit, Keyed, grapheme_start, graphemes_before};
use crate::geometry::{Point, Rect};
use crate::input::Handler;
use crate::keyboard::{Key, KeyEvent};
use crate::prop::{IntoProp, Prop};
use crate::select::{cursor_at, highlights, index_of};
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
  /// A text area: many lines.
  pub(crate) multiline: bool,
  pub(crate) wrap: Option<Prop<bool>>,
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
        multiline: false,
        wrap: None,
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
  /// Its text takes the font, color, alignment, line height and
  /// decorations it inherits or sets, as a [`Text`](crate::Text) does.
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
  /// A [`TextArea`](crate::TextArea), whose text has line breaks.
  pub multiline: bool,
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
      multiline: input.edit.multiline,
    })
  }

  /// Inserts `text` from an input method at the focused field's caret,
  /// replacing its selection; in a one-line field, line breaks become
  /// spaces. A password field takes nothing.
  pub fn commit_text(&self, text: &str) {
    self.edit_field(|edit| edit.insert(text));
  }

  /// Pastes `text` from the clipboard into the focused field, replacing
  /// its selection; in a one-line field, line breaks become spaces.
  /// Unlike an input method's text, a paste goes into a password field
  /// too.
  pub fn paste(&self, text: &str) {
    // An empty clipboard pastes nothing, not even over the selection.
    if text.is_empty() {
      return;
    }
    self.change_field(true, |edit| edit.insert(text));
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
  /// Where across Up and Down keep the caret, and the caret position it
  /// led to: a move that lands anywhere else forgets it.
  pub(crate) goal: Option<(usize, f32)>,
  /// A caret position shown at the end of a wrapped line rather than at
  /// the start of the next, where both are the same byte.
  pub(crate) upstream: Option<usize>,
  /// Kept in the reactive runtime, which drops the user's closure when the
  /// field's owner goes.
  pub(crate) on_accept: Signal<Option<AcceptHandler>>,
}

impl InputNode {
  pub(crate) fn new(
    signal: Signal<String>,
    on_accept: Signal<Option<AcceptHandler>>,
    multiline: bool,
  ) -> InputNode {
    let mut text = TextNode::new();
    if multiline {
      text.set_max_lines(u32::MAX);
    }
    InputNode {
      text,
      edit: Edit {
        multiline,
        ..Edit::default()
      },
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
      goal: None,
      upstream: None,
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

  /// The line the caret before byte `at` of what is shown is drawn on:
  /// the first of two that meet there when `upstream`, else the last.
  fn caret_run(&self, at: usize, upstream: bool) -> Option<LayoutRun<'_>> {
    let cursor = cursor_at(&self.text.buffer, at);
    let mut runs = self.text.runs().filter(|run| {
      run.line_i == cursor.line
        && (run.glyphs.is_empty() || {
          let start = run.glyphs.iter().map(|glyph| glyph.start).min();
          let end = run.glyphs.iter().map(|glyph| glyph.end).max();
          start.is_some_and(|start| start <= cursor.index)
            && end.is_some_and(|end| cursor.index <= end)
        })
    });
    if upstream { runs.next() } else { runs.last() }
  }

  /// Where the caret before byte `at` of what is shown is, from the
  /// text's corner: across, the top of its line and the line's height.
  /// In right-to-left text, the start is on the right. On an empty line,
  /// or over the placeholder, it is where typed text would start.
  fn caret_spot(&self, at: usize, upstream: bool) -> Option<(f32, f32, f32)> {
    let run = self
      .caret_run(at, upstream)
      .or_else(|| self.text.runs().next())?;
    let x = if run.glyphs.is_empty() || self.showing_placeholder() {
      self.text.empty_line_place(&run) * self.text.layout_width()
    } else {
      run
        .cursor_position(&cursor_at(&self.text.buffer, at))
        .unwrap_or(0.0)
    };
    Some((x, run.line_top, run.line_height))
  }

  /// The part of a line `height` tall from `top` that its glyphs fill,
  /// as top and height: the default line height's, centered as
  /// cosmic-text centers the glyphs, when the style's is taller.
  fn glyph_box(&self, top: f32, height: f32) -> (f32, f32) {
    let natural = (1.2 * self.text.buffer.metrics().font_size).round();
    let glyphs = natural.min(height);
    (top + ((height - glyphs) / 2.0).round(), glyphs)
  }

  /// Lays the text out in the field's `content` box. Aligned text sits
  /// in the room left of the caret's last pixel; a line too long for it
  /// starts at the start, as unaligned text does, and scrolls.
  pub(crate) fn finish(&mut self, content: Rect, fonts: &mut FontSystem) {
    let room = (content.width - 1.0).max(0.0);
    let width = match (self.text.aligned(), self.text.wraps()) {
      (false, wraps) => wraps.then_some(content.width),
      (true, true) => Some(room),
      // A long placeholder is cut, but typed text starts where it should.
      (true, false) if self.showing_placeholder() => Some(room),
      (true, false) => Some(self.text.width().max(room)),
    };
    self.text.finish_at(content, width, fonts);
  }

  /// Scrolls the text sideways to keep the caret inside `content`, then
  /// places the caret and the selection. Called after the text node's own
  /// layout.
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
    let upstream = self.upstream == Some(self.edit.cursor) && self.preedit.is_none();
    let (x, top, height) = self
      .caret_spot(caret, upstream)
      .unwrap_or((0.0, 0.0, content.height));
    let width = self.text.runs().map(|run| run.line_w).fold(0.0, f32::max);
    // The caret's own pixel counts as text, so it shows at the end.
    let room = (content.width - 1.0).max(0.0);
    let mut scroll = self.scroll.min((width - room).max(0.0));
    if x - scroll > room {
      scroll = x - room;
    }
    if x < scroll {
      scroll = x;
    }
    self.scroll = scroll.max(0.0);
    self.text.origin.x = content.x - self.scroll;
    let origin = self.text.origin;
    let (top, height) = self.glyph_box(top, height);
    self.caret = Rect::new(origin.x + x, origin.y + top, 1.0, height);
    self.area = content;
    self.selection = match self.edit.selection() {
      Some(range) => highlights(
        &self.text,
        self.shown_index(range.start)..self.shown_index(range.end),
      ),
      None => Vec::new(),
    };
    self.underline = match &self.preedit {
      Some(preedit) => highlights(&self.text, at..at + preedit.text.len())
        .into_iter()
        .map(|span| {
          let (top, height) = self.glyph_box(span.y, span.height);
          Rect::new(span.x, top + height - 1.0, span.width, 1.0)
        })
        .collect(),
      None => Vec::new(),
    };
  }

  /// The text position nearest `point`, in window coordinates, and
  /// whether it is the end of a wrapped line rather than the start of the
  /// next.
  pub(crate) fn hit(&self, point: Point) -> (usize, bool) {
    let origin = self.text.origin;
    let Some(cursor) = self.text.buffer.hit(point.x - origin.x, point.y - origin.y) else {
      return (0, false);
    };
    let at = self.text_index(index_of(&self.text.buffer, cursor));
    (at, cursor.affinity == Affinity::Before)
  }

  /// What a key that moves by line does in a text area: Up and Down to
  /// the line above or below, keeping the caret's place across, PageUp
  /// and PageDown by `page` logical pixels, and Home and End to the ends
  /// of the line as it is wrapped. Shift extends the selection. `None`
  /// for any other key.
  pub(crate) fn line_key(&mut self, event: &KeyEvent, page: f32) -> Option<Keyed> {
    let ctrl = event.modifiers.ctrl;
    let lines = match event.key {
      Key::Up => -1.0,
      Key::Down => 1.0,
      Key::PageUp => -page,
      Key::PageDown => page,
      Key::Home | Key::End if !ctrl => 0.0,
      _ => return None,
    };
    let at = self.shown_index(self.edit.cursor);
    let upstream = self.upstream == Some(self.edit.cursor);
    let (target, upstream, goal) = if lines == 0.0 {
      let run = self.caret_run(at, upstream)?;
      let ends = run.glyphs.iter().map(|glyph| (glyph.start, glyph.end));
      let index = if event.key == Key::Home {
        ends.map(|(start, _)| start).min()
      } else {
        ends.map(|(_, end)| end).max()
      };
      let line = cosmic_text::Cursor::new(run.line_i, index.unwrap_or(0));
      (
        index_of(&self.text.buffer, line),
        event.key == Key::End,
        None,
      )
    } else {
      let (x, top, height) = self.caret_spot(at, upstream)?;
      let x = self
        .goal
        .filter(|&(cursor, _)| cursor == self.edit.cursor)
        .map_or(x, |(_, x)| x);
      let step = if matches!(event.key, Key::Up | Key::Down) {
        lines * height
      } else {
        lines
      };
      let cursor = self.text.buffer.hit(x, top + height / 2.0 + step)?;
      (
        index_of(&self.text.buffer, cursor),
        cursor.affinity == Affinity::Before,
        Some(x),
      )
    };
    let target = self.text_index(target);
    self.edit.move_to(event.modifiers.shift, target);
    self.upstream = upstream.then_some(target);
    self.goal = goal.map(|x| (target, x));
    Some(Keyed::Moved)
  }
}
