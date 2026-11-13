//! Layout: taffy computes flexbox and grid layout over the window's nodes.
//!
//! `Row` and `Column` are flex containers. `Stack` and the window itself are
//! one-cell grids, so every child fills the cell. `Button` is a flex
//! container that centers its child. `Text` and `Icon` report their size
//! through taffy's measure function.
//!
//! A layout runs in three passes. The first walks the tree from the root,
//! hands each node's taffy style to taffy and reshapes text whose content or
//! inherited font changed. The second is taffy's. The third walks the tree
//! again to turn taffy's results into window coordinates, lays text out in
//! its final width, and records as damage every node that moved, appeared
//! or changed.

use cosmic_text::FontSystem;
use taffy::prelude::{TaffyGridLine, TaffyGridSpan};
use taffy::{
  AlignItems, AvailableSpace, Dimension, Display, FlexDirection, GridPlacement, JustifyContent,
  LengthPercentage, Line, Overflow,
};

use crate::geometry::{Point, Rect};
use crate::style::Font;
use crate::text::TextSystem;
use crate::tree::{Node, NodeId, NodeKind, Tree, add_damage, centered, icon_rect};
use crate::widgets::{Align, Justify};

/// The kind of container a node sits in, which decides how it is placed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
  /// A flex container, or nothing for the root.
  Flex,
  /// A `Stack`'s one-cell grid.
  Stack,
  /// The window's one-cell grid.
  Window,
  /// A scroll view, whose child keeps its full height.
  Scroll,
}

impl Tree {
  /// Lays the window out at `self.size` and records damage. A popup first
  /// takes the size its content wants, rounded up to whole pixels.
  pub(crate) fn layout(&mut self, text: &mut TextSystem) {
    let root = self.root;
    self.sync(root, Parent::Flex, Font::DEFAULT, text);

    let fonts = text.font_system();
    if self.popup.is_some() {
      self.size = self.natural_size(fonts);
      let mut style = self.taffy.style(root).cloned().unwrap_or_default();
      style.size = definite(self.size);
      let _ = self.taffy.set_style(root, style);
    }
    let available = taffy::Size {
      width: AvailableSpace::Definite(self.size.width),
      height: AvailableSpace::Definite(self.size.height),
    };
    let measured =
      self
        .taffy
        .compute_layout_with_measure(root, available, |inputs, _id, node, style| {
          taffy::compute_leaf_layout(
            inputs,
            style,
            |_, _| 0.0,
            |known, available| measure(node, fonts, known, available),
          )
        });
    if let Err(error) = measured {
      tracing::error!(%error, "taffy failed to lay out a window");
      return;
    }

    self.place(root, Point::default(), fonts);
    self.needs_layout = false;
    self.pointer_after_layout();
  }

  /// The size the window's content wants, in whole logical pixels and at
  /// least 1 a side: its root laid out with no size of its own and
  /// unlimited room.
  fn natural_size(&mut self, fonts: &mut FontSystem) -> crate::Size {
    let root = self.root;
    let mut style = self.taffy.style(root).cloned().unwrap_or_default();
    style.size = taffy::Size::auto();
    let _ = self.taffy.set_style(root, style);
    let unlimited = taffy::Size {
      width: AvailableSpace::MaxContent,
      height: AvailableSpace::MaxContent,
    };
    let measured =
      self
        .taffy
        .compute_layout_with_measure(root, unlimited, |inputs, _id, node, style| {
          taffy::compute_leaf_layout(
            inputs,
            style,
            |_, _| 0.0,
            |known, available| measure(node, fonts, known, available),
          )
        });
    match (measured, self.taffy.layout(root)) {
      // At least 1 px a side: xdg_positioner refuses an empty size.
      (Ok(()), Ok(layout)) => crate::Size::new(
        layout.size.width.ceil().max(1.0),
        layout.size.height.ceil().max(1.0),
      ),
      (Err(error), _) => {
        tracing::error!(%error, "taffy failed to measure a popup");
        self.size
      }
      (_, Err(error)) => {
        tracing::error!(%error, "taffy has no layout for a popup's root");
        self.size
      }
    }
  }

