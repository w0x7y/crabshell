//! A window's tree of nodes: turning elements into nodes, applying property
//! changes, and recording what needs layout and what needs repainting.
//!
//! The nodes live in taffy's tree, each carrying a [`Node`] as its context,
//! so taffy's parent and child links are the only ones.
//!
//! The tree sits in an `Rc<RefCell<..>>` that the [`Window`](crate::Window)
//! owns. Effects reach it through a `Weak`, so they never keep a dropped
//! window alive. It is never borrowed while user code runs: closures run
//! before the borrow and handlers after it, and user closures and values
//! live in the reactive runtime rather than here, so the runtime drops them.

use std::cell::{Cell, RefCell, RefMut};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use crabshell_reactive::Signal;
use taffy::TaffyTree;

use crate::animation::{self, Transition};
use crate::canvas::CanvasNode;
use crate::cx::{Anchor, Cx};
use crate::floating::FloatingState;
use crate::geometry::{Affine, Point, Rect, Size};
use crate::image::{ImageTransition, Leaving};
use crate::input::{CursorShape, DragValue, Handlers, Pointer};
use crate::keyboard::Keyboard;
use crate::keyed::KeyedSlot;
use crate::prop::{IntoProp, Prop, PropKind};
use crate::region::Region;
use crate::screencopy::CaptureNode;
use crate::scrolling::Motion;
use crate::style::{Color, Padding, Style};
use crate::text::TextNode;
use crate::text_input::InputNode;
use crate::timer::Timer;
use crate::widgets::{
  Align, Element, ElementChild, ElementKind, Fit, GridProps, IconSource, ImageFilters, Justify,
  Measure,
};
use crate::window::{
  Anchors, DEFAULT_NAMESPACE, Hints, KeyboardFocus, Layer, Margins, Output, PanelWindow, Window,
};

pub(crate) type NodeId = taffy::NodeId;

pub(crate) struct Tree {
  /// This tree, for effects to reach without keeping it alive.
  pub(crate) this: Weak<RefCell<Tree>>,
  pub(crate) taffy: TaffyTree<Node>,
  pub(crate) root: NodeId,
  pub(crate) output: Output,
  pub(crate) surface: Surface,
  /// The size the compositor gave the window.
  pub(crate) size: Size,
  pub(crate) needs_layout: bool,
  /// [`fonts_added`](crate::text::fonts_added) at the last layout.
  pub(crate) fonts_added: u64,
  /// A transform changed, so where things are drawn is worked out again,
  /// without laying anything out.
  pub(crate) needs_place: bool,
  /// Areas to repaint, gathered since the renderer last took them.
  pub(crate) damage: Vec<Rect>,
  pub(crate) pointer: Pointer,
  pub(crate) keyboard: Keyboard,
  /// Popups its widgets opened, until the platform takes them.
  pub(crate) popups: Vec<Window>,
  /// Set for a popup's own tree.
  pub(crate) popup: Option<PopupState>,
  /// Set for a floating window's own tree.
  pub(crate) floating: Option<FloatingState>,
  /// The screencopy views, in the order they were built.
  pub(crate) captures: Vec<NodeId>,
  /// The images, for the platform to repaint a GIF's next frame by id.
  pub(crate) images: Vec<NodeId>,
  /// Rectangles the last layout moved, for the window to write to their
  /// widgets' `geometry` signals once the tree is released.
  pub(crate) geometry: Vec<(Signal<Rect>, Rect)>,
  /// The sizes to tell widgets' `measure` once the tree is released.
  pub(crate) measured: Vec<(Measure, Size)>,
  /// The focused field changed, so the next layout scrolls the views
  /// around it to show its caret.
  pub(crate) reveal: Option<NodeId>,
  /// Scroll offsets that showing a caret moved, for the window to write
  /// to the views' signals once the tree is released.
  pub(crate) offsets: Vec<(Signal<f32>, f32)>,
  /// Widgets shown again since the last layout, which gives the focus to
  /// a field in them that asks for it.
  pub(crate) shown_since_layout: Vec<NodeId>,
}

/// What only a popup's tree has.
pub(crate) struct PopupState {
  /// The widget it opened from, in a panel or another popup.
  pub(crate) opener: Rc<Anchor>,
  /// `close_popup` asked for it to close.
  pub(crate) close: bool,
  /// Lives under the popup's owner, so it is gone once the widget that
  /// opened the popup is.
  pub(crate) alive: Signal<()>,
  /// Where it goes, as the config set it.
  pub(crate) placement: crate::popup::Placement,
  /// The size its content wanted at the last layout.
  pub(crate) wanted: Size,
  /// The size the compositor shrank it to, which it is laid out within.
  pub(crate) limit: Option<Size>,
}

pub(crate) struct Node {
  /// The widget type, such as `"Row"`.
  pub(crate) name: &'static str,
  /// The widget path, such as `PanelWindow[DP-1]/Row/Button[2]`, for log
  /// lines.
  pub(crate) path: Rc<str>,
  /// The context of the widget's own owner.
  pub(crate) cx: Cx,
  pub(crate) style: Style,
  pub(crate) kind: NodeKind,
  /// The children in order, grouped by the call that added them: one node
  /// per `child`, any number per `children_keyed`.
  pub(crate) groups: Vec<Vec<NodeId>>,
  /// The number the next child's path gets: how many children a row,
  /// column or stack has created so far, single or keyed. Never reused, so
  /// no two children share a path.
  pub(crate) next_child: usize,
  /// The rectangle from the last layout, in window coordinates. `None`
  /// until the first.
  pub(crate) rect: Option<Rect>,
  /// `rect` without padding.
  pub(crate) content: Rect,
  /// What the node draws: `rect`, and text or an icon that overflows it.
  pub(crate) painted: Rect,
  /// `painted` together with every descendant's, moved by the node's own
  /// transform into its parent's coordinates.
  pub(crate) bounds: Rect,
  /// `painted` together with every descendant's, in the node's own
  /// coordinates, before its transform: what a transformed layer holds.
  pub(crate) inner_bounds: Rect,
  /// `bounds` in window coordinates, through every transform around it.
  pub(crate) window_bounds: Rect,
  /// `painted` in window coordinates.
  pub(crate) window_painted: Rect,
  /// What the node drew itself at the last layout, in window coordinates:
  /// `window_painted` if it has a background, a border, text or an icon,
  /// and nothing otherwise. Moving or resizing it repaints only this,
  /// since its children report their own; a spacer that grows repaints
  /// nothing.
  pub(crate) drawn: Rect,
  /// Its style's transform at the last layout, from its own coordinates
  /// into its parent's.
  pub(crate) transform: Option<Affine>,
  /// Whether its style clipped its children at the last layout.
  pub(crate) clips: bool,
  /// Whether it cast a content shadow at the last layout.
  pub(crate) casts: bool,
  /// Whether a content shadow it couldn't cast was logged.
  pub(crate) shadow_refused: bool,
  /// A style change fading in, and the count of fades started, which
  /// tells a fade's clock step whether it was replaced.
  pub(crate) fade: Option<Fade>,
  pub(crate) fades: u64,
  /// A layout property changed, so the node is repainted after the next
  /// layout even if it doesn't move.
  pub(crate) changed: bool,
  /// Shown, rather than taken out of the layout with everything in it.
  pub(crate) visible: bool,
  /// A keyed list's change asked it to slide over this from where it is
  /// drawn, once the next layout says where it goes.
  pub(crate) slide: Option<Transition>,
  /// How far from its laid-out place a slide draws it, in its parent's
  /// coordinates, and the count of slides started, which tells a slide's
  /// clock step whether it was replaced.
  pub(crate) displace: Point,
  pub(crate) slides: u64,
  /// Written with `rect` when a layout changes it.
  pub(crate) geometry: Option<Signal<Rect>>,
  /// What `geometry` was last written, so a layout that moves nothing
  /// writes nothing.
  pub(crate) written: Option<Rect>,
  /// Told the size after a layout that changes it.
  pub(crate) measure: Option<Measure>,
  /// The size `measure` was last told.
  pub(crate) measured: Option<Size>,
}

