//! What a screen reader is shown of the windows: each widget's role, name,
//! state and place, read from the window trees when it asks, and the
//! changes heard while one listens. The AT-SPI side is in
//! crabshell-services; nothing here walks a tree unless it is asked.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::task::Waker;

use crate::geometry::Rect;
use crate::tree::{NodeId, NodeKind, Tree};

/// Every window's tree, for a screen reader. The shell provides one in
/// its root context, and each window registers its tree there as it is
/// made; a dropped window leaves on its own.
#[doc(hidden)]
#[derive(Default)]
pub struct Accessibility {
  windows: RefCell<Vec<(u32, Weak<RefCell<Tree>>)>>,
  next: Cell<u32>,
  /// A screen reader listens, so changes are kept for it.
  listening: Cell<bool>,
  events: RefCell<Vec<AccessEvent>>,
  /// Woken when an event is kept.
  waker: RefCell<Option<Waker>>,
}

/// One widget, or a window's root, as `window` and the node's number in
/// its tree. A removed node's number is never given again.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AccessId {
  pub window: u32,
  pub node: u64,
}

/// What a widget is to a screen reader.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessRole {
  /// A panel, floating window or lock surface.
  Frame,
  /// A popup.
  Window,
  /// A row, column, stack, grid, scroll view or spacer.
  Filler,
  Button,
  Label,
  Entry,
  PasswordText,
  Slider,
  Image,
}

/// A widget as a screen reader reads it, at the moment it asked.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct AccessNode {
  pub role: AccessRole,
  pub name: String,
  /// `None` for a window's root, whose parent is the application.
  pub parent: Option<AccessId>,
  /// The shown children, in Tab order.
  pub children: Vec<AccessId>,
  /// Its place among its parent's children, or a root's among the windows.
  pub index: usize,
  /// The laid-out rectangle in the window's logical pixels.
  pub rect: Rect,
  pub focusable: bool,
  pub focused: bool,
  pub enabled: bool,
  /// It and everything around it are shown.
  pub shown: bool,
  /// A root whose window has the keyboard.
  pub active: bool,
  pub editable: bool,
  pub multi_line: bool,
  /// It clicks, as a button does.
  pub clickable: bool,
}

/// A change a screen reader hears of.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessEvent {
  /// A widget shows the keyboard focus, or stops.
  Focused(AccessId, bool),
  NameChanged(AccessId),
  /// A window's root, as its window gets or loses the keyboard.
  Active(AccessId, bool),
}

impl Accessibility {
  /// Adds a tree, and returns its window's number.
  pub(crate) fn register(&self, tree: Weak<RefCell<Tree>>) -> u32 {
    let id = self.next.get();
    self.next.set(id + 1);
    let mut windows = self.windows.borrow_mut();
    windows.retain(|(_, tree)| tree.strong_count() > 0);
    windows.push((id, tree));
    id
  }

  /// Keeps changes for a screen reader while `on`; off, it drops those
  /// not taken.
  pub fn listen(&self, on: bool) {
    self.listening.set(on);
    if !on {
      self.events.borrow_mut().clear();
      // So whatever waits for events sees that nobody listens.
      if let Some(waker) = self.waker.take() {
        waker.wake();
      }
    }
  }

  pub(crate) fn listening(&self) -> bool {
    self.listening.get()
  }

  pub(crate) fn push(&self, event: AccessEvent) {
    self.events.borrow_mut().push(event);
    if let Some(waker) = self.waker.take() {
      waker.wake();
    }
  }

  /// The changes kept since the last call. `waker` is woken at the next.
  pub fn take_events(&self, waker: &Waker) -> Vec<AccessEvent> {
    self.waker.replace(Some(waker.clone()));
    std::mem::take(&mut self.events.borrow_mut())
  }

  fn tree(&self, window: u32) -> Option<Rc<RefCell<Tree>>> {
    let windows = self.windows.borrow();
    let (_, tree) = windows.iter().find(|(id, _)| *id == window)?;
    tree.upgrade()
  }

  /// The roots of the windows shown, oldest first.
  pub fn windows(&self) -> Vec<AccessId> {
    let windows = self.windows.borrow();
    let live = windows.iter().filter_map(|(id, tree)| {
      let tree = tree.upgrade()?;
      let tree = tree.try_borrow().ok()?;
      let shown = tree
        .floating
        .as_ref()
        .is_none_or(|floating| floating.shown());
      shown.then(|| AccessId {
        window: *id,
        node: tree.root.into(),
      })
    });
    live.collect()
  }

  /// Widget `id` as it is now, or `None` once it is gone.
  pub fn node(&self, id: AccessId) -> Option<AccessNode> {
    let rc = self.tree(id.window)?;
    let tree = rc.try_borrow().ok()?;
    let node_id = NodeId::from(id.node);
    let node = tree.node(node_id)?;
    let is_root = node_id == tree.root;
    let at = |node: NodeId| AccessId {
      window: id.window,
      node: node.into(),
    };
    let (parent, index) = if is_root {
      let index = self.windows().iter().position(|root| *root == id);
      (None, index.unwrap_or_default())
    } else {
      let parent = tree
        .taffy
        .parent(node_id)
        .expect("invariant: a node other than the root has a parent");
      let index = exposed_children(&tree, parent)
        .iter()
        .position(|&n| n == node_id);
      (Some(at(parent)), index.unwrap_or_default())
    };
    let role = role(&tree, node_id);
    let focused = tree.keyboard.entered
      && tree.keyboard.focused.map(|focused| exposed(&tree, focused)) == Some(node_id);
    let (enabled, clickable) = match &node.kind {
      NodeKind::Button(button) => (button.enabled, button.enabled),
      _ => (true, false),
    };
    let (editable, multi_line) = match &node.kind {
      NodeKind::Input(input) => (true, input.edit.multiline),
      _ => (false, false),
    };
    let focusable = match role {
      AccessRole::Button => enabled,
      AccessRole::Entry | AccessRole::PasswordText | AccessRole::Slider => true,
      _ => false,
    };
    let children = match role {
      AccessRole::Slider => Vec::new(),
      _ => exposed_children(&tree, node_id),
    };
    Some(AccessNode {
      role,
      name: name(&tree, node_id),
      parent,
      children: children.into_iter().map(at).collect(),
      index,
      rect: node.rect.unwrap_or_default(),
      focusable,
      focused,
      enabled,
      shown: tree.shown(node_id),
      active: is_root && tree.keyboard.entered,
      editable,
      multi_line,
      clickable,
    })
  }

