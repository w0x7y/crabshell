//! The widgets, and the [`Widget`] trait they share.
//!
//! A widget value is only a description. Nothing happens until its window is
//! created: then each description becomes a node in the window's tree, and
//! each closure property becomes an effect that updates that node.

use std::hash::Hash;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use crabshell_reactive::{Memo, Signal};

use crate::cx::Cx;
use crate::input::{Handler, Handlers};
use crate::keyboard::KeyEvent;
use crate::keyed::{self, KeyedList, KeyedMount};
use crate::prop::{IntoProp, Prop};
use crate::screencopy::ScreencopyProps;
use crate::scroll::ScrollProps;
use crate::style::Style;
use crate::text_input::InputProps;

/// Something that can be placed in a window: one of this crate's widgets, or
/// a type of your own that is made of them.
///
/// To make your own, return another widget's element. Calling
/// `into_element` on a container describes everything inside it too, so the
/// result is plain data:
///
/// ```
/// use crabshell_ui::{Element, Row, Text, Widget};
///
/// struct Greeting;
///
/// impl Widget for Greeting {
///     fn into_element(self: Box<Self>) -> Element {
///         Box::new(Row::new().gap(4).child(Text::new("hello")).child(Text::new("world")))
///             .into_element()
///     }
/// }
/// ```
pub trait Widget: 'static {
  /// Turns the description, and everything inside it, into the element
  /// the window builds nodes from. It runs when the window is created or a
  /// list item first appears, before the window's tree is touched and
  /// without recording what it reads, so it only describes.
  fn into_element(self: Box<Self>) -> Element;
}

/// What a [`Widget`] turns into: plain data, with every child already turned
/// into its element. Only this crate's widgets create one.
pub struct Element {
  pub(crate) name: &'static str,
  pub(crate) style: Option<Prop<Style>>,
  pub(crate) kind: ElementKind,
  pub(crate) children: Vec<ElementChild>,
}

pub(crate) enum ElementKind {
  Flex(FlexProps),
  Stack,
  Spacer,
  Text(TextProps),
  Icon(IconProps),
  Image(ImageProps),
  Capture(ScreencopyProps),
  // Boxed: a button's handlers make it the largest by far.
  Button(Box<ButtonProps>),
  Scroll(ScrollProps),
  Input(Box<InputProps>),
}

/// One entry of a container's child list: a single widget, or a keyed list
/// that grows and shrinks.
pub(crate) enum Child {
  One(Box<dyn Widget>),
  Keyed(KeyedList),
}

/// A [`Child`] with its widget turned into an element.
pub(crate) enum ElementChild {
  One(Box<Element>),
  Keyed(KeyedMount),
}

/// Turns every child widget into its element, which does the same for its
/// own children.
fn describe(children: Vec<Child>) -> Vec<ElementChild> {
  children
    .into_iter()
    .map(|child| match child {
      Child::One(widget) => ElementChild::One(Box::new(widget.into_element())),
      Child::Keyed(list) => ElementChild::Keyed(list.into_mount()),
    })
    .collect()
}

pub(crate) struct FlexProps {
  pub(crate) column: bool,
  /// Takes the room left in its parent, like a spacer. Only `Menu` sets
  /// it, so an entry's row fills its button.
  pub(crate) grow: bool,
  pub(crate) gap: Option<Prop<u32>>,
  pub(crate) padding_vertical: Option<Prop<u32>>,
  pub(crate) padding_horizontal: Option<Prop<u32>>,
  pub(crate) align: Option<Prop<Align>>,
  pub(crate) justify: Option<Prop<Justify>>,
}

pub(crate) struct TextProps {
  pub(crate) text: Prop<String>,
  pub(crate) ellipsis: Option<Prop<bool>>,
  pub(crate) max_lines: Option<Prop<u32>>,
}

pub(crate) struct IconProps {
  pub(crate) source: Prop<IconSource>,
  pub(crate) size: Option<Prop<u32>>,
}