pub(crate) enum NodeKind {
  /// The `PanelWindow` itself.
  Root,
  Flex(Flex),
  Grid(GridNode),
  Stack,
  Spacer,
  Text(TextNode),
  Icon(IconNode),
  Image(ImageNode),
  Capture(CaptureNode),
  Canvas(CanvasNode),
  // Boxed: its scroll bar's style makes it large.
  Scroll(Box<ScrollNode>),
  Input(Box<InputNode>),
  // Boxed: its three styles make it the largest by far.
  Button(Box<ButtonNode>),
}

pub(crate) struct Flex {
  pub(crate) column: bool,
  pub(crate) grow: bool,
  pub(crate) wrap: bool,
  pub(crate) reverse: bool,
  pub(crate) gap: u32,
  pub(crate) padding_vertical: Option<u32>,
  pub(crate) padding_horizontal: Option<u32>,
  pub(crate) align: Align,
  pub(crate) justify: Justify,
}

pub(crate) struct GridNode {
  pub(crate) columns: u32,
  pub(crate) gap: u32,
  pub(crate) row_gap: Option<u32>,
  pub(crate) column_gap: Option<u32>,
  pub(crate) padding_vertical: Option<u32>,
  pub(crate) padding_horizontal: Option<u32>,
}

pub(crate) struct ScrollNode {
  /// Written by scrolling, read for the offset.
  pub(crate) signal: Signal<f32>,
  /// The offset asked for; layout shows it clamped to the content.
  pub(crate) offset: f32,
  /// The offset shown at the last layout.
  pub(crate) shown: f32,
  /// How far the child can scroll, from the last layout.
  pub(crate) most: f32,
  /// Scrolls along x, with its child at full width.
  pub(crate) horizontal: bool,
  /// The scroll bar's style, when it has one.
  pub(crate) bar: Option<Style>,
  /// The bar's track and handle from the last layout, in the node's own
  /// coordinates, while the child is longer than the box.
  pub(crate) handle: Option<(Rect, Rect)>,
  /// The box's length along its axis at the last layout.
  pub(crate) length: f32,
  /// A touchpad fling and a dragged child's release keep it moving.
  pub(crate) kinetic: bool,
  /// A touchpad, drag or fling may go past either end.
  pub(crate) overshoots: bool,
  /// How far past an end the child is shown, in logical pixels: below 0
  /// before the start, above it past the end.
  pub(crate) overshoot: f32,
  pub(crate) motion: Motion,
  /// What a list moved the offset by since a motion last looked.
  pub(crate) shifts: Option<Rc<Cell<f32>>>,
  /// Told whether the view is moving.
  pub(crate) moving: Option<Rc<Cell<bool>>>,
}

pub(crate) struct ImageNode {
  /// Names the image for the renderer's playback of a GIF and the
  /// platform's repaint of its next frame.
  pub(crate) id: u64,
  pub(crate) source: Option<PathBuf>,
  pub(crate) width: u32,
  pub(crate) height: u32,
  pub(crate) fit: Fit,
  pub(crate) filters: ImageFilters,
  pub(crate) playing: bool,
  pub(crate) transition: ImageTransition,
  /// The picture a running transition leaves.
  pub(crate) leaving: Option<Leaving>,
  /// How many transitions it has started, to tell the running one.
  pub(crate) transitions: u64,
}

pub(crate) struct IconNode {
  /// `None` until the icon's binding first runs.
  pub(crate) source: Option<IconSource>,
  pub(crate) size: u32,
  pub(crate) tint: Option<Color>,
}

pub(crate) struct ButtonNode {
  pub(crate) hover_style: Style,
  pub(crate) pressed_style: Style,
  pub(crate) focus_style: Style,
  /// Kept in the reactive runtime, which drops the user's closures when
  /// the button's owner goes.
  pub(crate) handlers: Signal<Handlers>,
  /// Written while the pointer is over the button or inside it.
  pub(crate) hover_signal: Option<Signal<bool>>,
  /// Written with where the pointer is while it is over the button or
  /// dragging it.
  pub(crate) position_signal: Option<Signal<Option<Point>>>,
  /// What a drag of it carries, kept in the reactive runtime as the
  /// handlers are.
  pub(crate) drag_data: Option<Signal<Option<DragValue>>>,
  /// Written while a drag it would take a drop from is over it.
  pub(crate) drop_hovered: Option<Signal<bool>>,
  /// Off, the pointer passes through it and Tab skips it.
  pub(crate) enabled: bool,
  /// The cursor over it.
  pub(crate) cursor: CursorShape,
  /// Its long press, made the first time it is held.
  pub(crate) long_press: Option<Timer>,
  pub(crate) hovered: bool,
  pub(crate) pressed: bool,
  /// Focused while its window has the keyboard.
  pub(crate) focused: bool,
}

