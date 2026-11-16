//! A read-only view of a laid-out window, for drawing it.

use std::cell::Ref;
use std::path::Path;

use cosmic_text::{Buffer, LayoutRun};

use crate::canvas::CanvasOp;
use crate::geometry::{Affine, Point, Rect, Size};
use crate::image::ImageTransition;
use crate::screencopy::CaptureFrame;
use crate::select::{highlights, text_selection};
use crate::style::{Color, Style};
use crate::text::TextNode;
use crate::tree::{Node, NodeId, NodeKind, Tree, centered, icon_rect};
use crate::widgets::{Fit, IconSource, ImageFilters};

/// The color text uses when no ancestor sets one.
const DEFAULT_TEXT_COLOR: Color = Color::rgb(0, 0, 0);

/// A window's laid-out tree, borrowed for reading. Get one from
/// [`Window::view`](crate::Window::view).
pub struct WindowView<'w> {
  tree: Ref<'w, Tree>,
}

impl<'w> WindowView<'w> {
  pub(crate) fn new(tree: Ref<'w, Tree>) -> WindowView<'w> {
    WindowView { tree }
  }

  /// The window's size in logical pixels.
  pub fn size(&self) -> Size {
    self.tree.size
  }

  /// The window's root node, which has the window's style and its child.
  pub fn root(&self) -> NodeView<'_> {
    NodeView::new(&self.tree, self.tree.root)
      .expect("invariant: the root node exists while the tree does")
  }
}

/// One node of a laid-out window: a widget, or the window itself at the
/// root. Draw it, then its [`children`](Self::children) in order, so later
/// ones are on top.
#[derive(Clone, Copy)]
pub struct NodeView<'a> {
  tree: &'a Tree,
  id: NodeId,
  node: &'a Node,
}

impl<'a> NodeView<'a> {
  fn new(tree: &'a Tree, id: NodeId) -> Option<NodeView<'a>> {
    let node = tree.node(id)?;
    Some(NodeView { tree, id, node })
  }

  /// The widget type, such as `"Row"`, or `"PanelWindow"` at the root.
  pub fn name(&self) -> &'static str {
    self.node.name
  }