pub(crate) struct ImageProps {
  pub(crate) source: Prop<PathBuf>,
  pub(crate) width: Option<Prop<u32>>,
  pub(crate) height: Option<Prop<u32>>,
  pub(crate) fit: Option<Prop<Fit>>,
}

/// How an [`Image`] fills its box when their shapes differ, as CSS's
/// `object-fit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fit {
  /// Stretched to the box. The default.
  #[default]
  Fill,
  /// As large as fits whole, centered, keeping its shape.
  Contain,
  /// Covering the whole box, centered, keeping its shape; what sticks out
  /// is cut off.
  Cover,
}

pub(crate) struct ButtonProps {
  pub(crate) hover_style: Option<Prop<Style>>,
  pub(crate) pressed_style: Option<Prop<Style>>,
  pub(crate) focus_style: Option<Prop<Style>>,
  pub(crate) hovered: Option<Signal<bool>>,
  pub(crate) handlers: Handlers,
}

/// How a [`Row`] or [`Column`] places children across its direction: up and
/// down in a row, left and right in a column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
  /// At the start: the top of a row, the left of a column.
  Start,
  /// In the middle.
  Center,
  /// At the end: the bottom of a row, the right of a column.
  End,
  /// Stretched to fill. The default, as in CSS.
  #[default]
  Stretch,
}

/// How a [`Row`] or [`Column`] places children along its direction when
/// they don't fill it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
  /// Packed at the start. The default.
  #[default]
  Start,
  /// Packed in the middle.
  Center,
  /// Packed at the end.
  End,
  /// The first child at the start, the last at the end, equal space between.
  SpaceBetween,
  /// Equal space around each child.
  SpaceAround,
  /// Equal space between children and at both ends.
  SpaceEvenly,
}

/// What an [`Icon`] shows.
#[derive(Clone, Debug, PartialEq)]
pub enum IconSource {
  /// A freedesktop icon name such as `"audio-volume-high"`, looked up in
  /// the icon theme.
  Name(String),
  /// An SVG or PNG file.
  File(PathBuf),
  /// Raw pixels, as a tray item sends them.
  Pixels(IconPixels),
}

/// Raw icon pixels: ARGB32 in network byte order (A, R, G, B bytes), row
/// by row, as the StatusNotifierItem spec sends them.
#[derive(Clone, Debug, PartialEq)]
pub struct IconPixels {
  /// Width in pixels.
  pub width: u32,
  /// Height in pixels.
  pub height: u32,
  /// `width * height * 4` bytes. Shared, so the same icon isn't copied.
  pub data: Rc<[u8]>,
}

/// The methods every container has: `child`, `children_keyed` and `exit`.
macro_rules! container_methods {
  () => {
    /// Adds `child` after the children added so far.
    pub fn child(mut self, child: impl Widget) -> Self {
      self.children.push(Child::One(Box::new(child)));
      self
    }

    /// Adds one child per item, after the children added so far.
    ///
    /// `items` runs in an effect, so the list updates when a signal it
    /// read changes. `key` names each item. `view` builds the child for
    /// a key the first time it appears, and that child is kept while
    /// its key stays in the list, even if it moves. `view` receives the
    /// item's [`Cx`] and a [`Memo`] that updates when that item's value
    /// changes. When a key leaves the list, its child and everything
    /// created in `view` are freed, at once or after an
    /// [`exit`](Self::exit). If two items share a key, only the first is
    /// shown and the duplicate is logged.
    pub fn children_keyed<T, K, W>(
      mut self,
      items: impl FnMut() -> Vec<T> + 'static,
      key: impl FnMut(&T) -> K + 'static,
      view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
    ) -> Self
    where
      T: Clone + PartialEq + 'static,
      K: Eq + Hash + 'static,
      W: Widget,
    {
      self
        .children
        .push(Child::Keyed(keyed::mount(items, key, view)));
      self
    }

    /// Lets the children of the last [`children_keyed`](Self::children_keyed)
    /// list added leave over `exit` instead of at once. A child whose key
    /// leaves stays mounted and painted right after the child that was
    /// before it, and its context's [`leaving`](Cx::leaving) turns `true`,
    /// so its own `cx.animate` can fade or shrink it out. After `exit` it
    /// is freed. If its key comes back first, the same child, with its
    /// state, takes the key's new place.
    pub fn exit(mut self, exit: Duration) -> Self {
      let last = self
        .children
        .iter_mut()
        .rev()
        .find_map(|child| match child {
          Child::Keyed(list) => Some(list),
          Child::One(_) => None,
        });
      match last {
        Some(list) => list.exit = Some(exit),
        None => tracing::warn!("`exit` was called before any `children_keyed`; it does nothing"),
      }
      self
    }
  };
}