  /// The first pass: gives taffy each node's style, and reshapes text.
  /// `font` is the font inherited from the ancestors.
  fn sync(&mut self, id: NodeId, parent: Parent, font: Font, text: &mut TextSystem) {
    let size = self.size;
    let Some(node) = self.taffy.get_node_context_mut(id) else {
      return;
    };
    let font = node.effective_style().font.unwrap_or(font);
    let mut reshaped = false;
    if let NodeKind::Text(text_node) = &mut node.kind {
      reshaped = text_node.prepare(font, text);
    }
    let changed = node.changed;
    let style = taffy_style(node, parent, size);
    let child_parent = match node.kind {
      NodeKind::Root => Parent::Window,
      NodeKind::Stack => Parent::Stack,
      NodeKind::Scroll(_) => Parent::Scroll,
      _ => Parent::Flex,
    };

    let same_style = self.taffy.style(id).is_ok_and(|current| *current == style);
    if !same_style {
      // Setting a style also marks the node for measuring again.
      let _ = self.taffy.set_style(id, style);
    } else if reshaped || changed {
      let _ = self.taffy.mark_dirty(id);
    }
    for child in self.taffy.children(id).unwrap_or_default() {
      self.sync(child, child_parent, font, text);
    }
  }

  /// The third pass: turns taffy's layout of `id`, relative to its
  /// parent's corner `origin`, into window coordinates, and records damage.
  /// Returns the bounds of `id` and its descendants.
  fn place(&mut self, id: NodeId, origin: Point, fonts: &mut FontSystem) -> Rect {
    let Ok(&layout) = self.taffy.layout(id) else {
      return Rect::default();
    };
    let rect = Rect::new(
      origin.x + layout.location.x,
      origin.y + layout.location.y,
      layout.size.width,
      layout.size.height,
    );
    let inset_left = layout.padding.left + layout.border.left;
    let inset_top = layout.padding.top + layout.border.top;
    let content = Rect::new(
      rect.x + inset_left,
      rect.y + inset_top,
      (layout.content_box_width()).max(0.0),
      (layout.content_box_height()).max(0.0),
    );

    // A scroll view shows its child moved up by the offset, clamped so
    // neither end scrolls past the box.
    let mut origin = Point::new(rect.x, rect.y);
    if let Some(NodeKind::Scroll(scroll)) = self.node(id).map(|node| &node.kind) {
      let bottom = self
        .taffy
        .children(id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|child| self.taffy.layout(child).ok())
        .map(|child| {
          child.location.y + child.size.height + layout.padding.bottom + layout.border.bottom
        })
        .fold(0.0, f32::max);
      let most = (bottom - layout.size.height).max(0.0);
      let shown = if scroll.offset.is_nan() {
        0.0
      } else {
        scroll.offset.clamp(0.0, most)
      };
      origin.y -= shown;
      if let Some(NodeKind::Scroll(scroll)) = self.node_mut(id).map(|node| &mut node.kind) {
        scroll.shown = shown;
        scroll.most = most;
      }
    }
    let mut children = Rect::default();
    for child in self.taffy.children(id).unwrap_or_default() {
      children = children.union(self.place(child, origin, fonts));
    }

    let Some(node) = self.taffy.get_node_context_mut(id) else {
      return rect.union(children);
    };
    // What the node itself draws: its rectangle, and text or an icon
    // that overflows it. Only a clipping style cuts its children's
    // overflow; otherwise it shows, as in CSS.
    let mut painted = rect;
    match &mut node.kind {
      NodeKind::Text(text) => {
        text.finish(content, fonts);
        painted = painted.union(text.ink_box());
      }
      NodeKind::Icon(icon) => painted = painted.union(icon_rect(content, icon.size)),
      NodeKind::Image(image) => {
        painted = painted.union(centered(content, image.width, image.height));
      }
      _ => {}
    }
    // Repaint what the node drew where it was and draws where it is
    // now. A node that draws nothing itself, like a spacer, adds no
    // damage; its children report their own.
    let drawn = if node.draws() {
      painted
    } else {
      Rect::default()
    };
    match node.rect {
      None => add_damage(&mut self.damage, drawn),
      Some(_) if node.painted != painted || node.changed => {
        add_damage(&mut self.damage, node.drawn);
        add_damage(&mut self.damage, drawn);
      }
      Some(_) => {}
    }
    node.drawn = drawn;
    let clips = node.clips();
    let children = if clips {
      children.intersect(rect)
    } else {
      children
    };
    let bounds = painted.union(children);
    // A clip that came, went or changed shape shows or hides parts of
    // children that themselves didn't change.
    if (clips || node.clips) && node.rect.is_some() && (node.changed || node.bounds != bounds) {
      add_damage(&mut self.damage, node.bounds);
      add_damage(&mut self.damage, bounds);
    }
    node.clips = clips;
    node.rect = Some(rect);
    node.content = content;
    node.painted = painted;
    node.bounds = bounds;
    node.changed = false;
    bounds
  }
}