  /// The widget path, such as `PanelWindow[DP-1]/Row/Button[2]`.
  pub fn path(&self) -> &'a str {
    &self.node.path
  }

  /// The rectangle from the last layout, in window coordinates. Empty
  /// before the first layout.
  pub fn rect(&self) -> Rect {
    self.node.rect.unwrap_or_default()
  }

  /// The area the node and everything inside it painted at the last
  /// layout: its rectangle, text and icons that overflow it, and its
  /// descendants' bounds, moved by its [`transform`](Self::transform) into
  /// its parent's coordinates. Nothing outside it needs drawing for this
  /// node.
  pub fn bounds(&self) -> Rect {
    self.node.bounds
  }

  /// [`bounds`](Self::bounds) before the node's own transform, in its own
  /// coordinates: what a layer drawn for the transform must hold. The
  /// same as `bounds` when it has none.
  pub fn inner_bounds(&self) -> Rect {
    self.node.inner_bounds
  }

  /// How the node's style moves it and everything inside it where it is
  /// drawn, from its own coordinates, which its rectangle and its
  /// children's are in, into its parent's. `None` when it doesn't. The
  /// whole subtree is drawn as one layer through it.
  pub fn transform(&self) -> Option<Affine> {
    self.node.transform
  }

  /// The style in effect, with a button's hover and pressed styles laid
  /// over its own. Inherited fields aren't filled in;
  /// [`TextView::color`] has the color text is drawn in.
  pub fn style(&self) -> Style {
    self.node.shown_style()
  }

  /// The children, in drawing order: by their style's `z`, lowest
  /// first, and in child order where it is equal.
  pub fn children(&self) -> impl Iterator<Item = NodeView<'a>> + 'a {
    let tree = self.tree;
    tree
      .drawn_children(self.id)
      .into_iter()
      .filter_map(move |id| NodeView::new(tree, id))
  }

  /// Whether the node cuts its children to the inside of its border: a
  /// clipping style or a scroll view.
  pub fn clips(&self) -> bool {
    self.node.clips()
  }

  /// A scroll view's scroll bar handle and the style to draw it with,
  /// over its children, while it shows one.
  pub fn scroll_bar(&self) -> Option<(Rect, Style)> {
    match &self.node.kind {
      NodeKind::Scroll(scroll) => Some((scroll.handle?.1, scroll.bar?)),
      _ => None,
    }
  }

  /// What the node draws besides its background and border.
  pub fn content(&self) -> Content<'a> {
    match &self.node.kind {
      NodeKind::Text(text) => Content::Text(TextView {
        text,
        color: self.inherited_color(),
        underline: self.inherited(|style| style.underline) == Some(true),
        strikethrough: self.inherited(|style| style.strikethrough) == Some(true),
      }),
      NodeKind::Icon(icon) => match &icon.source {
        Some(source) => Content::Icon(IconView {
          source,
          rect: icon_rect(self.node.content, icon.size),
          tint: icon.tint,
        }),
        None => Content::None,
      },
      NodeKind::Input(input) => {
        let color = self.inherited_color();
        let color = if input.showing_placeholder() {
          Color::rgba(color.r, color.g, color.b, color.a / 2)
        } else {
          color
        };
        Content::Input(InputView {
          text: TextView {
            text: &input.text,
            color,
            underline: false,
            strikethrough: false,
          },
          // An input method may ask for no caret while it composes.
          caret: (input.focused
            && !matches!(&input.preedit, Some(preedit) if preedit.cursor.is_none()))
          .then_some(input.caret),
          selection: if input.focused { &input.selection } else { &[] },
          area: input.area,
          underline: &input.underline,
        })
      }
      NodeKind::Image(image) => match &image.source {
        Some(path) => Content::Image(ImageView {
          id: image.id,
          path,
          rect: centered(self.node.content, image.width, image.height),
          fit: image.fit,
          filters: image.filters,
          playing: image.playing,
          transition: image.transition,
          leaving: image
            .leaving
            .as_ref()
            .map(|leaving| (leaving.source.as_path(), leaving.t)),
        }),
        None => Content::None,
      },
      NodeKind::Capture(capture) => match &capture.frame {
        Some(frame) => Content::Capture(CaptureView {
          id: capture.id,
          frame,
          rect: self.node.content,
          fit: capture.fit,
        }),
        None => Content::None,
      },
      NodeKind::Canvas(canvas) => Content::Canvas(CanvasView {
        ops: &canvas.ops,
        rect: self.node.rect.unwrap_or_default(),
      }),
      _ => Content::None,
    }
  }

  /// The color of the nearest node, starting with this one, whose style
  /// sets one.
  fn inherited_color(&self) -> Color {
    self
      .inherited(|style| style.color)
      .unwrap_or(DEFAULT_TEXT_COLOR)
  }

  /// `field` of the nearest node, starting with this one, whose style
  /// sets it.
  fn inherited<T>(&self, field: impl Fn(&Style) -> Option<T>) -> Option<T> {
    let mut id = Some(self.id);
    while let Some(current) = id {
      if let Some(value) = self
        .tree
        .node(current)
        .and_then(|node| field(&node.shown_style()))
      {
        return Some(value);
      }
      id = self.tree.taffy.parent(current);
    }
    None
  }
}