/// The `style` method every widget has.
macro_rules! style_method {
  () => {
    /// Sets the widget's [`Style`].
    pub fn style(mut self, style: impl IntoProp<Style>) -> Self {
      self.style = Some(style.into_prop());
      self
    }
  };
}

/// Defines `Row` and `Column`, which differ only in direction.
macro_rules! flex_widget {
  ($name:ident, $column:literal, $doc:literal) => {
    #[doc = $doc]
    ///
    /// `gap`, `padding`, `align` and `justify` change the layout. Each
    /// takes a value or a closure.
    pub struct $name {
      flex: FlexProps,
      style: Option<Prop<Style>>,
      children: Vec<Child>,
    }

    impl $name {
      /// An empty container.
      pub fn new() -> Self {
        $name {
          flex: FlexProps {
            column: $column,
            grow: false,
            gap: None,
            padding_vertical: None,
            padding_horizontal: None,
            align: None,
            justify: None,
          },
          style: None,
          children: Vec::new(),
        }
      }

      /// Sets the space between children in logical pixels.
      pub fn gap(mut self, gap: impl IntoProp<u32>) -> Self {
        self.flex.gap = Some(gap.into_prop());
        self
      }

      /// Sets the padding: `vertical` above and below, `horizontal`
      /// left and right, in logical pixels. It wins over the padding
      /// of the container's [`Style`].
      pub fn padding(
        mut self,
        vertical: impl IntoProp<u32>,
        horizontal: impl IntoProp<u32>,
      ) -> Self {
        self.flex.padding_vertical = Some(vertical.into_prop());
        self.flex.padding_horizontal = Some(horizontal.into_prop());
        self
      }

      /// Sets how children are placed across the direction.
      pub fn align(mut self, align: impl IntoProp<Align>) -> Self {
        self.flex.align = Some(align.into_prop());
        self
      }

      /// Sets how children are placed along the direction.
      pub fn justify(mut self, justify: impl IntoProp<Justify>) -> Self {
        self.flex.justify = Some(justify.into_prop());
        self
      }

      container_methods!();
      style_method!();
    }

    impl Default for $name {
      fn default() -> Self {
        Self::new()
      }
    }

    impl Widget for $name {
      fn into_element(self: Box<Self>) -> Element {
        Element {
          name: stringify!($name),
          style: self.style,
          kind: ElementKind::Flex(self.flex),
          children: describe(self.children),
        }
      }
    }
  };
}

flex_widget!(
  Row,
  false,
  "Lays its children out left to right, like a CSS flexbox row."
);
flex_widget!(
  Column,
  true,
  "Lays its children out top to bottom, like a CSS flexbox column."
);

/// Draws its children on top of each other in one cell, each stretched to
/// the size of the largest. Later children are on top.
pub struct Stack {
  style: Option<Prop<Style>>,
  children: Vec<Child>,
}

impl Stack {
  /// An empty stack.
  pub fn new() -> Stack {
    Stack {
      style: None,
      children: Vec::new(),
    }
  }

  container_methods!();
  style_method!();
}

impl Default for Stack {
  fn default() -> Self {
    Self::new()
  }
}

