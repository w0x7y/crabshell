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

use std::cell::{RefCell, RefMut};
use std::rc::{Rc, Weak};

use crabshell_reactive::Signal;
use taffy::TaffyTree;

use crate::cx::{Anchor, Cx};
use crate::geometry::{Rect, Size};
use crate::input::{Handlers, Pointer};
use crate::keyed::KeyedSlot;
use crate::prop::{Prop, PropKind};
use crate::style::{Padding, Style};
use crate::text::TextNode;
use crate::widgets::{Align, Element, ElementChild, ElementKind, IconSource, Justify};
use crate::window::{Anchors, Layer, Output, PanelWindow, Window};

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
  /// Areas to repaint, gathered since the renderer last took them.
  pub(crate) damage: Vec<Rect>,
  pub(crate) pointer: Pointer,
  /// Popups its widgets opened, until the platform takes them.
  pub(crate) popups: Vec<Window>,
  /// Set for a popup's own tree.
  pub(crate) popup: Option<PopupState>,
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
  /// `painted` together with every descendant's.
  pub(crate) bounds: Rect,
  /// What the node drew itself at the last layout: `painted` if it has a
  /// background, a border, text or an icon, and nothing otherwise. Moving
  /// or resizing it repaints only this, since its children report their
  /// own; a spacer that grows repaints nothing.
  pub(crate) drawn: Rect,
  /// A layout property changed, so the node is repainted after the next
  /// layout even if it doesn't move.
  pub(crate) changed: bool,
}

pub(crate) enum NodeKind {
  /// The `PanelWindow` itself.
  Root,
  Flex(Flex),
  Stack,
  Spacer,
  Text(TextNode),
  Icon(IconNode),
  Button(ButtonNode),
}

pub(crate) struct Flex {
  pub(crate) column: bool,
  pub(crate) grow: bool,
  pub(crate) gap: u32,
  pub(crate) padding_vertical: Option<u32>,
  pub(crate) padding_horizontal: Option<u32>,
  pub(crate) align: Align,
  pub(crate) justify: Justify,
}

pub(crate) struct IconNode {
  /// `None` until the icon's binding first runs.
  pub(crate) source: Option<IconSource>,
  pub(crate) size: u32,
}

pub(crate) struct ButtonNode {
  pub(crate) hover_style: Style,
  pub(crate) pressed_style: Style,
  /// Kept in the reactive runtime, which drops the user's closures when
  /// the button's owner goes.
  pub(crate) handlers: Signal<Handlers>,
  pub(crate) hovered: bool,
  pub(crate) pressed: bool,
}

/// The layer-surface settings of the window.
#[derive(Default)]
pub(crate) struct Surface {
  pub(crate) anchors: Anchors,
  pub(crate) width: u32,
  pub(crate) height: u32,
  pub(crate) exclusive_zone: Option<i32>,
  pub(crate) layer: Layer,
  /// Something changed since the platform last read the settings.
  pub(crate) changed: bool,
}