/// What a node draws besides its background and border.
pub enum Content<'a> {
  /// Nothing.
  None,
  /// Text.
  Text(TextView<'a>),
  /// An icon.
  Icon(IconView<'a>),
  /// An image.
  Image(ImageView<'a>),
  /// A screencopy view's frame.
  Capture(CaptureView<'a>),
  /// A text field.
  Input(InputView<'a>),
  /// A canvas's drawing.
  Canvas(CanvasView<'a>),
}

/// A canvas's drawing, ready to draw: each op in order, in logical pixels
/// from the corner of `rect`, cut to it and its style's rounded corners.
pub struct CanvasView<'a> {
  /// What the paint closure drew at its last run that didn't panic.
  pub ops: &'a [CanvasOp],
  /// Where: the canvas's rectangle.
  pub rect: Rect,
}

/// A text field, ready to draw: the selection behind the text, then the
/// text, then the caret.
pub struct InputView<'a> {
  /// What the field shows; a placeholder comes in half its color.
  pub text: TextView<'a>,
  /// The caret, while the field has the keyboard.
  pub caret: Option<Rect>,
  /// The selection, while the field has the keyboard: one span, or more
  /// where right-to-left and left-to-right text mix.
  pub selection: &'a [Rect],
  /// The content box everything is cut to.
  pub area: Rect,
  /// The line under text an input method is composing.
  pub underline: &'a [Rect],
}

/// Laid-out text, ready to draw. Lines can stick out of the node's
/// rectangle when it is too small for them; draw them unclipped, as CSS
/// does. The window's damage covers them.
pub struct TextView<'a> {
  text: &'a TextNode,
  color: Color,
  underline: bool,
  strikethrough: bool,
}

impl<'a> TextView<'a> {
  /// The text itself.
  pub fn content(&self) -> &'a str {
    &self.text.content
  }

  /// The cosmic-text buffer layout measured, for drawing glyphs from.
  pub fn buffer(&self) -> &'a Buffer {
    &self.text.buffer
  }

  /// The lines to draw: the buffer's lines, at most `max_lines`. Glyph
  /// positions are relative to [`origin`](Self::origin).
  pub fn runs(&self) -> impl Iterator<Item = LayoutRun<'a>> + 'a {
    self.text.runs()
  }

  /// The top-left corner of the first line, in window coordinates.
  pub fn origin(&self) -> Point {
    self.text.origin
  }

  /// The color to draw in, inherited if the text's own style doesn't set
  /// one.
  pub fn color(&self) -> Color {
    self.color
  }

  /// Whether to draw a line under the glyphs, in the text's color.
  pub fn underline(&self) -> bool {
    self.underline
  }

  /// Whether to draw a line through the glyphs, in the text's color.
  pub fn strikethrough(&self) -> bool {
    self.strikethrough
  }

  /// Where a selectable text's selection shows, in window coordinates,
  /// to be drawn behind it: a rectangle for each line it covers, or more
  /// where right-to-left and left-to-right text mix.
  pub fn selection(&self) -> Vec<Rect> {
    text_selection(self.text).map_or_else(Vec::new, |range| highlights(self.text, range))
  }
}

/// An icon, ready to draw.
pub struct IconView<'a> {
  /// What to show.
  pub source: &'a IconSource,
  /// Where: a square of the icon's size, centered in the node.
  pub rect: Rect,
  /// The color its pixels are painted in, keeping their alpha.
  pub tint: Option<Color>,
}

/// An image, ready to draw.
pub struct ImageView<'a> {
  /// Names the image, so a renderer can keep a GIF's playback and the
  /// platform repaint it with [`Window::repaint_image`](crate::Window::repaint_image).
  pub id: u64,
  /// The file to show.
  pub path: &'a Path,
  /// Where: a box of the image's size, centered in the node.
  pub rect: Rect,
  /// How the picture fills the box.
  pub fit: Fit,
  /// What the picture is drawn through.
  pub filters: ImageFilters,
  /// Whether a GIF plays; paused, it stays on its frame.
  pub playing: bool,
  /// How a new source comes in. With one other than none, a source that
  /// can't be loaded shows the last one that could.
  pub transition: ImageTransition,
  /// While a transition runs: the file it leaves and how far along it is,
  /// eased, from 0 (all old) to 1 (all new); an easing that overshoots
  /// may stray past either end.
  pub leaving: Option<(&'a Path, f32)>,
}

/// A screencopy view's frame, ready to draw.
pub struct CaptureView<'a> {
  /// Names the view, so a cache can keep its frame scaled to its box.
  pub id: u64,
  /// The frame last captured.
  pub frame: &'a CaptureFrame,
  /// Where: the view's content box.
  pub rect: Rect,
  /// How the frame fills the box.
  pub fit: Fit,
}
