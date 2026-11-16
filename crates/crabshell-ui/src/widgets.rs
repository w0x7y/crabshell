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
use crate::geometry::{Point, Rect, Size};
use crate::image::ImageTransition;
use crate::input::{
  CursorShape, Drag, DragValue, DropTarget, Handler, Handlers, LinkHandler, Wheel,
};
use crate::keyboard::KeyEvent;
use crate::keyed::{self, KeyedList, KeyedMount};
use crate::prop::{IntoProp, Prop};
use crate::rich::Span;
use crate::screencopy::ScreencopyProps;
use crate::scroll::ScrollProps;
use crate::style::{Color, Style};
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
  pub(crate) common: Common,
  pub(crate) kind: ElementKind,
  pub(crate) children: Vec<ElementChild>,
}

pub(crate) enum ElementKind {
  Flex(FlexProps),
  Grid(GridProps),
  Stack,
  Spacer,
  Text(TextProps),
  Icon(IconProps),
  Image(ImageProps),
  Capture(ScreencopyProps),
  Canvas(Box<dyn FnMut(&mut crate::canvas::Pen)>),
  // Boxed: a button's handlers make it the largest by far.
  Button(Box<ButtonProps>),
  // Boxed: its scroll bar's style makes it large.
  Scroll(Box<ScrollProps>),
  Input(Box<InputProps>),
}

/// What every widget takes besides its own properties.
#[derive(Default)]
pub(crate) struct Common {
  pub(crate) style: Option<Prop<Style>>,
  pub(crate) visible: Option<Prop<bool>>,
  pub(crate) geometry: Option<Signal<Rect>>,
  /// Told the widget's size after each layout that changes it, with the
  /// tree released. Lists measure their rows with it; configs have
  /// `geometry`.
  pub(crate) measure: Option<Measure>,
}

/// What a list runs with a row's size after a layout changes it.
pub(crate) type Measure = Rc<dyn Fn(Size)>;

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
  pub(crate) wrap: Option<Prop<bool>>,
  pub(crate) reverse: Option<Prop<bool>>,
  /// Overrides `column` with a binding, for a list that turns.
  pub(crate) column_binding: Option<Prop<bool>>,
}

pub(crate) struct GridProps {
  pub(crate) columns: Option<Prop<u32>>,
  pub(crate) gap: Option<Prop<u32>>,
  pub(crate) row_gap: Option<Prop<u32>>,
  pub(crate) column_gap: Option<Prop<u32>>,
  pub(crate) padding_vertical: Option<Prop<u32>>,
  pub(crate) padding_horizontal: Option<Prop<u32>>,
}

pub(crate) struct TextProps {
  /// The plain text, or for a rich text `None` and its spans.
  pub(crate) text: Option<Prop<String>>,
  pub(crate) spans: Option<Prop<Vec<Span>>>,
  pub(crate) elide: Option<Prop<Elide>>,
  pub(crate) max_lines: Option<Prop<u32>>,
  pub(crate) selectable: Option<Prop<bool>>,
  pub(crate) on_link: Option<LinkHandler>,
}

pub(crate) struct IconProps {
  pub(crate) source: Prop<IconSource>,
  pub(crate) size: Option<Prop<u32>>,
  pub(crate) tint: Option<Prop<Option<Color>>>,
}

pub(crate) struct ImageProps {
  pub(crate) source: Prop<PathBuf>,
  pub(crate) width: Option<Prop<u32>>,
  pub(crate) height: Option<Prop<u32>>,
  pub(crate) fit: Option<Prop<Fit>>,
  pub(crate) tint: Option<Prop<Option<Color>>>,
  pub(crate) brightness: Option<Prop<f32>>,
  pub(crate) contrast: Option<Prop<f32>>,
  pub(crate) saturation: Option<Prop<f32>>,
  pub(crate) blur: Option<Prop<f32>>,
  pub(crate) playing: Option<Prop<bool>>,
  pub(crate) transition: Option<Prop<ImageTransition>>,
}