/// What a property change invalidates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
  None,
  Paint,
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
      drawn: Rect::default(),
      changed: true,
    }
  }

  /// The style in effect: a button's hover and pressed styles laid over
  /// its own.
  pub(crate) fn effective_style(&self) -> Style {
    match &self.kind {
      NodeKind::Button(button) if button.hovered => {
        let hovered = self.style.merge(button.hover_style);
        if button.pressed {
          hovered.merge(button.pressed_style)
        } else {
          hovered
        }
      }
      _ => self.style,
    }
  }

  /// Whether the node draws anything itself, in its style in effect. The
  /// window always does: its area is what a repaint clears.
  pub(crate) fn draws(&self) -> bool {
    let style = self.effective_style();
    matches!(
      self.kind,
      NodeKind::Root | NodeKind::Text(_) | NodeKind::Icon(_)
    ) || style.background.is_some()
      || style.border.is_some()
  }

  /// The padding in effect: a row's or column's own wins over its style's.
  pub(crate) fn padding(&self) -> Padding {
    let style = self.effective_style().padding.unwrap_or_default();
    match &self.kind {
      NodeKind::Flex(flex) => Padding {
        vertical: flex.padding_vertical.unwrap_or(style.vertical),
        horizontal: flex.padding_horizontal.unwrap_or(style.horizontal),
      },
      _ => style,
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
      damage: Vec::new(),
      pointer: Pointer::default(),
      popups: Vec::new(),
      popup: None,
    }
  }

  pub(crate) fn node(&self, id: NodeId) -> Option<&Node> {
    self.taffy.get_node_context(id)
  }

  pub(crate) fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
    self.taffy.get_node_context_mut(id)
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
      style,
      child: _,
    } = panel;
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
    self.mount_root(style, child);
    self.surface.changed = true;
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
      style,
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
        gap: 0,
        padding_vertical: None,
        padding_horizontal: None,
        align: Align::default(),
        justify: Justify::default(),
      }),
      ElementKind::Stack => NodeKind::Stack,
      ElementKind::Spacer => NodeKind::Spacer,
      ElementKind::Text(_) => NodeKind::Text(TextNode::new()),
      ElementKind::Icon(_) => NodeKind::Icon(IconNode {
        source: None,
        size: crate::Icon::DEFAULT_SIZE,
      }),
      ElementKind::Button(props) => NodeKind::Button(ButtonNode {
        hover_style: Style::new(),
        pressed_style: Style::new(),
        handlers: cx.signal(std::mem::take(&mut props.handlers)),
        hovered: false,
        pressed: false,
      }),
    };
    let id = self
      .taffy
      .new_leaf_with_context(
        taffy::Style::default(),
        Node::new(name, Rc::clone(&path), cx.clone(), node_kind),
      )
      .expect("invariant: adding a leaf to taffy never fails");
    anchor.set(self.this.clone(), id);

    self.bind_opt(id, style, "style", set_style);
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
      }
      ElementKind::Text(props) => {
        self.bind(id, props.text, "text", |tree, id, value| {
          tree.update_text(id, |text| text.set_content(value))
        });
        self.bind_opt(id, props.ellipsis, "ellipsis", |tree, id, value| {
          tree.update_text(id, |text| text.set_ellipsis(value))
        });
        self.bind_opt(id, props.max_lines, "max_lines", |tree, id, value| {
          tree.update_text(id, |text| text.set_max_lines(value))
        });
      }
      ElementKind::Icon(props) => {
        self.bind(id, props.source, "icon", |tree, id, value| {
          tree.update_icon(id, |icon| match replace(&mut icon.source, Some(value)) {
            // A new picture of the same size only needs a repaint.
            Change::None => Change::None,
            _ => Change::Paint,
          })
        });
        self.bind_opt(id, props.size, "size", |tree, id, value| {
          tree.update_icon(id, |icon| replace(&mut icon.size, value))
        });
      }
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
      }
      ElementKind::Stack | ElementKind::Spacer => {}
    }
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
    if !matches!(node.kind, NodeKind::Flex(_) | NodeKind::Stack) {
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
      add_damage(&mut self.damage, node.bounds);
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

  fn bind_opt<T: 'static>(
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
    self.update(id, |node| {
      let before = node.effective_style();
      f(node);
      let after = node.effective_style();
      if before == after {
        Change::None
      } else if before.same_layout(&after) {
        Change::Paint
      } else {
        Change::Layout
      }
    })
  }

  fn update_flex(&mut self, id: NodeId, f: impl FnOnce(&mut Flex) -> Change) {
    self.update(id, |node| match &mut node.kind {
      NodeKind::Flex(flex) => f(flex),
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

  fn update_icon(&mut self, id: NodeId, f: impl FnOnce(&mut IconNode) -> Change) {
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
      Change::Paint => {
        // A node that was never laid out is painted after its first
        // layout anyway. Its bounds cover the descendants, which
        // inherit its color and opacity.
        if let Some(node) = self.taffy.get_node_context_mut(id)
          && node.rect.is_some()
        {
          add_damage(&mut self.damage, node.bounds);
          // It may draw now, as when a background was added, so a
          // later move repaints where it was.
          node.drawn = node.painted;
        }
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

/// Where an icon of `size` logical pixels is drawn in its node's `content`
/// box: a square, centered, on whole pixels. It overflows a smaller box.
pub(crate) fn icon_rect(content: Rect, size: u32) -> Rect {
  let side = size as f32;
  Rect::new(
    content.x + ((content.width - side) / 2.0).round(),
    content.y + ((content.height - side) / 2.0).round(),
    side,
    side,
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
