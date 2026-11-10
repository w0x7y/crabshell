//! A read-only view of a window's tree.

use std::cell::Ref;

use taffy::TraversePartialTree;

use crate::geometry::Size;
use crate::style::{Color, Style};
use crate::text::TextNode;
use crate::tree::{Node, NodeId, NodeKind, Tree};
use crate::widgets::IconSource;

/// The color text uses when no ancestor sets one.
const DEFAULT_TEXT_COLOR: Color = Color::rgb(0, 0, 0);

/// A window's tree, borrowed for reading. Get one from
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

/// One node of a window: a widget, or the window itself at the root. Its
/// children are in drawing order, so later ones are on top.
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

    /// The widget's style. Inherited fields aren't filled in;
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
                Some(source) => Content::Icon(IconView { source }),
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

/// A text node's content and color.
pub struct TextView<'a> {
    text: &'a TextNode,
    color: Color,
}

impl<'a> TextView<'a> {
    /// The text itself.
    pub fn content(&self) -> &'a str {
        &self.text.content
    }

    /// The color to draw in, inherited if the text's own style doesn't set
    /// one.
    pub fn color(&self) -> Color {
        self.color
    }
}

/// An icon node's content.
pub struct IconView<'a> {
    /// What to show.
    pub source: &'a IconSource,
}