/// The filters an [`Image`] is drawn through, for the renderer. Each
/// number is finite and at least 0, and the default changes nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageFilters {
  /// The color every pixel is painted in, keeping its alpha.
  pub tint: Option<Color>,
  /// Scales each color channel, as CSS's `brightness()`.
  pub brightness: f32,
  /// Moves each channel from or toward the middle gray, as CSS's
  /// `contrast()`.
  pub contrast: f32,
  /// Moves each color from or toward its gray, as CSS's `saturate()`.
  pub saturation: f32,
  /// A Gaussian blur's standard deviation in logical pixels, as CSS's
  /// `blur()`.
  pub blur: f32,
}

impl Default for ImageFilters {
  fn default() -> ImageFilters {
    ImageFilters {
      tint: None,
      brightness: 1.0,
      contrast: 1.0,
      saturation: 1.0,
      blur: 0.0,
    }
  }
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
  pub(crate) position: Option<Signal<Option<Point>>>,
  pub(crate) enabled: Option<Prop<bool>>,
  pub(crate) focus: Option<Prop<bool>>,
  pub(crate) cursor: Option<Prop<CursorShape>>,
  pub(crate) drag_data: Option<Prop<Option<DragValue>>>,
  pub(crate) drop_hovered: Option<Signal<bool>>,
  pub(crate) handlers: Handlers,
}

/// How a [`Row`] or [`Column`] places children across its direction: up and
/// down in a row, left and right in a column. As a child's own
/// [`Style::align_self`] and [`Style::justify_self`], it also places the
/// child in a [`Stack`]'s or grid's cell: start is the top or the left.
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

/// The methods every container has: `child`, `children_keyed`, `exit`
/// and `displaced`.
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
    /// state, takes the key's new place. A closure is read as each key
    /// leaves, and an exit of 0 removes at once. Leaving children still
    /// take pointer and keyboard input; a handler can check
    /// [`cx.leaving()`](Cx::leaving) to ignore clicks during the exit.
    pub fn exit(mut self, exit: impl IntoProp<Duration>) -> Self {
      let last = self
        .children
        .iter_mut()
        .rev()
        .find_map(|child| match child {
          Child::Keyed(list) => Some(list),
          Child::One(_) => None,
        });
      match last {
        Some(list) => list.exit = Some(exit.into_prop()),
        None => tracing::warn!("`exit` was called before any `children_keyed`; it does nothing"),
      }
      self
    }

    /// Slides the children that a change of the last
    /// [`children_keyed`](Self::children_keyed) list moves, instead of
    /// jumping: when its keys are added, leave (once their
    /// [`exit`](Self::exit) ends) or move, each child of this container
    /// whose place changes goes from where it was drawn to its new place
    /// over `transition`. It moves where it is drawn and hit, as
    /// [`Style::translate`](crate::Style::translate) does, so a slide never
    /// lays the window out again. A child moved again mid-slide starts from
    /// where it is drawn. Other layout changes, such as a resize or text
    /// growing, still move children at once, and a new child shows in its
    /// place. A closure is read at each change of the list, and a
    /// transition of 0 jumps.
    pub fn displaced(mut self, transition: impl IntoProp<crate::Transition>) -> Self {
      let last = self
        .children
        .iter_mut()
        .rev()
        .find_map(|child| match child {
          Child::Keyed(list) => Some(list),
          Child::One(_) => None,
        });
      match last {
        Some(list) => list.displaced = Some(transition.into_prop()),
        None => {
          tracing::warn!("`displaced` was called before any `children_keyed`; it does nothing")
        }
      }
      self
    }
  };
}

/// The `visible` and `geometry` methods every widget has.
macro_rules! visibility_methods {
  () => {
    /// Shows or hides the widget. Hidden, it is taken out of the layout,
    /// so its space goes to the others, and it isn't drawn, hit or
    /// reached by Tab; it drops the keyboard focus and the pointer. Its
    /// bindings and state stay, so it shows again as it was. Shown by
    /// default.
    pub fn visible(mut self, visible: impl $crate::IntoProp<bool>) -> Self {
      self.common.visible = Some(visible.into_prop());
      self
    }

    /// Shares where the widget is: after each layout that moves or
    /// resizes it, its laid-out rectangle in the window's logical
    /// pixels, scrolled with any scroll view around it but not moved by
    /// a style's translate, scale or rotate, is written to `geometry`. A
    /// hidden widget writes nothing. An effect that reads it may change
    /// the layout: the window is laid out again at once, until nothing
    /// moves.
    pub fn geometry(mut self, geometry: ::crabshell_reactive::Signal<$crate::Rect>) -> Self {
      self.common.geometry = Some(geometry);
      self
    }
  };
}