/// Measures a leaf: text through its buffer, an icon as a square, an
/// image at its size.
fn measure(
  node: Option<&mut Node>,
  fonts: &mut FontSystem,
  known: taffy::Size<Option<f32>>,
  available: taffy::Size<AvailableSpace>,
) -> taffy::Size<f32> {
  match node.map(|node| &mut node.kind) {
    Some(NodeKind::Text(text)) => text.measure(fonts, known, available),
    Some(NodeKind::Image(image)) => taffy::Size {
      width: known.width.unwrap_or(image.width as f32),
      height: known.height.unwrap_or(image.height as f32),
    },
    Some(NodeKind::Icon(icon)) => {
      let side = icon.size as f32;
      taffy::Size {
        width: known.width.unwrap_or(side),
        height: known.height.unwrap_or(side),
      }
    }
    _ => taffy::Size {
      width: known.width.unwrap_or(0.0),
      height: known.height.unwrap_or(0.0),
    },
  }
}

/// The taffy style for `node`, which sits in a `parent` container. `size`
/// is the window's, which the root takes.
fn taffy_style(node: &Node, parent: Parent, size: crate::Size) -> taffy::Style {
  let padding = node.padding();
  let vertical = LengthPercentage::length(padding.vertical as f32);
  let horizontal = LengthPercentage::length(padding.horizontal as f32);
  let mut style = taffy::Style {
    padding: taffy::Rect {
      left: horizontal,
      right: horizontal,
      top: vertical,
      bottom: vertical,
    },
    ..taffy::Style::default()
  };
  match &node.kind {
    NodeKind::Root => {
      style.display = Display::Grid;
      style.size = definite(size);
      style.grid_template_columns = vec![taffy::prelude::fr(1.0)];
      style.grid_template_rows = vec![taffy::prelude::fr(1.0)];
    }
    NodeKind::Flex(flex) => {
      style.flex_direction = if flex.column {
        FlexDirection::Column
      } else {
        FlexDirection::Row
      };
      if flex.grow {
        style.flex_grow = 1.0;
      }
      let gap = LengthPercentage::length(flex.gap as f32);
      style.gap = taffy::Size {
        width: gap,
        height: gap,
      };
      style.align_items = match flex.align {
        Align::Start => AlignItems::START,
        Align::Center => AlignItems::CENTER,
        Align::End => AlignItems::END,
        Align::Stretch => AlignItems::STRETCH,
      };
      style.justify_content = match flex.justify {
        Justify::Start => JustifyContent::START,
        Justify::Center => JustifyContent::CENTER,
        Justify::End => JustifyContent::END,
        Justify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        Justify::SpaceAround => JustifyContent::SPACE_AROUND,
        Justify::SpaceEvenly => JustifyContent::SPACE_EVENLY,
      };
    }
    NodeKind::Stack => style.display = Display::Grid,
    NodeKind::Scroll(_) => {
      style.flex_direction = FlexDirection::Column;
      // As in CSS, a scroll container isn't held to its content's size.
      style.overflow = taffy::Point {
        x: Overflow::Visible,
        y: Overflow::Scroll,
      };
      style.scrollbar_width = 0.0;
    }
    NodeKind::Spacer => style.flex_grow = 1.0,
    NodeKind::Button(_) => {
      style.align_items = AlignItems::CENTER;
      style.justify_content = JustifyContent::CENTER;
    }
    NodeKind::Text(_) | NodeKind::Icon(_) | NodeKind::Image(_) => {}
  }
  // A size the style sets wins over the content's, but never over the
  // window's own.
  let own = node.effective_style();
  if !matches!(node.kind, NodeKind::Root) {
    let length = |value: Option<f32>| match value {
      Some(value) if value.is_finite() => Dimension::length(value.max(0.0)),
      _ => Dimension::auto(),
    };
    style.size = taffy::Size {
      width: length(own.width),
      height: length(own.height),
    };
  }
  if parent == Parent::Scroll {
    // A child with a height of its own would otherwise shrink to what its
    // content needs.
    style.flex_shrink = 0.0;
  }
  if matches!(parent, Parent::Stack | Parent::Window) {
    let cell = Line {
      start: GridPlacement::from_line_index(1),
      end: GridPlacement::from_span(1),
    };
    style.grid_row = cell.clone();
    style.grid_column = cell;
  }
  if parent == Parent::Window {
    // As in CSS, an item can't be smaller than its content by default.
    // The window's size comes from the compositor, so its child is
    // exactly that size, and content too wide overflows inside it.
    style.min_size = taffy::Size {
      width: taffy::LengthPercentageAuto::length(0.0),
      height: taffy::LengthPercentageAuto::length(0.0),
    };
  }
  style
}

/// A taffy size of exactly `size`.
fn definite(size: crate::Size) -> taffy::Size<taffy::Dimension> {
  taffy::Size {
    width: taffy::Dimension::length(size.width),
    height: taffy::Dimension::length(size.height),
  }
}