  /// Clicks button `id`, as Enter on it does, and says whether it was a
  /// shown, enabled button. The click runs with the tree released.
  pub fn click(&self, id: AccessId) -> bool {
    let Some(rc) = self.tree(id.window) else {
      return false;
    };
    let call = {
      let Ok(tree) = rc.try_borrow() else {
        return false;
      };
      let node_id = NodeId::from(id.node);
      let enabled = matches!(
        tree.node(node_id).map(|node| &node.kind),
        Some(NodeKind::Button(button)) if button.enabled
      );
      if !enabled || !tree.shown(node_id) {
        return false;
      }
      tree.click(node_id)
    };
    // Dropped first: the handler may drop the window.
    drop(rc);
    if let Some(call) = call {
      call.run();
    }
    true
  }
}

impl Tree {
  /// Keeps `event` for a screen reader, made from this window's number,
  /// if one listens.
  pub(crate) fn access_event(&self, event: impl FnOnce(u32) -> AccessEvent) {
    if let Some((access, window)) = &self.access
      && access.listening()
    {
      access.push(event(*window));
    }
  }

  pub(crate) fn access_id(&self, window: u32, node: NodeId) -> AccessId {
    AccessId {
      window,
      node: exposed(self, node).into(),
    }
  }

  /// Text `id` changed, which renames it and the button around it.
  pub(crate) fn text_renamed(&self, id: NodeId) {
    if !self
      .access
      .as_ref()
      .is_some_and(|(access, _)| access.listening())
    {
      return;
    }
    self.access_event(|window| AccessEvent::NameChanged(self.access_id(window, id)));
    let mut at = self.taffy.parent(id);
    while let Some(parent) = at {
      if matches!(
        self.node(parent).map(|node| &node.kind),
        Some(NodeKind::Button(_))
      ) {
        // A label inside a slider renames nothing more.
        if exposed(self, parent) == parent {
          self.access_event(|window| AccessEvent::NameChanged(self.access_id(window, parent)));
        }
        return;
      }
      at = self.taffy.parent(parent);
    }
  }
}

/// The node a screen reader sees for `id`: the slider around it, if any,
/// since a slider shows as one object.
fn exposed(tree: &Tree, id: NodeId) -> NodeId {
  let mut found = id;
  let mut at = Some(id);
  while let Some(current) = at {
    if tree.node(current).is_some_and(|node| node.name == "Slider") {
      found = current;
    }
    at = tree.taffy.parent(current);
  }
  found
}

/// The shown children of `id`, in Tab order.
fn exposed_children(tree: &Tree, id: NodeId) -> Vec<NodeId> {
  let children = tree.taffy.children(id).unwrap_or_default();
  children
    .into_iter()
    .filter(|&child| tree.node(child).is_some_and(|node| node.visible))
    .collect()
}

fn role(tree: &Tree, id: NodeId) -> AccessRole {
  let Some(node) = tree.node(id) else {
    return AccessRole::Filler;
  };
  if node.name == "Slider" {
    return AccessRole::Slider;
  }
  match &node.kind {
    NodeKind::Root if tree.popup.is_some() => AccessRole::Window,
    NodeKind::Root => AccessRole::Frame,
    NodeKind::Button(_) => AccessRole::Button,
    NodeKind::Text(_) => AccessRole::Label,
    NodeKind::Input(input) if input.password => AccessRole::PasswordText,
    NodeKind::Input(_) => AccessRole::Entry,
    NodeKind::Icon(_) | NodeKind::Image(_) | NodeKind::Capture(_) | NodeKind::Canvas(_) => {
      AccessRole::Image
    }
    _ => AccessRole::Filler,
  }
}

/// The name a config gave, or else a button's shown text, a text's own,
/// a field's placeholder, or a window's title or namespace.
fn name(tree: &Tree, id: NodeId) -> String {
  let Some(node) = tree.node(id) else {
    return String::new();
  };
  if let Some(name) = &node.accessible_name {
    return name.clone();
  }
  match &node.kind {
    NodeKind::Root => match &tree.floating {
      Some(floating) => floating.title().to_owned(),
      None if tree.popup.is_none() && node.name == "PanelWindow" => tree.surface.namespace.clone(),
      None => String::new(),
    },
    NodeKind::Text(text) => text.content.clone(),
    NodeKind::Input(input) => input.placeholder.clone(),
    NodeKind::Button(_) => {
      let mut texts = Vec::new();
      let mut pending = vec![id];
      while let Some(at) = pending.pop() {
        match tree.node(at) {
          Some(node) if !node.visible => continue,
          Some(node) => {
            if let NodeKind::Text(text) = &node.kind
              && !text.content.trim().is_empty()
            {
              texts.push(text.content.trim().to_owned());
            }
          }
          None => continue,
        }
        let mut children = tree.taffy.children(at).unwrap_or_default();
        children.reverse();
        pending.extend(children);
      }
      texts.join(" ")
    }
    _ => String::new(),
  }
}