impl Widget for Stack {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Stack",
      style: self.style,
      kind: ElementKind::Stack,
      children: describe(self.children),
    }
  }
}

/// Takes the space left over in a [`Row`] or [`Column`]. Several spacers
/// share it equally.
pub struct Spacer;

impl Spacer {
  /// A spacer.
  pub fn new() -> Spacer {
    Spacer
  }
}

impl Default for Spacer {
  fn default() -> Self {
    Self::new()
  }
}

impl Widget for Spacer {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Spacer",
      style: None,
      kind: ElementKind::Spacer,
      children: Vec::new(),
    }
  }
}

/// A line of text.
///
/// The text, `ellipsis` and `max_lines` change the layout. Color and font
/// come from the nearest widget whose [`Style`] sets them, starting with the
/// text's own.
pub struct Text {
  text: TextProps,
  style: Option<Prop<Style>>,
}

impl Text {
  /// Text showing `text`: a string, or a closure that returns a `String`.
  pub fn new(text: impl IntoProp<String>) -> Text {
    Text {
      text: TextProps {
        text: text.into_prop(),
        ellipsis: None,
        max_lines: None,
      },
      style: None,
    }
  }

  /// When on, text that doesn't fit ends in "…" instead of being cut off.
  /// Off by default.
  pub fn ellipsis(mut self, ellipsis: impl IntoProp<bool>) -> Text {
    self.text.ellipsis = Some(ellipsis.into_prop());
    self
  }

  /// Lets the text wrap onto at most this many lines, at word boundaries,
  /// or inside a word too long for a line. The default is 1, which never
  /// wraps. 0 counts as 1.
  pub fn max_lines(mut self, max_lines: impl IntoProp<u32>) -> Text {
    self.text.max_lines = Some(max_lines.into_prop());
    self
  }

  style_method!();
}

impl Widget for Text {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Text",
      style: self.style,
      kind: ElementKind::Text(self.text),
      children: Vec::new(),
    }
  }
}

/// An icon, drawn as a square.
pub struct Icon {
  icon: IconProps,
  style: Option<Prop<Style>>,
}

impl Icon {
  /// The default size in logical pixels.
  pub const DEFAULT_SIZE: u32 = 16;

  /// An icon showing `source`: an icon name (`&str`), a file (`&Path` or
  /// `PathBuf`), an [`IconSource`], or a closure that returns one.
  pub fn new(source: impl IntoProp<IconSource>) -> Icon {
    Icon {
      icon: IconProps {
        source: source.into_prop(),
        size: None,
      },
      style: None,
    }
  }

  /// Sets the width and height in logical pixels.
  pub fn size(mut self, size: impl IntoProp<u32>) -> Icon {
    self.icon.size = Some(size.into_prop());
    self
  }

  style_method!();
}

/// A picture from a PNG, JPEG, GIF (its first frame), WebP or SVG file,
/// drawn at a fixed size.
pub struct Image {
  image: ImageProps,
  style: Option<Prop<Style>>,
}

impl Image {
  /// The default width and height in logical pixels.
  pub const DEFAULT_SIZE: u32 = 64;

  /// An image of the file at `source`: a path (`&str`, `String`, `&Path`
  /// or `PathBuf`) or a closure that returns one.
  pub fn new(source: impl IntoProp<PathBuf>) -> Image {
    Image {
      image: ImageProps {
        source: source.into_prop(),
        width: None,
        height: None,
        fit: None,
      },
      style: None,
    }
  }

  /// Sets the width in logical pixels.
  pub fn width(mut self, width: impl IntoProp<u32>) -> Image {
    self.image.width = Some(width.into_prop());
    self
  }

  /// Sets the height in logical pixels.
  pub fn height(mut self, height: impl IntoProp<u32>) -> Image {
    self.image.height = Some(height.into_prop());
    self
  }