/// The layer-surface settings of the window.
pub(crate) struct Surface {
  pub(crate) anchors: Anchors,
  pub(crate) width: u32,
  pub(crate) height: u32,
  pub(crate) exclusive_zone: Option<i32>,
  pub(crate) layer: Layer,
  pub(crate) margins: Margins,
  pub(crate) keyboard_focus: KeyboardFocus,
  pub(crate) namespace: String,
  pub(crate) mask: Option<Region>,
  /// A panel's or floating window's blur region.
  pub(crate) blur: Option<Region>,
  /// Whether a panel or floating window keeps the screen from idling.
  pub(crate) inhibit_idle: bool,
  /// Whether it takes the compositor's shortcuts while it has the keys.
  pub(crate) inhibit_shortcuts: bool,
  /// Written by the platform: whether the compositor honors that.
  pub(crate) shortcuts_inhibited: Option<Signal<bool>>,
  /// Shared by the windows in one focus grab.
  pub(crate) focus_grab: Option<Signal<bool>>,
  /// The opacity the compositor shows the window at, within 0 to 1.
  pub(crate) compositor_opacity: f32,
  /// Whether a value outside 0 to 1 was logged, which happens once.
  pub(crate) opacity_logged: bool,
  /// The part the compositor is told holds anything to see.
  pub(crate) visible_mask: Option<Region>,
  /// Something changed since the platform last read the settings.
  pub(crate) changed: bool,
}

impl Default for Surface {
  fn default() -> Surface {
    Surface {
      anchors: Anchors::NONE,
      width: 0,
      height: 0,
      exclusive_zone: None,
      layer: Layer::Top,
      margins: Margins::default(),
      keyboard_focus: KeyboardFocus::None,
      namespace: String::from(DEFAULT_NAMESPACE),
      mask: None,
      blur: None,
      inhibit_idle: false,
      inhibit_shortcuts: false,
      shortcuts_inhibited: None,
      focus_grab: None,
      compositor_opacity: 1.0,
      opacity_logged: false,
      visible_mask: None,
      changed: false,
    }
  }
}

/// A style change fading in from `from`, `t` of the way.
pub(crate) struct Fade {
  pub(crate) from: Style,
  pub(crate) t: f32,
  pub(crate) generation: u64,
}

/// What a property change invalidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
  None,
  Paint,
  /// A paint change that moves where the node is drawn: its old area is
  /// repainted now, and its new one once it is placed again.
  Place,
  Layout,
}

/// Borrows the tree for an effect or a pointer event.
pub(crate) fn borrow(tree: &RefCell<Tree>) -> RefMut<'_, Tree> {
  tree
    .try_borrow_mut()
    .expect("invariant: a window's tree is not borrowed while effects or handlers run")
}

impl Node {
  fn new(name: &'static str, path: Rc<str>, cx: Cx, kind: NodeKind) -> Node {
    Node {
      name,
      path,
      cx,
      style: Style::new(),
      kind,
      groups: Vec::new(),
      next_child: 0,
      rect: None,
      content: Rect::default(),
      painted: Rect::default(),
      bounds: Rect::default(),
      inner_bounds: Rect::default(),
      window_bounds: Rect::default(),
      window_painted: Rect::default(),
      drawn: Rect::default(),
      transform: None,
      clips: false,
      casts: false,
      shadow_refused: false,
      fade: None,
      fades: 0,
      changed: true,
      visible: true,
      slide: None,
      displace: Point::default(),
      slides: 0,
      geometry: None,
      written: None,
      measure: None,
      measured: None,
    }
  }

  /// The style in effect: a button's focus, hover and pressed styles laid
  /// over its own, in that order.
  pub(crate) fn effective_style(&self) -> Style {
    let NodeKind::Button(button) = &self.kind else {
      return self.style;
    };
    let mut style = self.style;
    if button.focused {
      style = style.merge(button.focus_style);
    }
    if button.hovered {
      style = style.merge(button.hover_style);
      if button.pressed {
        style = style.merge(button.pressed_style);
      }
    }
    style
  }

  /// Whether a focus, hover or pressed style is laid over its own.
  fn in_state(&self) -> bool {
    matches!(&self.kind, NodeKind::Button(button) if button.focused || button.hovered)
  }

  /// The style as drawn: the style in effect, or partway to it while a
  /// change fades in.
  pub(crate) fn shown_style(&self) -> Style {
    match &self.fade {
      Some(fade) => fade.from.lerp(&self.effective_style(), fade.t),
      None => self.effective_style(),
    }
  }

  /// Whether the node cuts its children to the inside of its border: a
  /// clipping style or a scroll view.
  pub(crate) fn clips(&self) -> bool {
    self.effective_style().clips() || matches!(self.kind, NodeKind::Scroll(_) | NodeKind::Input(_))
  }

  /// Whether the node draws anything itself, in its style in effect. The
  /// window always does: its area is what a repaint clears.
  pub(crate) fn draws(&self) -> bool {
    let style = self.effective_style();
    matches!(
      self.kind,
      NodeKind::Root
        | NodeKind::Text(_)
        | NodeKind::Icon(_)
        | NodeKind::Image(_)
        | NodeKind::Capture(_)
        | NodeKind::Canvas(_)
        | NodeKind::Input(_)
    ) || style.background.is_some()
      || style.gradient.is_some()
      || style.shadow.is_some()
      // A fade still shows what the style had.
      || self.fade.is_some()
      || style.border.is_some()
  }

  /// The padding in effect: a row's, column's or grid's own wins over its
  /// style's, on both sides of its axis.
  pub(crate) fn padding(&self) -> Padding {
    let style = self.effective_style().padding.unwrap_or_default();
    let (vertical, horizontal) = match &self.kind {
      NodeKind::Flex(flex) => (flex.padding_vertical, flex.padding_horizontal),
      NodeKind::Grid(grid) => (grid.padding_vertical, grid.padding_horizontal),
      _ => return style,
    };
    let (top, bottom) = vertical.map_or((style.top, style.bottom), |own| (own, own));
    let (left, right) = horizontal.map_or((style.left, style.right), |own| (own, own));
    Padding {
      top,
      right,
      bottom,
      left,
    }
  }
}

impl Tree {
  /// A tree holding only the window's root node, a `name` such as
  /// `"PanelWindow"`, owned by `cx`, which is anchored at the root.
  pub(crate) fn new(
    this: Weak<RefCell<Tree>>,
    name: &'static str,
    output: Output,
    cx: Cx,
    path: Rc<str>,
  ) -> Tree {
    let mut taffy = TaffyTree::new();
    let anchor = cx.anchor.clone();
    let root = taffy
      .new_leaf_with_context(
        taffy::Style::default(),
        Node::new(name, path, cx, NodeKind::Root),
      )
      .expect("invariant: adding a leaf to taffy never fails");
    if let Some(anchor) = anchor {
      anchor.set(this.clone(), root);
    }
    Tree {
      this,
      taffy,
      root,
      output,
      surface: Surface::default(),
      size: Size::default(),
      needs_layout: true,
      fonts_added: 0,
      needs_place: false,
      damage: Vec::new(),
      pointer: Pointer::default(),
      keyboard: Keyboard::default(),
      popups: Vec::new(),
      popup: None,
      floating: None,
      captures: Vec::new(),
      images: Vec::new(),
      geometry: Vec::new(),
      measured: Vec::new(),
      reveal: None,
      offsets: Vec::new(),
      shown_since_layout: Vec::new(),
    }
  }

