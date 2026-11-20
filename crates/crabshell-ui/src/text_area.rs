//! [`TextArea`], a multi-line text editor. It is a text field with line
//! breaks: the tree, the keys, the pointer, the clipboard and input
//! methods are the field's.

use std::rc::Rc;

use crabshell_reactive::Signal;

use crate::cx::Cx;
use crate::input::Handler;
use crate::prop::IntoProp;
use crate::style::Style;
use crate::text_input::InputProps;
use crate::widgets::{Common, Element, ElementKind, Widget, visibility_methods};

/// A multi-line text editor. It edits the text in a shared signal as a
/// [`TextInput`](crate::TextInput) does, and takes the focus the same
/// ways. Enter breaks the line, and Ctrl+Enter runs
/// [`on_accept`](Self::on_accept). Up and Down move by line, keeping the
/// caret's place across, Home and End go to the ends of the line as it
/// is wrapped, Ctrl+Home and Ctrl+End to the ends of the text, and
/// PageUp and PageDown by the height of the [`ScrollView`](crate::ScrollView)
/// around it, or 10 lines. Tab types a tab, drawn four spaces wide, and
/// Shift+Tab and Ctrl+Tab move the focus back and on.
///
/// The area is as tall as its lines, at least one. Put it in a
/// `ScrollView` to cap its height; when the caret leaves the view, the
/// view scrolls the least that shows it.
///
/// ```
/// use crabshell_ui::{Cx, ScrollView, Style, TextArea};
///
/// fn note(cx: &Cx) -> ScrollView {
///     let text = cx.signal(String::new());
///     ScrollView::new(TextArea::new(text).placeholder("Write a note"))
///         .style(Style::new().height(120.0))
/// }
/// ```
pub struct TextArea {
  input: InputProps,
  common: Common,
}

impl TextArea {
  /// An area editing `text`.
  pub fn new(text: Signal<String>) -> TextArea {
    TextArea {
      input: InputProps {
        text,
        placeholder: None,
        password: None,
        focus: None,
        on_accept: None,
        multiline: true,
        wrap: None,
        max_length: None,
        read_only: None,
      },
      common: Common::default(),
    }
  }

  /// Sets the text shown, dimmed, while the area is empty.
  pub fn placeholder(mut self, text: impl IntoProp<String>) -> TextArea {
    self.input.placeholder = Some(text.into_prop());
    self
  }

  /// Lets the area hold at most `max` characters, a line break counting
  /// as one, or any number with `None`, the default. Typing, pasting and
  /// an input method stop there, cutting what they bring between
  /// graphemes. Text set from outside, or already there when the limit
  /// drops, is cut to it, and the cut text is written back to the signal.
  pub fn max_length(mut self, max: impl IntoProp<Option<usize>>) -> TextArea {
    self.input.max_length = Some(max.into_prop());
    self
  }

  /// While on, off by default, the area takes no edits: typing, Enter
  /// and Tab, deleting, cutting, pasting and input methods leave it
  /// alone, and their keys go on to the window's `on_key`. The caret,
  /// selection and Ctrl+C still work, Ctrl+Enter still runs `on_accept`,
  /// and text set from outside still shows.
  pub fn read_only(mut self, on: impl IntoProp<bool>) -> TextArea {
    self.input.read_only = Some(on.into_prop());
    self
  }

  /// Takes the keyboard focus whenever this turns true.
  pub fn focus(mut self, focus: impl IntoProp<bool>) -> TextArea {
    self.input.focus = Some(focus.into_prop());
    self
  }

  /// When on, the default, lines wrap at words to the area's width, 200
  /// unless its style sets one. Off, each line is as long as it is and
  /// the area as wide as its longest, so a sideways `ScrollView` scrolls
  /// it.
  pub fn wrap(mut self, wrap: impl IntoProp<bool>) -> TextArea {
    self.input.wrap = Some(wrap.into_prop());
    self
  }

  /// Runs `handler` with the text when Ctrl+Enter is pressed in the area.
  pub fn on_accept(mut self, handler: impl Fn(&Cx, &str) + 'static) -> TextArea {
    self.input.on_accept = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Sets the style. An area cuts its text to the inside of its border.
  /// Its text takes the font, color, alignment, line height and
  /// decorations it inherits or sets, as a [`Text`](crate::Text) does.
  pub fn style(mut self, style: impl IntoProp<Style>) -> TextArea {
    self.common.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for TextArea {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "TextArea",
      common: self.common,
      kind: ElementKind::Input(Box::new(self.input)),
      children: Vec::new(),
    }
  }
}