/// The `style` method every widget has, with `visible` and `geometry`.
macro_rules! style_method {
  () => {
    /// Sets the widget's [`Style`](crate::Style).
    pub fn style(mut self, style: impl $crate::IntoProp<$crate::Style>) -> Self {
      self.common.style = Some(style.into_prop());
      self
    }

    $crate::widgets::visibility_methods!();
  };
}

pub(crate) use visibility_methods;

/// Defines `Row` and `Column`, which differ only in direction.
macro_rules! flex_widget {
  ($name:ident, $column:literal, $doc:literal) => {
    #[doc = $doc]
    ///
    /// `gap`, `padding`, `align`, `justify`, `wrap` and `reverse` change
    /// the layout. Each takes a value or a closure.
    pub struct $name {
      flex: FlexProps,
      common: Common,
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
            wrap: None,
            reverse: None,
            column_binding: None,
          },
          common: Common::default(),
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

      /// When on, children that don't fit start a new line, `gap` apart
      /// like the children, and the lines pack at the start. Off by
      /// default.
      pub fn wrap(mut self, wrap: impl IntoProp<bool>) -> Self {
        self.flex.wrap = Some(wrap.into_prop());
        self
      }

      /// When on, a row runs right to left and a column bottom to top, so
      /// [`Justify::Start`] packs children at the right or the bottom.
      /// Tab still moves through them in child order. Off by default.
      pub fn reverse(mut self, reverse: impl IntoProp<bool>) -> Self {
        self.flex.reverse = Some(reverse.into_prop());
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
          common: self.common,
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

/// Places its children row by row into equal-width columns, each row as
/// tall as its tallest child. A child takes several cells with
/// [`Style::span`], and places itself in its cell with
/// [`Style::align_self`] and [`Style::justify_self`]; it fills the cell by
/// default.
///
/// `columns`, `gap`, `row_gap`, `column_gap` and `padding` change the
/// layout. Each takes a value or a closure.
///
/// ```
/// use crabshell_ui::{Grid, Style, Text};
///
/// let days = (1..=31).fold(Grid::new().columns(7).gap(4), |grid, day| {
///     grid.child(Text::new(day.to_string()))
/// });
/// let wide = Grid::new()
///     .columns(2)
///     .child(Text::new("title").style(Style::new().span(2, 1)));
/// ```
pub struct Grid {
  grid: GridProps,
  common: Common,
  children: Vec<Child>,
}

impl Grid {
  /// An empty grid of one column.
  pub fn new() -> Grid {
    Grid {
      grid: GridProps {
        columns: None,
        gap: None,
        row_gap: None,
        column_gap: None,
        padding_vertical: None,
        padding_horizontal: None,
      },
      common: Common::default(),
      children: Vec::new(),
    }
  }

  /// Sets the number of columns, all as wide. 1 by default, and 0
  /// counts as 1.
  pub fn columns(mut self, columns: impl IntoProp<u32>) -> Grid {
    self.grid.columns = Some(columns.into_prop());
    self
  }

  /// Sets the space between rows and between columns in logical pixels.
  /// `row_gap` and `column_gap` win over it.
  pub fn gap(mut self, gap: impl IntoProp<u32>) -> Grid {
    self.grid.gap = Some(gap.into_prop());
    self
  }

  /// Sets the space between rows in logical pixels.
  pub fn row_gap(mut self, gap: impl IntoProp<u32>) -> Grid {
    self.grid.row_gap = Some(gap.into_prop());
    self
  }

  /// Sets the space between columns in logical pixels.
  pub fn column_gap(mut self, gap: impl IntoProp<u32>) -> Grid {
    self.grid.column_gap = Some(gap.into_prop());
    self
  }

  /// Sets the padding: `vertical` above and below, `horizontal` left and
  /// right, in logical pixels. It wins over the padding of the grid's
  /// [`Style`], as a [`Row`]'s does.
  pub fn padding(mut self, vertical: impl IntoProp<u32>, horizontal: impl IntoProp<u32>) -> Grid {
    self.grid.padding_vertical = Some(vertical.into_prop());
    self.grid.padding_horizontal = Some(horizontal.into_prop());
    self
  }

  container_methods!();
  style_method!();
}

impl Default for Grid {
  fn default() -> Self {
    Self::new()
  }
}

impl Widget for Grid {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Grid",
      common: self.common,
      kind: ElementKind::Grid(self.grid),
      children: describe(self.children),
    }
  }
}

/// Draws its children on top of each other in one cell, each stretched to
/// the size of the largest. Later children are on top.
pub struct Stack {
  common: Common,
  children: Vec<Child>,
}

impl Stack {
  /// An empty stack.
  pub fn new() -> Stack {
    Stack {
      common: Common::default(),
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
      common: self.common,
      kind: ElementKind::Stack,
      children: describe(self.children),
    }
  }
}

/// Takes the space left over in a [`Row`] or [`Column`]. Several spacers
/// share it equally, or by their styles' [`grow`](Style::grow).
pub struct Spacer {
  common: Common,
}

impl Spacer {
  /// A spacer.
  pub fn new() -> Spacer {
    Spacer {
      common: Common::default(),
    }
  }

  style_method!();
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
      common: self.common,
      kind: ElementKind::Spacer,
      children: Vec::new(),
    }
  }
}