  pub(crate) fn node(&self, id: NodeId) -> Option<&Node> {
    self.taffy.get_node_context(id)
  }

  pub(crate) fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
    self.taffy.get_node_context_mut(id)
  }

  /// Whether node `id` and every node above it are shown.
  pub(crate) fn shown(&self, id: NodeId) -> bool {
    let mut at = Some(id);
    while let Some(current) = at {
      if self.node(current).is_some_and(|node| !node.visible) {
        return false;
      }
      at = self.taffy.parent(current);
    }
    true
  }

  /// Gives the focus to the first field in Tab order under `id`, just
  /// shown, whose `focus` is still true, as it asked while hidden.
  pub(crate) fn focus_shown_field(&mut self, id: NodeId) {
    let mut pending = vec![id];
    while let Some(next) = pending.pop() {
      let Some(node) = self.node(next) else {
        continue;
      };
      if !node.visible {
        continue;
      }
      if matches!(&node.kind, NodeKind::Input(input) if input.wants_focus) {
        self.focus(Some(next));
        return;
      }
      let mut children = self.taffy.children(next).unwrap_or_default();
      children.reverse();
      pending.extend(children);
    }
  }

  /// `id`'s shown children in the order they are drawn, and so found by
  /// the pointer last first: by their style's `z`, lowest first, and in
  /// child order where it is equal.
  pub(crate) fn drawn_children(&self, id: NodeId) -> Vec<NodeId> {
    let mut children = self.taffy.children(id).unwrap_or_default();
    children.retain(|&child| self.node(child).is_some_and(|node| node.visible));
    // A stable sort keeps child order for equal z.
    children.sort_by_cached_key(|&child| {
      self
        .node(child)
        .and_then(|node| node.effective_style().z)
        .unwrap_or(0)
    });
    children
  }

  /// Applies the window's own properties and adds its child, which the
  /// caller already turned into an element.
  pub(crate) fn mount_panel(&mut self, panel: PanelWindow, child: Option<Element>) {
    let root = self.root;
    let PanelWindow {
      output: _,
      anchors,
      width,
      height,
      exclusive_zone,
      layer,
      margins,
      keyboard_focus,
      namespace,
      hints,
      on_key,
      style,
      child: _,
    } = panel;
    self.keyboard.handler = on_key;
    self.bind_opt(root, anchors, "anchors", |tree, _, value| {
      tree.surface.set(|s| &mut s.anchors, value);
    });
    self.bind_opt(root, width, "width", |tree, _, value| {
      tree.surface.set(|s| &mut s.width, value);
    });
    self.bind_opt(root, height, "height", |tree, _, value| {
      tree.surface.set(|s| &mut s.height, value);
    });
    self.bind_opt(root, exclusive_zone, "exclusive_zone", |tree, _, value| {
      tree.surface.set(|s| &mut s.exclusive_zone, Some(value));
    });
    self.bind_opt(root, layer, "layer", |tree, _, value| {
      tree.surface.set(|s| &mut s.layer, value);
    });
    self.bind_opt(root, margins, "margins", |tree, _, value| {
      tree.surface.set(|s| &mut s.margins, value);
    });
    self.bind_opt(root, keyboard_focus, "keyboard_focus", |tree, _, value| {
      tree.surface.set(|s| &mut s.keyboard_focus, value);
    });
    self.bind_opt(root, namespace, "namespace", |tree, _, value| {
      tree.surface.set(|s| &mut s.namespace, value);
    });
    self.bind_compositor_hints(hints);
    self.mount_root(style, child);
    self.surface.changed = true;
  }

  /// Binds what a window asks of the compositor: its input mask, opacity,
  /// visible mask, blur region, whether it inhibits idle and shortcuts,
  /// and the signals the platform writes.
  pub(crate) fn bind_compositor_hints(&mut self, hints: Hints) {
    let Hints {
      mask,
      compositor_opacity,
      visible_mask,
      blur,
      inhibit_idle,
      inhibit_shortcuts,
      shortcuts_inhibited,
      focus_grab,
    } = hints;
    self.bind_opt(self.root, mask, "mask", |tree, _, value| {
      tree.surface.set(|s| &mut s.mask, value);
    });
    self.bind_opt(
      self.root,
      compositor_opacity,
      "compositor_opacity",
      |tree, id, asked| {
        // Hyprland ends the connection on a value outside 0 to 1.
        let opacity = if asked.is_nan() {
          1.0
        } else {
          asked.clamp(0.0, 1.0)
        };
        if opacity != asked && !std::mem::replace(&mut tree.surface.opacity_logged, true) {
          let path = tree.node(id).map_or("", |node| &node.path);
          tracing::warn!(
            "{path}.compositor_opacity of {asked} is outside 0 to 1, so {opacity} is used"
          );
        }
        tree.surface.set(|s| &mut s.compositor_opacity, opacity);
      },
    );
    self.bind_opt(self.root, visible_mask, "visible_mask", |tree, _, value| {
      tree.surface.set(|s| &mut s.visible_mask, value);
    });
    self.bind_opt(self.root, blur, "blur", |tree, _, value| {
      tree.surface.set(|s| &mut s.blur, value);
    });
    self.bind_opt(self.root, inhibit_idle, "inhibit_idle", |tree, _, value| {
      tree.surface.set(|s| &mut s.inhibit_idle, value);
    });
    self.bind_opt(
      self.root,
      inhibit_shortcuts,
      "inhibit_shortcuts",
      |tree, _, value| tree.surface.set(|s| &mut s.inhibit_shortcuts, value),
    );
    self.surface.shortcuts_inhibited = shortcuts_inhibited;
    self.surface.focus_grab = focus_grab;
  }

  /// Applies the root's style and adds its child, as a panel's or a
  /// popup's.
  pub(crate) fn mount_root(&mut self, style: Option<Prop<Style>>, child: Option<Element>) {
    let root = self.root;
    self.bind_opt(root, style, "style", set_style);
    if let Some(child) = child {
      let cx = self.node(root).map(|node| node.cx.clone());
      let cx = cx.expect("invariant: the root node exists while the tree does");
      let child = self.mount(child, &cx, root);
      self.set_group(root, 0, vec![child]);
    }
  }

  /// Turns `element` and everything in it into nodes under a new child
  /// owner of `parent_cx`. The caller adds the returned node to `parent`.
  pub(crate) fn mount(&mut self, element: Element, parent_cx: &Cx, parent: NodeId) -> NodeId {
    let Element {
      name,
      common,
      mut kind,
      children,
    } = element;
    let index = self.next_index(parent);
    let parent_path = self.node(parent).map_or("", |node| &node.path);
    let path: Rc<str> = match index {
      Some(index) => format!("{parent_path}/{name}[{index}]"),
      None => format!("{parent_path}/{name}"),
    }
    .into();
    let anchor = Rc::new(Anchor::default());
    let cx = parent_cx
      .child()
      .named(path.to_string())
      .at(Rc::clone(&anchor));

    // The node starts with defaults; the properties are applied below.
    let node_kind = match &mut kind {
      ElementKind::Flex(props) => NodeKind::Flex(Flex {
        column: props.column,
        grow: props.grow,
        wrap: false,
        reverse: false,
        gap: 0,
        padding_vertical: None,
        padding_horizontal: None,
        align: Align::default(),
        justify: Justify::default(),
      }),
      ElementKind::Grid(_) => NodeKind::Grid(GridNode {
        columns: 1,
        gap: 0,
        row_gap: None,
        column_gap: None,
        padding_vertical: None,
        padding_horizontal: None,
      }),
      ElementKind::Stack => NodeKind::Stack,
      ElementKind::Spacer => NodeKind::Spacer,
      ElementKind::Text(props) => {
        let mut text = TextNode::new();
        text.on_link = props.on_link.take();
        NodeKind::Text(text)
      }
      ElementKind::Input(props) => NodeKind::Input(Box::new(InputNode::new(
        props.text,
        cx.signal(props.on_accept.take()),
        props.multiline,
      ))),
      ElementKind::Scroll(props) => NodeKind::Scroll(Box::new(ScrollNode {
        signal: props.offset.unwrap_or_else(|| cx.signal(0.0)),
        offset: 0.0,
        shown: 0.0,
        most: 0.0,
        horizontal: false,
        bar: None,
        handle: None,
        length: 0.0,
        kinetic: true,
        overshoots: true,
        overshoot: 0.0,
        motion: Motion::default(),
        shifts: props.shifts.take(),
        moving: props.moving.take(),
      })),
      ElementKind::Image(_) => NodeKind::Image(ImageNode::new()),
      ElementKind::Capture(_) => NodeKind::Capture(CaptureNode::new()),
      ElementKind::Canvas(_) => NodeKind::Canvas(CanvasNode::default()),
      ElementKind::Icon(_) => NodeKind::Icon(IconNode {
        source: None,
        size: crate::Icon::DEFAULT_SIZE,
        tint: None,
      }),
      ElementKind::Button(props) => NodeKind::Button(Box::new(ButtonNode {
        hover_style: Style::new(),
        pressed_style: Style::new(),
        focus_style: Style::new(),
        handlers: cx.signal(std::mem::take(&mut props.handlers)),
        hover_signal: props.hovered,
        position_signal: props.position,
        drag_data: props
          .drag_data
          .take()
          .map(|data| drag_data_signal(&cx, &path, data)),
        drop_hovered: props.drop_hovered,
        enabled: true,
        cursor: CursorShape::Pointer,
        long_press: None,
        hovered: false,
        pressed: false,
        focused: false,
      })),
    };
    let id = self
      .taffy
      .new_leaf_with_context(
        taffy::Style::default(),
        Node::new(name, Rc::clone(&path), cx.clone(), node_kind),
      )
      .expect("invariant: adding a leaf to taffy never fails");
    anchor.set(self.this.clone(), id);
    if let Some(node) = self.node_mut(id) {
      node.geometry = common.geometry;
      node.measure = common.measure;
    }

    self.bind_opt(id, common.style, "style", set_style);
    self.bind_opt(id, common.visible, "visible", set_visible);
    self.bind_kind(id, kind);
    self.mount_children(id, children, &cx, &path);
    id
  }

  /// Applies the properties only one kind of widget has.
  fn bind_kind(&mut self, id: NodeId, kind: ElementKind) {
    match kind {
      ElementKind::Flex(props) => {
        self.bind_opt(id, props.gap, "gap", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.gap, value))
        });
        self.bind_opt(id, props.padding_vertical, "padding", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.padding_vertical, Some(value)))
        });
        self.bind_opt(
          id,
          props.padding_horizontal,
          "padding",
          |tree, id, value| {
            tree.update_flex(id, |flex| {
              replace(&mut flex.padding_horizontal, Some(value))
            })
          },
        );
        self.bind_opt(id, props.align, "align", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.align, value))
        });
        self.bind_opt(id, props.justify, "justify", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.justify, value))
        });
        self.bind_opt(id, props.wrap, "wrap", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.wrap, value))
        });
        self.bind_opt(id, props.reverse, "reverse", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.reverse, value))
        });
        self.bind_opt(id, props.column_binding, "horizontal", |tree, id, value| {
          tree.update_flex(id, |flex| replace(&mut flex.column, value))
        });
      }
      ElementKind::Grid(props) => self.bind_grid(id, props),
      ElementKind::Text(props) => {
        // The old selection's bytes may not be in the new text.
        self.bind_opt(id, props.text, "text", |tree, id, value| {
          tree.update_text(id, |text| {
            let changed = text.set_content(value);
            text.forget_selection_if(changed)
          })
        });
        self.bind_opt(id, props.spans, "spans", |tree, id, value| {
          tree.update_text(id, |text| {
            let changed = text.set_spans(value);
            text.forget_selection_if(changed)
          })
        });
        self.bind_opt(id, props.selectable, "selectable", Tree::set_selectable);
        self.bind_opt(id, props.elide, "elide", |tree, id, value| {
          tree.update_text(id, |text| text.set_elide(value))
        });
        self.bind_opt(id, props.max_lines, "max_lines", |tree, id, value| {
          tree.update_text(id, |text| text.set_max_lines(value))
        });
      }
      ElementKind::Input(props) => {
        let signal = props.text;
        self.bind(
          id,
          (move || signal.get()).into_prop(),
          "text",
          |tree, id, value| {
            tree.update_input(id, |input| {
              input.edit.set_text(value);
              input.refresh();
            });
          },
        );
        self.bind_opt(id, props.placeholder, "placeholder", |tree, id, value| {
          tree.update_input(id, |input| {
            input.placeholder = value;
            input.refresh();
          });
        });
        self.bind_opt(id, props.wrap, "wrap", |tree, id, value| {
          tree.update_input(id, |input| {
            input.text.set_wrap(value);
          });
        });
        self.bind_opt(id, props.password, "password", |tree, id, value| {
          tree.update_input(id, |input| {
            input.password = value;
            input.refresh();
          });
        });
        self.bind_opt(id, props.focus, "focus", |tree, id, value| {
          if let Some(NodeKind::Input(input)) = tree.node_mut(id).map(|node| &mut node.kind) {
            input.wants_focus = value;
          }
          if value {
            tree.focus(Some(id));
          }
        });
      }
      ElementKind::Scroll(props) => {
        self.bind_opt(id, props.horizontal, "horizontal", |tree, id, value| {
          tree.update_scroll(id, |scroll| replace(&mut scroll.horizontal, value));
        });
        self.bind_opt(id, props.kinetic, "kinetic", |tree, id, value| {
          tree.update_scroll(id, |scroll| {
            scroll.kinetic = value;
            Change::None
          });
        });
        self.bind_opt(id, props.overshoot, "overshoot", |tree, id, value| {
          tree.update_scroll(id, |scroll| {
            scroll.overshoots = value;
            scroll.stop_overshoot()
          });
        });
        self.bind_opt(id, props.scroll_bar, "scroll_bar", |tree, id, value| {
          // Placed again to find the handle, which repaints it.
          tree.update_scroll(id, |scroll| match replace(&mut scroll.bar, value) {
            Change::None => Change::None,
            _ => Change::Place,
          });
        });
        if let Some(NodeKind::Scroll(scroll)) = self.node(id).map(|node| &node.kind) {
          let signal = scroll.signal;
          self.bind(
            id,
            (move || signal.get()).into_prop(),
            "offset",
            |tree, id, value| {
              tree.update(id, |node| match &mut node.kind {
                NodeKind::Scroll(scroll) => replace(&mut scroll.offset, value),
                _ => Change::None,
              });
            },
          );
        }
      }
      ElementKind::Image(props) => self.bind_image(id, props),
      ElementKind::Capture(props) => self.bind_capture(id, props),
      ElementKind::Canvas(paint) => self.bind_canvas(id, paint),
      ElementKind::Icon(props) => self.bind_icon(id, props),
      ElementKind::Button(props) => {
        self.bind_opt(id, props.hover_style, "hover_style", |tree, id, value| {
          tree.update_button_style(id, |button| button.hover_style = value);
        });
        self.bind_opt(
          id,
          props.pressed_style,
          "pressed_style",
          |tree, id, value| {
            tree.update_button_style(id, |button| button.pressed_style = value);
          },
        );
        self.bind_opt(id, props.focus_style, "focus_style", |tree, id, value| {
          tree.update_button_style(id, |button| button.focus_style = value);
        });
        self.bind_opt(id, props.cursor, "cursor", |tree, id, value| {
          if let Some(NodeKind::Button(button)) = tree.node_mut(id).map(|node| &mut node.kind) {
            button.cursor = value;
          }
        });
        // Before `focus`, so a button made disabled never takes it.
        if let Some(enabled) = props.enabled {
          self.bind_enabled(id, enabled);
        }
        self.bind_opt(id, props.focus, "focus", |tree, id, value| {
          if value && tree.enabled(id) {
            tree.focus(Some(id));
          }
        });
      }
      ElementKind::Stack | ElementKind::Spacer => {}
    }
  }

  fn bind_grid(&mut self, id: NodeId, props: GridProps) {
    self.bind_opt(id, props.columns, "columns", |tree, id, value| {
      tree.update_grid(id, |grid| replace(&mut grid.columns, value))
    });
    self.bind_opt(id, props.gap, "gap", |tree, id, value| {
      tree.update_grid(id, |grid| replace(&mut grid.gap, value))
    });
    self.bind_opt(id, props.row_gap, "row_gap", |tree, id, value| {
      tree.update_grid(id, |grid| replace(&mut grid.row_gap, Some(value)))
    });
    self.bind_opt(id, props.column_gap, "column_gap", |tree, id, value| {
      tree.update_grid(id, |grid| replace(&mut grid.column_gap, Some(value)))
    });
    self.bind_opt(id, props.padding_vertical, "padding", |tree, id, value| {
      tree.update_grid(id, |grid| replace(&mut grid.padding_vertical, Some(value)))
    });
    self.bind_opt(
      id,
      props.padding_horizontal,
      "padding",
      |tree, id, value| {
        tree.update_grid(id, |grid| {
          replace(&mut grid.padding_horizontal, Some(value))
        })
      },
    );
  }

  /// Mounts a container's children, each `child` call as one group and
  /// each `children_keyed` call as a group its effect fills.
  fn mount_children(&mut self, id: NodeId, children: Vec<ElementChild>, cx: &Cx, path: &Rc<str>) {
    for child in children {
      let Some(node) = self.node(id) else {
        return;
      };
      let group = node.groups.len();
      match child {
        ElementChild::One(element) => {
          let child = self.mount(*element, cx, id);
          self.set_group(id, group, vec![child]);
        }
        ElementChild::Keyed(mount) => {
          self.set_group(id, group, Vec::new());
          mount(KeyedSlot {
            tree: self.this.clone(),
            container: id,
            group,
            cx: cx.clone(),
            path: Rc::clone(path),
          });
        }
      }
    }
  }

  /// The number in the path of `parent`'s next child, such as the 2 in
  /// `Row/Button[2]`. `None` for the only child of a window or button.
  fn next_index(&mut self, parent: NodeId) -> Option<usize> {
    let node = self.node_mut(parent)?;
    if !matches!(
      node.kind,
      NodeKind::Flex(_) | NodeKind::Grid(_) | NodeKind::Stack
    ) {
      return None;
    }
    let index = node.next_child;
    node.next_child += 1;
    Some(index)
  }

  /// Replaces the nodes of one child group of `parent`, adding the group if
  /// it is the next one.
  pub(crate) fn set_group(&mut self, parent: NodeId, group: usize, nodes: Vec<NodeId>) {
    let Some(node) = self.node_mut(parent) else {
      return;
    };
    if group == node.groups.len() {
      node.groups.push(nodes);
    } else if let Some(slot) = node.groups.get_mut(group) {
      *slot = nodes;
    }
    let children: Vec<NodeId> = node.groups.iter().flatten().copied().collect();
    self
      .taffy
      .set_children(parent, &children)
      .expect("invariant: every node in a child group is in the tree");
    self.needs_layout = true;
  }

  /// Removes `id` and everything under it, and repaints where it was.
  /// Their owners are disposed by whoever owns them, after the tree is
  /// released.
  pub(crate) fn unmount(&mut self, id: NodeId) {
    if let Some(node) = self.taffy.get_node_context(id)
      && node.rect.is_some()
    {
      add_damage(&mut self.damage, node.window_bounds);
    }
    let mut removed = Vec::new();
    let mut pending = vec![id];
    while let Some(next) = pending.pop() {
      removed.push(next);
      if let Ok(children) = self.taffy.children(next) {
        pending.extend(children);
      }
    }
    self.pointer.forget(&removed);
    self.captures.retain(|node| !removed.contains(node));
    self.images.retain(|node| !removed.contains(node));
    self.keyboard.forget(&removed);
    for node in removed.into_iter().rev() {
      // `remove` only fails for a node that isn't in the tree.
      let _ = self.taffy.remove(node);
    }
    self.needs_layout = true;
  }

  /// Applies `prop` to node `id` with `apply`: once now for a value, or
  /// from an effect under the node's owner for a binding. The effect is
  /// named after the widget path and `name`, for log lines.
  pub(crate) fn bind<T: 'static>(
    &mut self,
    id: NodeId,
    prop: Prop<T>,
    name: &str,
    apply: fn(&mut Tree, NodeId, T),
  ) {
    match prop.0 {
      PropKind::Value(value) => apply(self, id, value),
      PropKind::Binding(mut compute) => {
        let Some(node) = self.node(id) else {
          return;
        };
        let label = format!("{}.{name}", node.path);
        let tree = self.this.clone();
        node
          .cx
          .effect(move || {
            // The user's closure runs before the tree is borrowed.
            let value = compute();
            if let Some(tree) = tree.upgrade() {
              apply(&mut borrow(&tree), id, value);
            }
          })
          .named(label);
      }
    }
  }

  pub(crate) fn bind_opt<T: 'static>(
    &mut self,
    id: NodeId,
    prop: Option<Prop<T>>,
    name: &str,
    apply: fn(&mut Tree, NodeId, T),
  ) {
    if let Some(prop) = prop {
      self.bind(id, prop, name, apply);
    }
  }

  /// Runs `f` on node `id`, if it still exists, and records and returns
  /// what it invalidated.
  pub(crate) fn update(&mut self, id: NodeId, f: impl FnOnce(&mut Node) -> Change) -> Change {
    let Some(node) = self.node_mut(id) else {
      return Change::None;
    };
    let change = f(node);
    self.invalidate(id, change);
    change
  }

  /// Runs `f` on node `id` and works out from the style in effect before
  /// and after whether the change needs a layout or only a repaint.
  pub(crate) fn update_style(&mut self, id: NodeId, f: impl FnOnce(&mut Node)) -> Change {
    let mut fade = None;
    let change = self.update(id, |node| {
      let shown = node.shown_style();
      let before = node.effective_style();
      let was_in_state = node.in_state();
      f(node);
      let after = node.effective_style();
      let back = was_in_state && !node.in_state();
      if before == after {
        return Change::None;
      }
      node.fade = None;
      if !before.same_layout(&after) {
        return Change::Layout;
      }
      // A node not yet drawn shows its style at once.
      let transition = if back {
        after.transition_out.or(after.transition)
      } else {
        after.transition
      };
      if let Some(transition) = transition.filter(|transition| !transition.is_instant())
        && node.rect.is_some()
      {
        node.fades += 1;
        node.fade = Some(Fade {
          from: shown,
          t: 0.0,
          generation: node.fades,
        });
        fade = Some((node.fades, transition));
      }
      // A shadow reaches past the box, so the area painted is worked
      // out again, to cover a fade's start as well as its end.
      if before.shadow != after.shadow {
        Change::Layout
      // Against what showed: a fade cut short leaves it partway.
      } else if !shown.same_transform(&after) {
        Change::Place
      } else {
        Change::Paint
      }
    });
    if let Some((generation, transition)) = fade {
      self.start_fade(id, generation, transition);
    }
    change
  }

  /// Advances node `id`'s fade number `generation` with the animation
  /// clock, repainting it each tick, until it ends or another replaces it.
  fn start_fade(&self, id: NodeId, generation: u64, transition: Transition) {
    let tree = self.this.clone();
    let mut started = animation::start_time();
    let label = self.node(id).map_or_else(
      || Rc::from("a fade"),
      |node| format!("{}.transition", node.path).into(),
    );
    let busy = self.node(id).and_then(|node| node.cx.busy());
    animation::start(
      label,
      busy,
      Box::new(move |now| {
        let Some(tree) = tree.upgrade() else {
          return false;
        };
        let Ok(mut tree) = tree.try_borrow_mut() else {
          return true;
        };
        let begun = *started.get_or_insert(now);
        let waiting = transition.waiting(begun, now);
        let (t, ended) = transition.progress(begun, now);
        let mut mine = false;
        tree.update(id, |node| {
          if node
            .fade
            .as_ref()
            .is_none_or(|fade| fade.generation != generation)
          {
            return Change::None;
          }
          mine = true;
          // A fading transform moves through areas neither end covers.
          let moves = node
            .fade
            .as_ref()
            .is_some_and(|fade| !fade.from.same_transform(&node.effective_style()));
          if ended {
            node.fade = None;
          } else if waiting {
            // Nothing on screen changes until the delay is over.
            return Change::None;
          } else if let Some(fade) = &mut node.fade {
            fade.t = t;
          }
          if moves { Change::Place } else { Change::Paint }
        });
        mine && !ended
      }),
    );
  }

  /// Asks every child of `container` to slide over `transition` from
  /// where it is drawn to where the next layout puts it.
  pub(crate) fn slide_children(&mut self, container: NodeId, transition: Transition) {
    for child in self.taffy.children(container).unwrap_or_default() {
      if let Some(node) = self.node_mut(child) {
        node.slide = Some(transition);
      }
    }
  }

  /// Starts node `id`'s pending slide, now that the layout put it at
  /// `location` in its parent: it is drawn where it was, `displace` away,
  /// and eases to its place. A node not drawn before has nowhere to come
  /// from.
  pub(crate) fn begin_slide(&mut self, id: NodeId, location: Point) {
    let parent = self
      .taffy
      .parent(id)
      .and_then(|parent| self.node(parent)?.rect);
    let Some(node) = self.node_mut(id) else {
      return;
    };
    let Some(transition) = node.slide.take() else {
      return;
    };
    let (Some(old), Some(parent)) = (node.rect, parent) else {
      return;
    };
    // Against the parent, so a parent that moved too moves it at once.
    let by = Point::new(
      old.x - parent.x + node.displace.x - location.x,
      old.y - parent.y + node.displace.y - location.y,
    );
    if by == node.displace {
      return;
    }
    node.displace = by;
    node.slides += 1;
    let generation = node.slides;
    let label: Rc<str> = format!("{}.displaced", node.path).into();
    let busy = node.cx.busy();
    let tree = self.this.clone();
    let mut started = animation::start_time();
    animation::start(
      label,
      busy,
      Box::new(move |now| {
        let Some(tree) = tree.upgrade() else {
          return false;
        };
        let Ok(mut tree) = tree.try_borrow_mut() else {
          return true;
        };
        let begun = *started.get_or_insert(now);
        let (t, ended) = transition.progress(begun, now);
        let left = if ended { 0.0 } else { 1.0 - t };
        let mut mine = false;
        tree.update(id, |node| {
          if node.slides != generation {
            return Change::None;
          }
          mine = true;
          let at = Point::new(by.x * left, by.y * left);
          if std::mem::replace(&mut node.displace, at) == at {
            Change::None
          } else {
            Change::Place
          }
        });
        mine && !ended
      }),
    );
  }

  fn update_flex(&mut self, id: NodeId, f: impl FnOnce(&mut Flex) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Flex(flex) => f(flex),
      _ => Change::None,
    });
  }

  fn update_grid(&mut self, id: NodeId, f: impl FnOnce(&mut GridNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Grid(grid) => f(grid),
      _ => Change::None,
    });
  }

  pub(crate) fn update_scroll(&mut self, id: NodeId, f: impl FnOnce(&mut ScrollNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Scroll(scroll) => f(scroll),
      _ => Change::None,
    });
  }

  fn update_text(&mut self, id: NodeId, f: impl FnOnce(&mut TextNode) -> bool) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Text(text) => {
        if f(text) {
          Change::Layout
        } else {
          Change::None
        }
      }
      _ => Change::None,
    });
  }

  /// Changes a field; anything about it may move the caret, so it is laid
  /// out again, and a focused one's caret is shown.
  pub(crate) fn update_input(&mut self, id: NodeId, f: impl FnOnce(&mut InputNode)) {
    let change = self.update(id, |node| match &mut node.kind {
      NodeKind::Input(input) => {
        f(input);
        Change::Layout
      }
      _ => Change::None,
    });
    if change == Change::Layout && self.keyboard.focused == Some(id) {
      self.reveal = Some(id);
    }
  }

  pub(crate) fn update_image(&mut self, id: NodeId, f: impl FnOnce(&mut ImageNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Image(image) => f(image),
      _ => Change::None,
    });
  }

  pub(crate) fn update_icon(&mut self, id: NodeId, f: impl FnOnce(&mut IconNode) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Icon(icon) => f(icon),
      _ => Change::None,
    });
  }

  pub(crate) fn update_button_style(
    &mut self,
    id: NodeId,
    f: impl FnOnce(&mut ButtonNode),
  ) -> Change {
    self.update_style(id, |node| {
      if let NodeKind::Button(button) = &mut node.kind {
        f(button);
      }
    })
  }

  /// Records that node `id` needs a repaint, or the window a layout.
  pub(crate) fn invalidate(&mut self, id: NodeId, change: Change) {
    match change {
      Change::None => {}
      Change::Paint | Change::Place => {
        // A node that was never laid out is painted after its first
        // layout anyway. Its bounds cover the descendants, which
        // inherit its color and opacity.
        if let Some(node) = self.taffy.get_node_context_mut(id)
          && node.rect.is_some()
        {
          add_damage(&mut self.damage, node.window_bounds);
          // It may draw now, as when a background was added, so a
          // later move repaints where it was.
          node.drawn = node.window_painted;
        }
        self.needs_place |= change == Change::Place;
      }
      Change::Layout => {
        if let Some(node) = self.node_mut(id) {
          node.changed = true;
        }
        self.needs_layout = true;
      }
    }
  }
}

