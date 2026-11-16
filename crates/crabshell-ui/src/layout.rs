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

use std::rc::Rc;

use cosmic_text::FontSystem;
use taffy::prelude::{TaffyGridLine, TaffyGridSpan};
use taffy::{
  AlignContent, AlignItems, AvailableSpace, Dimension, Display, FlexDirection, FlexWrap,
  GridPlacement, JustifyContent, LengthPercentage, LengthPercentageAuto, Line, Overflow, Position,
};

use crate::geometry::{Affine, Point, Rect};
use crate::scroll::{BAR_MIN_LENGTH, BAR_WIDTH};
use crate::style::{Font, Shadow, Style};
use crate::text::TextSystem;
use crate::text_input::DEFAULT_WIDTH;
use crate::tree::{Node, NodeId, NodeKind, ScrollNode, Tree, add_damage, centered, icon_rect};
use crate::widgets::{Align, Justify};

/// The kind of container a node sits in, which decides how it is placed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
  /// A flex container, or nothing for the root.
  Flex,
  /// A `Stack`'s one-cell grid.
  Stack,
  /// A `Grid`, whose children may span cells.
  Grid,
  /// The window's one-cell grid.
  Window,
  /// A scroll view, whose child keeps its full height.
  Scroll,
}

impl Tree {
  /// Lays the window out at `self.size` and records damage. A popup first
  /// takes the size its content wants, rounded up to whole pixels, within
  /// the size the compositor shrank it to.
  pub(crate) fn layout(&mut self, text: &mut TextSystem) {
    let root = self.root;
    // Only a transform changed: nothing moves in the layout, so taffy
    // isn't asked again. Nor is the pointer: a hover transform that moves
    // its button off a still pointer would unhover it, move it back and
    // hover it again for as long as the pointer stays.
    if self.needs_place && !self.needs_layout {
      self.place(root, Point::default(), &mut Vec::new(), text.font_system());
      self.needs_place = false;
      // A drag holds the hover, but what it would drop on may have moved.
      self.drop_after_place();
      return;
    }
    for shown in std::mem::take(&mut self.shown_since_layout) {
      self.focus_shown_field(shown);
    }
    self.sync(root, Parent::Flex, Font::DEFAULT, text);

    let fonts = text.font_system();
    if self.popup.is_some() {
      let wanted = self.natural_size(fonts);
      self.size = wanted;
      if let Some(popup) = self.popup.as_mut() {
        popup.wanted = wanted;
        if let Some(limit) = popup.limit {
          self.size = crate::Size::new(
            wanted.width.min(limit.width),
            wanted.height.min(limit.height),
          );
        }
      }
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

    self.place(root, Point::default(), &mut Vec::new(), fonts);
    self.needs_layout = false;
    self.needs_place = false;
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
    match &mut node.kind {
      NodeKind::Text(text_node) => reshaped = text_node.prepare(font, text),
      NodeKind::Input(input) => reshaped = input.text.prepare(font, text),
      _ => {}
    }
    let changed = node.changed;
    let style = taffy_style(node, parent, size);
    let child_parent = match node.kind {
      NodeKind::Root => Parent::Window,
      NodeKind::Stack => Parent::Stack,
      NodeKind::Grid(_) => Parent::Grid,
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
  /// `chain` holds the ancestors' transforms and content shadows,
  /// outermost first. Returns the bounds of `id` and its descendants, in
  /// its parent's coordinates.
  fn place(
    &mut self,
    id: NodeId,
    origin: Point,
    chain: &mut Vec<Step>,
    fonts: &mut FontSystem,
  ) -> Rect {
    if self.node(id).is_some_and(|node| !node.visible) {
      self.unplace(id);
      return Rect::default();
    }
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

    // A scroll view shows its child moved up, or left, by the offset,
    // clamped so neither end scrolls past the box.
    let mut origin = Point::new(rect.x, rect.y);
    if let Some(NodeKind::Scroll(scroll)) = self.node(id).map(|node| &node.kind) {
      let horizontal = scroll.horizontal;
      let end = self
        .taffy
        .children(id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|child| self.taffy.layout(child).ok())
        .map(|child| {
          if horizontal {
            child.location.x + child.size.width + layout.padding.right + layout.border.right
          } else {
            child.location.y + child.size.height + layout.padding.bottom + layout.border.bottom
          }
        })
        .fold(0.0, f32::max);
      let length = if horizontal {
        layout.size.width
      } else {
        layout.size.height
      };
      let most = (end - length).max(0.0);
      let shown = if scroll.offset.is_nan() {
        0.0
      } else {
        scroll.offset.clamp(0.0, most)
      };
      // An overshoot is only shown, as a paint move of the child.
      let moved = shown + scroll.overshoot;
      if horizontal {
        origin.x -= moved;
      } else {
        origin.y -= moved;
      }
      if let Some(NodeKind::Scroll(scroll)) = self.node_mut(id).map(|node| &mut node.kind) {
        scroll.shown = shown;
        scroll.most = most;
        scroll.length = length;
      }
    }
    self.begin_slide(id, Point::new(layout.location.x, layout.location.y));
    let Some(node) = self.node_mut(id) else {
      return rect;
    };
    let transform = displaced(node.shown_style().transform(rect), node.displace);
    node.transform = transform;
    let own = chain.len();
    chain.extend(transform.map(Step::Transform));
    // Inside the transform: the shadow turns with what casts it. A fading
    // one reaches anywhere between its start and its end.
    let fading = node.fade.as_ref().map(|fade| fade.from);
    let refused = node
      .effective_style()
      .shadow
      .is_some_and(|shadow| shadow.follow_content && !shadow.casts_from_content());
    if refused && !std::mem::replace(&mut node.shadow_refused, true) {
      tracing::warn!(
        "{}: a content shadow reaching more than 64 pixels past its content isn't drawn",
        node.path
      );
    }
    let shadows = [Some(node.effective_style()), fading]
      .into_iter()
      .flatten()
      .filter_map(|style| style.shadow)
      .filter(Shadow::casts_from_content);
    chain.extend(shadows.map(Step::Shadow));
    let mut children = Rect::default();
    for child in self.taffy.children(id).unwrap_or_default() {
      children = children.union(self.place(child, origin, chain, fonts));
    }
    self.place_bar(id, rect, chain);
    let bounds = self.finish_place(id, rect, content, children, (chain, own), fonts);
    chain.truncate(own);
    bounds
  }

  /// Places scroll view `id`'s bar along its right or bottom edge, inside
  /// the border, and repaints where its handle was and is when it moves,
  /// comes or goes. `chain` ends with the node's own transform.
  fn place_bar(&mut self, id: NodeId, rect: Rect, chain: &[Step]) {
    // Most nodes aren't scroll views, so nothing else is read for them.
    let Some(Node {
      kind: NodeKind::Scroll(scroll),
      style,
      ..
    }) = self.taffy.get_node_context_mut(id)
    else {
      return;
    };
    // A scroll view's style is the style in effect: only a button's
    // differs.
    let border = style.border.map_or(0.0, |border| border.width.max(0.0));
    let handle = scroll
      .bar
      .filter(|_| scroll.most > 0.0)
      .and_then(|bar| bar_rects(scroll, bar, rect, border));
    let old = std::mem::replace(&mut scroll.handle, handle);
    if old != handle {
      for (_, shape) in [old, handle].into_iter().flatten() {
        add_damage(&mut self.damage, through(chain, shape));
      }
    }
  }

  /// Forgets where a hidden node and everything in it were, repainting
  /// the area they covered. Shown again, each is placed and painted as a
  /// new one is.
  fn unplace(&mut self, id: NodeId) {
    let mut pending = vec![id];
    while let Some(next) = pending.pop() {
      let Some(node) = self.taffy.get_node_context_mut(next) else {
        continue;
      };
      // Not placed since it was hidden, nor is anything in it.
      if node.rect.take().is_none() {
        continue;
      }
      if next == id {
        add_damage(&mut self.damage, node.window_bounds);
      }
      pending.extend(self.taffy.children(next).unwrap_or_default());
    }
  }

  /// The rest of [`place`](Self::place), once the children are placed:
  /// what node `id` paints, its bounds, and the damage. `chain` ends with
  /// the node's own transform and content shadows, from index `own`.
  fn finish_place(
    &mut self,
    id: NodeId,
    rect: Rect,
    content: Rect,
    children: Rect,
    (chain, own): (&[Step], usize),
    fonts: &mut FontSystem,
  ) -> Rect {
    let Some(node) = self.taffy.get_node_context_mut(id) else {
      return rect.union(children);
    };
    // What the node itself draws: its rectangle, and text or an icon
    // that overflows it. Only a clipping style cuts its children's
    // overflow; otherwise it shows, as in CSS.
    let mut painted = rect;
    // A fading shadow is drawn anywhere between its start and its end.
    let fading = node.fade.as_ref().map(|fade| fade.from);
    for style in [Some(node.effective_style()), fading].into_iter().flatten() {
      if let Some(shadow) = style.shadow.filter(|shadow| !shadow.follow_content) {
        painted = painted.union(shadow.reach(rect));
      }
    }
    match &mut node.kind {
      NodeKind::Text(text) => {
        text.finish(content, fonts);
        painted = painted.union(text.ink_box());
      }
      // A field's text is cut to its content box, so it paints only its
      // own rectangle.
      NodeKind::Input(input) => {
        input.text.finish(content, fonts);
        input.place(content);
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
    let window_painted = through(chain, painted);
    let drawn = if node.draws() {
      window_painted
    } else {
      Rect::default()
    };
    match node.rect {
      None => add_damage(&mut self.damage, drawn),
      Some(_) if node.window_painted != window_painted || node.changed => {
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
    let inside = painted.union(children);
    let window_bounds = through(chain, inside);
    // A clip that came, went or changed shape shows or hides parts of
    // children that themselves didn't change.
    if (clips || node.clips)
      && node.rect.is_some()
      && (node.changed || node.window_bounds != window_bounds)
    {
      add_damage(&mut self.damage, node.window_bounds);
      add_damage(&mut self.damage, window_bounds);
    }
    let bounds = through(&chain[own..], inside);
    if let Some(signal) = node.geometry
      && node.written != Some(rect)
    {
      node.written = Some(rect);
      self.geometry.push((signal, rect));
    }
    let size = crate::Size::new(rect.width, rect.height);
    if let Some(measure) = &node.measure
      && node.measured != Some(size)
    {
      node.measured = Some(size);
      self.measured.push((Rc::clone(measure), size));
    }
    node.clips = clips;
    node.rect = Some(rect);
    node.content = content;
    node.painted = painted;
    node.bounds = bounds;
    // What casts a content shadow is inside the transform.
    node.inner_bounds = match chain.get(own) {
      Some(Step::Transform(_)) => through(&chain[own + 1..], inside),
      _ => through(&chain[own..], inside),
    };
    node.window_bounds = window_bounds;
    node.window_painted = window_painted;
    node.changed = false;
    bounds
  }
}

/// One step out from a node's coordinates to its parent's, for working
/// out where its painting lands.
#[derive(Clone, Copy)]
pub(crate) enum Step {
  /// The node's transform.
  Transform(Affine),
  /// A shadow cast by what the node and its children draw, which reaches
  /// past what casts it.
  Shadow(Shadow),
}

/// Where `rect`, in the innermost space of `chain`, lands in the window:
/// through each step from the innermost out. A transform grows it by a
/// logical pixel first, since a transformed layer is filtered bilinearly
/// and bleeds up to half a pixel past its content; a content shadow adds
/// the shadow it casts.
pub(crate) fn through(chain: &[Step], rect: Rect) -> Rect {
  chain.iter().rev().fold(rect, |rect, step| {
    // A line, empty itself, would cover an area once turned.
    if rect.is_empty() {
      return rect;
    }
    match step {
      Step::Transform(transform) => transform.map_rect(rect.outset(1.0)),
      Step::Shadow(shadow) => rect.union(shadow.reach(rect)),
    }
  })
}

/// `transform`, then a slide's move `by`, in the parent's coordinates.
fn displaced(transform: Option<Affine>, by: Point) -> Option<Affine> {
  if by == Point::default() {
    return transform;
  }
  let mut moved = transform.unwrap_or(Affine {
    sx: 1.0,
    ky: 0.0,
    kx: 0.0,
    sy: 1.0,
    tx: 0.0,
    ty: 0.0,
  });
  moved.tx += by.x;
  moved.ty += by.y;
  Some(moved)
}

/// A scroll bar's track and handle inside `rect`, a scroll view's box
/// with a border `border` wide: as long as the part shown, placed by the
/// offset shown, and never shorter than [`BAR_MIN_LENGTH`] unless the
/// track is.
fn bar_rects(scroll: &ScrollNode, bar: Style, rect: Rect, border: f32) -> Option<(Rect, Rect)> {
  let width = bar
    .width
    .filter(|w| w.is_finite() && *w > 0.0)
    .unwrap_or(BAR_WIDTH);
  let (along, start) = if scroll.horizontal {
    (rect.width - 2.0 * border, rect.x + border)
  } else {
    (rect.height - 2.0 * border, rect.y + border)
  };
  if along <= 0.0 {
    return None;
  }
  let view = if scroll.horizontal {
    rect.width
  } else {
    rect.height
  };
  let length = (along * view / (view + scroll.most))
    .max(BAR_MIN_LENGTH)
    .min(along);
  let at = start + (along - length) * scroll.shown / scroll.most;
  Some(if scroll.horizontal {
    let y = rect.bottom() - border - width;
    (
      Rect::new(start, y, along, width),
      Rect::new(at, y, length, width),
    )
  } else {
    let x = rect.right() - border - width;
    (
      Rect::new(x, start, width, along),
      Rect::new(x, at, width, length),
    )
  })
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
    // A field is as tall as a line and doesn't grow as it is typed in.
    Some(NodeKind::Input(input)) => {
      let line = input.text.measure(fonts, known, available);
      taffy::Size {
        width: known.width.unwrap_or(DEFAULT_WIDTH),
        height: line.height,
      }
    }
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
  let side = |pixels: u32| LengthPercentage::length(pixels as f32);
  let mut style = taffy::Style {
    padding: taffy::Rect {
      left: side(padding.left),
      right: side(padding.right),
      top: side(padding.top),
      bottom: side(padding.bottom),
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
      style.flex_direction = match (flex.column, flex.reverse) {
        (false, false) => FlexDirection::Row,
        (false, true) => FlexDirection::RowReverse,
        (true, false) => FlexDirection::Column,
        (true, true) => FlexDirection::ColumnReverse,
      };
      if flex.wrap {
        style.flex_wrap = FlexWrap::Wrap;
        // Lines pack at the start, as Qt's Flow does, rather than
        // stretch to share a taller box, as CSS's do.
        style.align_content = AlignContent::FLEX_START;
      }
      if flex.grow {
        style.flex_grow = 1.0;
      }
      let gap = LengthPercentage::length(flex.gap as f32);
      style.gap = taffy::Size {
        width: gap,
        height: gap,
      };
      style.align_items = align_items(flex.align);
      style.justify_content = match flex.justify {
        // Flex starts and ends, which a reversed row swaps.
        Justify::Start => JustifyContent::FLEX_START,
        Justify::Center => JustifyContent::CENTER,
        Justify::End => JustifyContent::FLEX_END,
        Justify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
        Justify::SpaceAround => JustifyContent::SPACE_AROUND,
        Justify::SpaceEvenly => JustifyContent::SPACE_EVENLY,
      };
    }
    NodeKind::Grid(grid) => {
      style.display = Display::Grid;
      // `minmax(0, 1fr)`: equal widths, whatever the content wants.
      let column = taffy::prelude::minmax(
        taffy::MinTrackSizingFunction::length(0.0),
        taffy::MaxTrackSizingFunction::fr(1.0),
      );
      let count = grid.columns.clamp(1, u32::from(u16::MAX)) as u16;
      style.grid_template_columns = vec![taffy::prelude::repeat(count, vec![column])];
      // Rows as tall as their content, packed at the top, rather than
      // stretched to share a taller grid, as CSS's are.
      style.align_content = AlignContent::START;
      let gap = |own: Option<u32>| LengthPercentage::length(own.unwrap_or(grid.gap) as f32);
      style.gap = taffy::Size {
        width: gap(grid.column_gap),
        height: gap(grid.row_gap),
      };
    }
    NodeKind::Stack => style.display = Display::Grid,
    NodeKind::Scroll(scroll) => {
      // As in CSS, a scroll container isn't held to its content's size.
      if scroll.horizontal {
        style.flex_direction = FlexDirection::Row;
        style.overflow = taffy::Point {
          x: Overflow::Scroll,
          y: Overflow::Visible,
        };
      } else {
        style.flex_direction = FlexDirection::Column;
        style.overflow = taffy::Point {
          x: Overflow::Visible,
          y: Overflow::Scroll,
        };
      }
      style.scrollbar_width = 0.0;
    }
    NodeKind::Spacer => style.flex_grow = 1.0,
    NodeKind::Button(_) => {
      style.align_items = AlignItems::CENTER;
      style.justify_content = JustifyContent::CENTER;
    }
    NodeKind::Text(_)
    | NodeKind::Icon(_)
    | NodeKind::Image(_)
    | NodeKind::Capture(_)
    | NodeKind::Canvas(_)
    | NodeKind::Input(_) => {}
  }
  if !node.visible {
    style.display = Display::None;
  }
  // A size the style sets wins over the content's, but never over the
  // window's own.
  let own = node.effective_style();
  if !matches!(node.kind, NodeKind::Root) {
    own_layout(&mut style, &own, parent);
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
  if parent == Parent::Grid
    && let Some((columns, rows)) = own.span
  {
    let span = |cells: u32| GridPlacement::from_span(cells.clamp(1, u32::from(u16::MAX)) as u16);
    style.grid_column = Line {
      start: span(columns),
      end: GridPlacement::Auto,
    };
    style.grid_row = Line {
      start: span(rows),
      end: GridPlacement::Auto,
    };
  }
  style
}

/// What a widget's own style says about its place: sizes, flex fields,
/// alignment and absolute position. A number that isn't finite is unset.
fn own_layout(style: &mut taffy::Style, own: &Style, parent: Parent) {
  let finite = |value: Option<f32>| value.filter(|value| value.is_finite());
  let length = |value: Option<f32>| match finite(value) {
    Some(value) => Dimension::length(value.max(0.0)),
    None => Dimension::auto(),
  };
  let limit = |value: Option<f32>, none: LengthPercentageAuto| match finite(value) {
    Some(value) => LengthPercentageAuto::length(value.max(0.0)),
    None => none,
  };
  style.size = taffy::Size {
    width: length(own.width),
    height: length(own.height),
  };
  // As in CSS, an item can't be smaller than its content by default.
  // The window's size comes from the compositor, so its child is exactly
  // that size, and content too wide overflows inside it.
  let least = if parent == Parent::Window {
    LengthPercentageAuto::length(0.0)
  } else {
    LengthPercentageAuto::auto()
  };
  style.min_size = taffy::Size {
    width: limit(own.min_width, least),
    height: limit(own.min_height, least),
  };
  style.max_size = taffy::Size {
    width: limit(own.max_width, LengthPercentageAuto::auto()),
    height: limit(own.max_height, LengthPercentageAuto::auto()),
  };
  if let Some(grow) = finite(own.grow) {
    style.flex_grow = grow.max(0.0);
  }
  if let Some(shrink) = finite(own.shrink) {
    style.flex_shrink = shrink.max(0.0);
  }
  if let Some(basis) = finite(own.basis) {
    style.flex_basis = Dimension::length(basis.max(0.0));
  }
  style.align_self = own.align_self.map(align_items);
  style.justify_self = own.justify_self.map(align_items);
  if own.absolute() {
    let inset = |value: Option<f32>| match finite(value) {
      Some(value) => LengthPercentageAuto::length(value),
      None => LengthPercentageAuto::auto(),
    };
    style.position = Position::Absolute;
    style.inset = taffy::Rect {
      left: inset(own.left),
      right: inset(own.right),
      top: inset(own.top),
      bottom: inset(own.bottom),
    };
  }
}

/// `align` as taffy's.
fn align_items(align: Align) -> AlignItems {
  match align {
    Align::Start => AlignItems::START,
    Align::Center => AlignItems::CENTER,
    Align::End => AlignItems::END,
    Align::Stretch => AlignItems::STRETCH,
  }
}

/// A taffy size of exactly `size`.
fn definite(size: crate::Size) -> taffy::Size<taffy::Dimension> {
  taffy::Size {
    width: taffy::Dimension::length(size.width),
    height: taffy::Dimension::length(size.height),
  }
}