/// Where text that doesn't fit is cut, with "…" in its place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Elide {
  /// Not cut: the text keeps its width and overflows. The default.
  #[default]
  None,
  /// The start of the last line shown.
  Start,
  /// The middle of the last line shown.
  Middle,
  /// The end of the last line shown.
  End,
}

/// A line of text.
///
/// The text, `elide` and `max_lines` change the layout. Color, font,
/// alignment, line height and decorations come from the nearest widget
/// whose [`Style`] sets each, starting with the text's own.
pub struct Text {
  text: TextProps,
  common: Common,
}

impl Text {
  /// Text showing `text`: a string, or a closure that returns a `String`.
  pub fn new(text: impl IntoProp<String>) -> Text {
    Text {
      text: TextProps {
        text: Some(text.into_prop()),
        spans: None,
        elide: None,
        max_lines: None,
        selectable: None,
        on_link: None,
      },
      common: Common::default(),
    }
  }

  /// Text made of styled spans: a `Vec<Span>`, or a closure that returns
  /// one. Each span takes what it doesn't set from the text, and
  /// everything else a `Text` takes works the same.
  pub fn rich(spans: impl IntoProp<Vec<Span>>) -> Text {
    Text {
      text: TextProps {
        text: None,
        spans: Some(spans.into_prop()),
        elide: None,
        max_lines: None,
        selectable: None,
        on_link: None,
      },
      common: Common::default(),
    }
  }