impl Surface {
  /// Sets one field and remembers that the settings changed.
  fn set<T: PartialEq>(&mut self, field: impl FnOnce(&mut Surface) -> &mut T, value: T) {
    let slot = field(self);
    if *slot != value {
      *slot = value;
      self.changed = true;
    }
  }
}

fn set_style(tree: &mut Tree, id: NodeId, style: Style) {
  tree.update_style(id, |node| node.style = style);
}

/// Shows or hides node `id`. Hidden, it and everything in it let go of
/// the keyboard focus and the pointer at once; the next layout frees its
/// space and repaints where it was.
fn set_visible(tree: &mut Tree, id: NodeId, visible: bool) {
  if tree.update(id, |node| replace(&mut node.visible, visible)) == Change::None {
    return;
  }
  if visible {
    // Once the flush is over, so the fields' `focus` bindings have run.
    tree.shown_since_layout.push(id);
    return;
  }
  let mut hidden = Vec::new();
  let mut pending = vec![id];
  while let Some(next) = pending.pop() {
    hidden.push(next);
    pending.extend(tree.taffy.children(next).unwrap_or_default());
  }
  if tree
    .keyboard
    .focused
    .is_some_and(|focused| hidden.contains(&focused))
  {
    tree.focus(None);
  }
  tree.pointer_hide(&hidden);
}

