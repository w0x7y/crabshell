//! A read-only view of a laid-out window, for drawing it.

use std::cell::Ref;

use cosmic_text::{Buffer, LayoutRun};
use taffy::TraversePartialTree;

use crate::geometry::{Point, Rect, Size};
use crate::style::{Color, Style};
use crate::text::TextNode;
use crate::tree::{Node, NodeId, NodeKind, Tree, icon_rect};
use crate::widgets::IconSource;

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
/// root. Draw it, then its children in order, so later ones are on top.
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
    /// descendants' bounds. Nothing outside it needs drawing for this node.
    pub fn bounds(&self) -> Rect {
        self.node.bounds
    }

    /// The style in effect, with a button's hover and pressed styles laid
    /// over its own. Inherited fields aren't filled in;
    /// [`TextView::color`] has the color text is drawn in.
    pub fn style(&self) -> Style {
        self.node.effective_style()
    }

    /// The children, in drawing order.
    pub fn children(&self) -> impl Iterator<Item = NodeView<'a>> + 'a {
        let tree = self.tree;
        tree.taffy
            .child_ids(self.id)
            .filter_map(move |id| NodeView::new(tree, id))
    }

    /// What the node draws besides its background and border.
    pub fn content(&self) -> Content<'a> {
        match &self.node.kind {
            NodeKind::Text(text) => Content::Text(TextView {
                text,
                color: self.inherited_color(),
            }),
            NodeKind::Icon(icon) => match &icon.source {
                Some(source) => Content::Icon(IconView {
                    source,
                    rect: icon_rect(self.node.content, icon.size),
                }),
                None => Content::None,
            },
            _ => Content::None,
        }
    }

    /// The color of the nearest node, starting with this one, whose style
    /// sets one.
    fn inherited_color(&self) -> Color {
        let mut id = Some(self.id);
        while let Some(current) = id {
            if let Some(color) = self
                .tree
                .node(current)
                .and_then(|node| node.effective_style().color)
            {
                return color;
            }
            id = self.tree.taffy.parent(current);
        }
        DEFAULT_TEXT_COLOR
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
}

/// Laid-out text, ready to draw. Lines can stick out of the node's
/// rectangle when it is too small for them; draw them unclipped, as CSS
/// does. The window's damage covers them.
pub struct TextView<'a> {
    text: &'a TextNode,
    color: Color,
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
}

/// An icon, ready to draw.
pub struct IconView<'a> {
    /// What to show.
    pub source: &'a IconSource,
    /// Where: a square of the icon's size, centered in the node.
    pub rect: Rect,
}