  /// Runs `handler` with a link span's URL when a left click lands on
  /// the span's glyphs. Over a link the cursor is the pointing hand and
  /// the click goes to nothing else, not even a button around the text;
  /// elsewhere the text lets the pointer through as before. In a
  /// [`selectable`](Self::selectable) text, a press and release on one
  /// link that selected nothing opens it, and a drag selects. A handler
  /// that panics is logged and disabled, and its links are plain text.
  pub fn on_link(mut self, handler: impl Fn(&Cx, &str) + 'static) -> Text {
    self.text.on_link = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// When on, the text takes the pointer with an I-beam: a press, a drag,
  /// a double click and Shift+click select it, as in a text field, and a
  /// press takes the keyboard focus, so Ctrl+C copies the selection.
  /// Inside a [`Button`], it takes the press, so the button doesn't
  /// click. Tab doesn't stop on it. Off by default.
  pub fn selectable(mut self, selectable: impl IntoProp<bool>) -> Text {
    self.text.selectable = Some(selectable.into_prop());
    self
  }

  /// When on, text that doesn't fit ends in "…" instead of being cut off.
  /// Off by default. The same as [`elide`](Self::elide) with
  /// [`Elide::End`] or [`Elide::None`]; whichever is called last wins.
  pub fn ellipsis(mut self, ellipsis: impl IntoProp<bool>) -> Text {
    let elide = ellipsis
      .into_prop()
      .map(|on| if on { Elide::End } else { Elide::None });
    self.text.elide = Some(elide);
    self
  }

  /// Where text that doesn't fit is cut, with "…" in its place:
  /// [`Elide::None`] by default. With `max_lines` above 1, the last line
  /// shown is cut.
  pub fn elide(mut self, elide: impl IntoProp<Elide>) -> Text {
    self.text.elide = Some(elide.into_prop());
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
      common: self.common,
      kind: ElementKind::Text(self.text),
      children: Vec::new(),
    }
  }
}

/// An icon, drawn as a square.
pub struct Icon {
  icon: IconProps,
  common: Common,
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
        tint: None,
      },
      common: Common::default(),
    }
  }

  /// Sets the width and height in logical pixels.
  pub fn size(mut self, size: impl IntoProp<u32>) -> Icon {
    self.icon.size = Some(size.into_prop());
    self
  }

  /// Paints the icon's pixels in `tint`, keeping their alpha, as a
  /// symbolic icon's color follows the theme. `None`, the default, draws
  /// it as it is.
  pub fn tint(mut self, tint: impl IntoProp<Option<Color>>) -> Icon {
    self.icon.tint = Some(tint.into_prop());
    self
  }

  style_method!();
}

/// A picture from a PNG, JPEG, GIF, WebP or SVG file, drawn at a fixed
/// size.
///
/// A GIF of more than one frame plays while it is drawn, at its own
/// delays, looping as the file says. The filters run once on the picture
/// at the size drawn, and the result is cached, so a filtered image costs
/// what a plain one does to draw.
pub struct Image {
  image: ImageProps,
  common: Common,
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
        tint: None,
        brightness: None,
        contrast: None,
        saturation: None,
        blur: None,
        playing: None,
        transition: None,
      },
      common: Common::default(),
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

  /// Paints the picture's pixels in `tint`, keeping their alpha, as
  /// [`Icon::tint`] does. `None`, the default, draws it as it is.
  pub fn tint(mut self, tint: impl IntoProp<Option<Color>>) -> Image {
    self.image.tint = Some(tint.into_prop());
    self
  }

  /// Scales the colors, as CSS's `brightness()`: 1, the default, leaves
  /// them, 0 is black and 2 twice as bright. A number that isn't finite
  /// counts as 1, and one below 0 as 0.
  pub fn brightness(mut self, brightness: impl IntoProp<f32>) -> Image {
    self.image.brightness = Some(brightness.into_prop());
    self
  }

  /// Sets the contrast, as CSS's `contrast()`: 1, the default, leaves the
  /// colors, 0 is middle gray and more than 1 is stronger. A number that
  /// isn't finite counts as 1, and one below 0 as 0.
  pub fn contrast(mut self, contrast: impl IntoProp<f32>) -> Image {
    self.image.contrast = Some(contrast.into_prop());
    self
  }

  /// Sets the saturation, as CSS's `saturate()`: 1, the default, leaves
  /// the colors, 0 is gray and more than 1 is more vivid. A number that
  /// isn't finite counts as 1, and one below 0 as 0.
  pub fn saturation(mut self, saturation: impl IntoProp<f32>) -> Image {
    self.image.saturation = Some(saturation.into_prop());
    self
  }

  /// Blurs the picture, as CSS's `blur()`: the Gaussian's standard
  /// deviation in logical pixels, 0 by default. Pixels past the picture's
  /// edge count as its edge pixels, so a blurred picture keeps its edges
  /// opaque. A number that isn't finite or is below 0 counts as 0. The
  /// blur is for still pictures, such as album art and wallpapers: a
  /// blurred GIF shows its first frame, still.
  pub fn blur(mut self, blur: impl IntoProp<f32>) -> Image {
    self.image.blur = Some(blur.into_prop());
    self
  }

  /// Whether an animated GIF plays, true by default. False pauses it on
  /// the frame it shows.
  pub fn playing(mut self, playing: impl IntoProp<bool>) -> Image {
    self.image.playing = Some(playing.into_prop());
    self
  }

  /// How a new source comes in: [`ImageTransition::None`], the default,
  /// shows it at once; the others bring it in over the old picture on the
  /// animation clock, repainting only the image's box. A new source
  /// mid-transition starts again from the picture most of the box shows:
  /// the one leaving before halfway, the one coming in from halfway on.
  /// A new source that can't be loaded keeps the old picture; the clock
  /// still runs and repaints the box for the transition's length, since
  /// only the renderer learns the file is bad.
  pub fn transition(mut self, transition: impl IntoProp<ImageTransition>) -> Image {
    self.image.transition = Some(transition.into_prop());
    self
  }

  style_method!();
}