/// The signal holding what a drag of a button carries: `data` itself, or
/// what its binding last gave, from an effect that never touches the tree.
fn drag_data_signal(
  cx: &Cx,
  path: &str,
  data: Prop<Option<DragValue>>,
) -> Signal<Option<DragValue>> {
  match data.0 {
    PropKind::Value(value) => cx.signal(value),
    PropKind::Binding(mut compute) => {
      let signal = cx.signal(None);
      cx.effect(move || signal.set(compute()))
        .named(format!("{path}.drag_data"));
      signal
    }
  }
}

/// Where an icon of `size` logical pixels is drawn in its node's `content`
/// box: a square, centered, on whole pixels. It overflows a smaller box.
pub(crate) fn icon_rect(content: Rect, size: u32) -> Rect {
  centered(content, size, size)
}

/// A `width` by `height` box centered in `content`, on whole logical
/// pixels.
pub(crate) fn centered(content: Rect, width: u32, height: u32) -> Rect {
  let (width, height) = (width as f32, height as f32);
  Rect::new(
    content.x + ((content.width - width) / 2.0).round(),
    content.y + ((content.height - height) / 2.0).round(),
    width,
    height,
  )
}

/// Adds `rect` to the areas to repaint, unless it is empty or already there.
pub(crate) fn add_damage(damage: &mut Vec<Rect>, rect: Rect) {
  if !rect.is_empty() && !damage.contains(&rect) {
    damage.push(rect);
  }
}

/// Stores `value` in `slot` and reports a layout change if it differs.
pub(crate) fn replace<T: PartialEq>(slot: &mut T, value: T) -> Change {
  if *slot == value {
    Change::None
  } else {
    *slot = value;
    Change::Layout
  }
}