  /// Sets how the picture fills the image's box when their shapes differ.
  pub fn fit(mut self, fit: impl IntoProp<Fit>) -> Image {
    self.image.fit = Some(fit.into_prop());
    self
  }

  style_method!();
}

impl Widget for Image {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Image",
      style: self.style,
      kind: ElementKind::Image(self.image),
      children: Vec::new(),
    }
  }
}

impl Widget for Icon {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Icon",
      style: self.style,
      kind: ElementKind::Icon(self.icon),
      children: Vec::new(),
    }
  }
}

/// A clickable box around one child, which it centers.
///
/// While the pointer is over it, `hover_style` is laid over its style, and
/// while it is also held down, `pressed_style` on top of that. Handlers run
/// on the main thread and receive the button's [`Cx`]. A handler that panics
/// is logged and never called again.
pub struct Button {
  button: ButtonProps,
  style: Option<Prop<Style>>,
  child: Box<dyn Widget>,
}

impl Button {
  /// A button around `child`.
  pub fn new(child: impl Widget) -> Button {
    Button {
      child: Box::new(child),
      button: ButtonProps {
        hover_style: None,
        pressed_style: None,
        focus_style: None,
        hovered: None,
        handlers: Handlers::default(),
      },
      style: None,
    }
  }

  /// Sets the style laid over the normal one while the pointer is over the
  /// button.
  pub fn hover_style(mut self, style: impl IntoProp<Style>) -> Button {
    self.button.hover_style = Some(style.into_prop());
    self
  }

  /// Shares whether the pointer is over the button: the button sets
  /// `hovered` to `true` while the pointer is over it or anything inside
  /// it, buttons too, and back to `false` when it leaves, the window loses
  /// it, or the button goes. Unlike `hover_style`, which only the deepest
  /// button under the pointer shows, an outer button stays hovered while
  /// the pointer is over one inside it, so it can show controls on hover.
  pub fn hovered(mut self, hovered: Signal<bool>) -> Button {
    self.button.hovered = Some(hovered);
    self
  }

  /// Sets the style laid over the hover style while the button is held
  /// down.
  pub fn pressed_style(mut self, style: impl IntoProp<Style>) -> Button {
    self.button.pressed_style = Some(style.into_prop());
    self
  }

  /// Sets the style laid over the normal one while the button has the
  /// keyboard focus, which Tab moves between buttons.
  pub fn focus_style(mut self, style: impl IntoProp<Style>) -> Button {
    self.button.focus_style = Some(style.into_prop());
    self
  }

  /// Runs `handler` for each key pressed while the button has the
  /// keyboard focus. It returns `true` when it used the key; otherwise the
  /// window's `on_key` gets it, and then Tab, Enter and Space do what
  /// they do by default.
  pub fn on_key(mut self, handler: impl Fn(&Cx, &KeyEvent) -> bool + 'static) -> Button {
    self.button.handlers.key = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` as soon as the left button goes down over this
  /// button, while it is held: the time to call
  /// [`Cx::start_move`](crate::Cx::start_move), as a title bar does.
  pub fn on_press(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.press = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` when the left button is pressed and released over this
  /// button.
  pub fn on_click(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.click = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` when the right button is pressed and released over this
  /// button.
  pub fn on_right_click(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.right_click = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` when the middle button is pressed and released over
  /// this button.
  pub fn on_middle_click(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.middle_click = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` with whole scroll steps while the pointer is over this
  /// button: positive for up, negative for down. A mouse wheel gives one
  /// step per notch; touchpad scrolling gives one step per 15 logical
  /// pixels.
  pub fn on_scroll(mut self, handler: impl Fn(&Cx, i32) + 'static) -> Button {
    self.button.handlers.scroll = Some(Rc::new(Handler::new(handler)));
    self
  }

  style_method!();
}

impl Widget for Button {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Button",
      style: self.style,
      kind: ElementKind::Button(Box::new(self.button)),
      children: vec![ElementChild::One(Box::new(self.child.into_element()))],
    }
  }
}