impl Widget for Image {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Image",
      common: self.common,
      kind: ElementKind::Image(self.image),
      children: Vec::new(),
    }
  }
}

impl Widget for Icon {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Icon",
      common: self.common,
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
  common: Common,
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
        position: None,
        enabled: None,
        focus: None,
        cursor: None,
        drag_data: None,
        drop_hovered: None,
        handlers: Handlers::default(),
      },
      common: Common::default(),
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
  /// A drag of this button or one inside it keeps it hovered until the
  /// release.
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

  /// Runs `handler` when the left button comes up after going down on
  /// this button, wherever the pointer is by then, before `on_click`.
  pub fn on_release(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.release = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` when the left button is pressed and released over this
  /// button, unless that ended a drag or a long press, or made a double
  /// click.
  pub fn on_click(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.click = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` instead of a second `on_click` when a click comes
  /// within 400 ms and 5 logical pixels of the one before, as Qt's
  /// defaults say. A third click starts over. Without it, every click
  /// runs `on_click`.
  pub fn on_double_click(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.double_click = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` once the left button has been held on this button for
  /// 800 ms without moving 10 logical pixels, as Qt's press and hold. The
  /// release after it doesn't click, though moving on can still drag.
  pub fn on_long_press(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.long_press = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` as the left button drags this button: a
  /// [`DragPhase::Start`](crate::DragPhase::Start) once it moves 10
  /// logical pixels from where it went down, a `Move` for each motion
  /// after, and an `End` when it comes up, the pointer leaves the window,
  /// or the button is disabled. A button with no `on_click`,
  /// `on_double_click` or `on_long_press` starts at the press instead, so
  /// a slider moves from the first touch.
  ///
  /// While it drags, the button gets every motion, even outside itself or
  /// the window, stays hovered, and doesn't click.
  pub fn on_drag(mut self, handler: impl Fn(&Cx, Drag) + 'static) -> Button {
    self.button.handlers.drag = Some(Rc::new(Handler::new(handler)));
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

  /// Runs `handler` when the mouse's back side button is pressed and
  /// released over this button.
  pub fn on_back(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.back = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` when the mouse's forward side button is pressed and
  /// released over this button.
  pub fn on_forward(mut self, handler: impl Fn(&Cx) + 'static) -> Button {
    self.button.handlers.forward = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` with whole scroll steps while the pointer is over this
  /// button: positive for up, negative for down. A mouse wheel gives one
  /// step per notch; touchpad scrolling gives one step per 15 logical
  /// pixels. Sideways scrolling gives none.
  pub fn on_scroll(mut self, handler: impl Fn(&Cx, i32) + 'static) -> Button {
    self.button.handlers.scroll = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Runs `handler` with every scroll event while the pointer is over
  /// this button, on both axes: a wheel's notches, a touchpad's pixels,
  /// where it came from, and the `stop` when the fingers lift. Positive
  /// is down and right. With `on_scroll` too, both run.
  pub fn on_wheel(mut self, handler: impl Fn(&Cx, Wheel) + 'static) -> Button {
    self.button.handlers.wheel = Some(Rc::new(Handler::new(handler)));
    self
  }

  /// Shares where the pointer is: the button writes `position`, in its
  /// own coordinates with (0, 0) at its top-left, while the pointer is
  /// over it or anything inside it, and `None` when it leaves. While a
  /// button inside it, or this one, is dragged, it follows the pointer
  /// wherever it goes, even outside the button. It is written only on a
  /// change.
  pub fn pointer_position(mut self, position: Signal<Option<Point>>) -> Button {
    self.button.position = Some(position);
    self
  }

  /// Turns the button on or off; it is on by default. Off, none of its
  /// handlers run, it shows no hover, pressed or focus style and the
  /// default cursor, and Tab skips it. The pointer passes through it to
  /// the button around it, as through a disabled Qt `MouseArea`. Turning
  /// it off while it is held lets go, ending a drag with its `End`, and
  /// drops its focus.
  pub fn enabled(mut self, enabled: impl IntoProp<bool>) -> Button {
    self.button.enabled = Some(enabled.into_prop());
    self
  }

  /// Takes the keyboard focus whenever this turns true, as
  /// [`TextInput::focus`](crate::TextInput::focus) does. Tab and a field
  /// may take it away again.
  pub fn focus(mut self, focus: impl IntoProp<bool>) -> Button {
    self.button.focus = Some(focus.into_prop());
    self
  }

  /// Makes a drag of this button carry `data`, which a button whose
  /// [`on_drop`](Self::on_drop) takes a `T` is given when the drag is
  /// released over it. A closure is read as it changes, and a drop gets
  /// the value it holds at the release. While it holds a value, the button
  /// drags as one with [`on_drag`](Self::on_drag) does, and its `on_drag`
  /// still runs, so it can follow the pointer with a translate; its `End`
  /// says whether it was dropped. While it holds `None`, the button acts as
  /// if it had no data: it clicks, and drags only with its own `on_drag`,
  /// so `editing.get().then_some(id)` drags only in an edit mode. A drag
  /// carries data only within its window.
  pub fn drag_data<T: Clone + 'static>(mut self, data: impl IntoProp<Option<T>>) -> Button {
    let data = data
      .into_prop()
      .map(|data| data.map(|value| Rc::new(value) as DragValue));
    self.button.drag_data = Some(data);
    self
  }

  /// Runs `handler` with a copy of the [`drag_data`](Self::drag_data) of a
  /// drag released over this button, or over anything inside it, when
  /// that data is a `T`. Data of any other type passes over it, as if it
  /// weren't there. The deepest button under the pointer that takes it is
  /// the one dropped on, and the dragged button never takes its own.
  pub fn on_drop<T: Clone + 'static>(mut self, handler: impl Fn(&Cx, T) + 'static) -> Button {
    let take = move |cx: &Cx, value: &dyn std::any::Any| {
      if let Some(value) = value.downcast_ref::<T>() {
        handler(cx, value.clone());
      }
    };
    self.button.handlers.drop = Some(DropTarget {
      accepts: std::any::TypeId::of::<T>(),
      handler: Rc::new(Handler::new(take)),
    });
    self
  }

  /// Shares whether a drop is waiting: the button writes `hovered` true
  /// while a drag whose data its [`on_drop`](Self::on_drop) would take is
  /// over it, as Qt's `DropArea.containsDrag`, and false when the drag
  /// leaves, ends, or either button goes, is hidden or is turned off.
  pub fn drop_hovered(mut self, hovered: Signal<bool>) -> Button {
    self.button.drop_hovered = Some(hovered);
    self
  }

  /// Sets the cursor shown over the button, instead of the pointing hand.
  pub fn cursor(mut self, cursor: impl IntoProp<CursorShape>) -> Button {
    self.button.cursor = Some(cursor.into_prop());
    self
  }

  style_method!();
}

impl Widget for Button {
  fn into_element(self: Box<Self>) -> Element {
    Element {
      name: "Button",
      common: self.common,
      kind: ElementKind::Button(Box::new(self.button)),
      children: vec![ElementChild::One(Box::new(self.child.into_element()))],
    }
  }
}
