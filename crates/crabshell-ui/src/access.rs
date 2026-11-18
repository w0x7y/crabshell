//! What a screen reader is shown of the windows: each widget's role, name,
//! state and place, read from the window trees when it asks, and the
//! changes heard while one listens. The AT-SPI side is in
//! crabshell-services; nothing here walks a tree unless it is asked.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::task::Waker;

use unicode_segmentation::UnicodeSegmentation;

use crate::geometry::Rect;
use crate::slider::Range;
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
  /// A field's text, caret and selection.
  pub text: Option<AccessText>,
  /// A slider's value.
  pub value: Option<AccessValue>,
}

/// A field's text as a screen reader reads it: a password's is one dot a
/// character, so only its length shows. Offsets count characters.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct AccessText {
  pub text: String,
  pub caret: usize,
  /// The selected characters, start first.
  pub selection: Option<(usize, usize)>,
}

impl AccessText {
  /// The character offsets of the character, word, sentence, or line or
  /// paragraph around character `at`, by AT-SPI's granularity from 0 to
  /// 4; at the end, the last one, or nothing for a character or after a
  /// last line break. A word runs to the start of the next.
  pub fn segment(&self, at: usize, granularity: u32) -> Option<(usize, usize)> {
    let text = self.text.as_str();
    let pieces: Vec<&str> = match granularity {
      0 => text.split_inclusive(|_| true).collect(),
      1 => {
        let mut cuts: Vec<usize> = text
          .split_word_bound_indices()
          .filter(|(at, piece)| *at > 0 && piece.chars().any(char::is_alphanumeric))
          .map(|(at, _)| at)
          .collect();
        cuts.insert(0, 0);
        cuts.push(text.len());
        cuts.windows(2).map(|cut| &text[cut[0]..cut[1]]).collect()
      }
      2 => text.split_sentence_bounds().collect(),
      3 | 4 => text.split_inclusive('\n').collect(),
      _ => return None,
    };
    let mut start = 0;
    let mut last = (0, 0);
    for piece in pieces {
      let end = start + piece.chars().count();
      if at < end {
        return Some((start, end));
      }
      last = (start, end);
      start = end;
    }
    Some(
      if granularity == 0 || (granularity >= 3 && text.ends_with('\n')) {
        (start, start)
      } else {
        last
      },
    )
  }
}

/// A slider's range and value. `increment` is what a key moves it by.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AccessValue {
  pub min: f64,
  pub max: f64,
  pub current: f64,
  pub increment: f64,
}

/// A change a screen reader hears of.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccessEvent {
  /// A widget shows the keyboard focus, or stops.
  Focused(AccessId, bool),
  NameChanged(AccessId),
  /// A window's root, as its window gets or loses the keyboard.
  Active(AccessId, bool),
  /// A widget shown or hidden.
  Shown(AccessId, bool),
  /// A button turned on or off.
  Enabled(AccessId, bool),
  /// `child` added to `parent` at `index`, or removed from there.
  Children {
    parent: AccessId,
    child: AccessId,
    index: usize,
    added: bool,
  },
  /// A window opened, closed, showed or hid, or a lock came or went, so
  /// [`Accessibility::windows`] may list others.
  WindowsChanged,
  /// A field's `text` inserted at character `start`, or deleted from
  /// there; a password's as its dots.
  TextChanged {
    id: AccessId,
    start: usize,
    text: String,
    inserted: bool,
  },
  /// The focused field's caret moved to this character.
  CaretMoved(AccessId, usize),
  /// A slider's value changed.
  ValueChanged(AccessId),
}

impl Accessibility {
  /// Adds a tree, and returns its window's number.
  pub(crate) fn register(&self, tree: Weak<RefCell<Tree>>) -> u32 {
    let id = self.next.get();
    self.next.set(id + 1);
    let mut windows = self.windows.borrow_mut();
    windows.retain(|(_, tree)| tree.strong_count() > 0);
    windows.push((id, tree));
    if self.listening() {
      self.push(AccessEvent::WindowsChanged);
    }
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

  /// Window `window`'s tree, while it is listed.
  fn tree(&self, window: u32) -> Option<Rc<RefCell<Tree>>> {
    if !self.windows().iter().any(|root| root.window == window) {
      return None;
    }
    let windows = self.windows.borrow();
    let (_, tree) = windows.iter().find(|(id, _)| *id == window)?;
    tree.upgrade()
  }

  /// The roots of the windows shown, oldest first. While a lock surface
  /// is up, only the lock surfaces, so nothing behind the lock is read
  /// or clicked.
  pub fn windows(&self) -> Vec<AccessId> {
    let windows = self.windows.borrow();
    let shown: Vec<(AccessId, bool)> = windows
      .iter()
      .filter_map(|(id, tree)| {
        let tree = tree.upgrade()?;
        let tree = tree.try_borrow().ok()?;
        let shown = tree
          .floating
          .as_ref()
          .is_none_or(|floating| floating.shown());
        let lock = tree
          .node(tree.root)
          .is_some_and(|root| root.name == "SessionLock");
        let root = AccessId {
          window: *id,
          node: tree.root.into(),
        };
        shown.then_some((root, lock))
      })
      .collect();
    let locked = shown.iter().any(|(_, lock)| *lock);
    let listed = shown.into_iter().filter(|(_, lock)| *lock || !locked);
    listed.map(|(root, _)| root).collect()
  }

  /// Widget `id` as it is now, or `None` once it is gone or its window
  /// isn't listed.
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
    let slider = (role == AccessRole::Slider)
      .then(|| slider_button(&tree, node_id))
      .flatten();
    let (enabled, clickable) = match (&node.kind, slider) {
      (_, Some((enabled, _))) => (enabled, false),
      (NodeKind::Button(button), None) => (button.enabled, button.enabled),
      _ => (true, false),
    };
    let (editable, multi_line, text) = match &node.kind {
      NodeKind::Input(input) => (true, input.edit.multiline, Some(input.access_text())),
      _ => (false, false, None),
    };
    let focusable = match role {
      AccessRole::Button | AccessRole::Slider => enabled,
      AccessRole::Entry | AccessRole::PasswordText => true,
      _ => false,
    };
    let children = match role {
      AccessRole::Slider => Vec::new(),
      _ => exposed_children(&tree, node_id),
    };
    let name = name(&tree, node_id);
    let rect = node.rect.unwrap_or_default();
    let (children, shown, active) = (
      children.into_iter().map(at).collect(),
      tree.shown(node_id),
      is_root && tree.keyboard.entered,
    );
    // Read with the tree released: a dirty range runs the config's closure.
    drop(tree);
    let value = slider.and_then(|(_, range)| range).map(Range::access);
    Some(AccessNode {
      role,
      name,
      parent,
      children,
      index,
      rect,
      focusable,
      focused,
      enabled,
      shown,
      active,
      editable,
      multi_line,
      clickable,
      text,
      value,
    })
  }

  /// Writes `value` to slider `id`, within its range and on a step, as a
  /// drag does, and says whether it was a shown, enabled slider.
  pub fn set_value(&self, id: AccessId, value: f64) -> bool {
    let Some(rc) = self.tree(id.window).filter(|_| !value.is_nan()) else {
      return false;
    };
    let range = {
      let Ok(tree) = rc.try_borrow() else {
        return false;
      };
      let node_id = NodeId::from(id.node);
      if role(&tree, node_id) != AccessRole::Slider || !tree.shown(node_id) {
        return false;
      }
      match slider_button(&tree, node_id) {
        Some((true, Some(range))) => range,
        _ => return false,
      }
    };
    drop(rc);
    range.write(value as f32);
    true
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

  /// Keeps an event about widget `id`, made from its id, if a screen
  /// reader listens and the widget was laid out, so one it can't have
  /// seen yet makes no noise.
  pub(crate) fn node_event(&self, id: NodeId, event: impl FnOnce(AccessId) -> AccessEvent) {
    if self.node(id).is_some_and(|node| node.rect.is_some()) {
      self.access_event(|window| event(self.access_id(window, id)));
    }
  }

  pub(crate) fn access_id(&self, window: u32, node: NodeId) -> AccessId {
    AccessId {
      window,
      node: exposed(self, node).into(),
    }
  }

  fn listening(&self) -> bool {
    self
      .access
      .as_ref()
      .is_some_and(|(access, _)| access.listening())
  }

  /// Field `id`'s text and caret before a change, while a screen reader
  /// listens and has seen it.
  pub(crate) fn field_before(&self, id: NodeId) -> Option<AccessText> {
    match &self.node(id)?.kind {
      NodeKind::Input(input) if self.listening() && self.node(id)?.rect.is_some() => {
        Some(input.access_text())
      }
      _ => None,
    }
  }

  /// Tells a screen reader what field `id` lost and gained since
  /// `before`, and where its caret went if it has the focus.
  pub(crate) fn field_changed(&self, id: NodeId, before: Option<AccessText>) {
    let (Some(before), Some(NodeKind::Input(input))) = (before, self.node(id).map(|n| &n.kind))
    else {
      return;
    };
    let after = input.access_text();
    let (old, new): (Vec<char>, Vec<char>) =
      (before.text.chars().collect(), after.text.chars().collect());
    let start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let end = old[start..]
      .iter()
      .rev()
      .zip(new[start..].iter().rev())
      .take_while(|(a, b)| a == b)
      .count();
    let changes = [
      (&old[start..old.len() - end], false),
      (&new[start..new.len() - end], true),
    ];
    for (text, inserted) in changes.into_iter().filter(|(text, _)| !text.is_empty()) {
      let text = text.iter().collect();
      self.access_event(|window| AccessEvent::TextChanged {
        id: self.access_id(window, id),
        start,
        text,
        inserted,
      });
    }
    if after.caret != before.caret && self.keyboard.focused == Some(id) {
      self.access_event(|window| AccessEvent::CaretMoved(self.access_id(window, id), after.caret));
    }
  }

  /// Text `id` changed, which renames it and the button around it.
  pub(crate) fn text_renamed(&self, id: NodeId) {
    if !self.listening() {
      return;
    }
    self.node_event(id, AccessEvent::NameChanged);
    self.button_renamed(id);
  }

  /// Something inside a button changed what it shows, which renames the
  /// nearest button around `id`.
  pub(crate) fn button_renamed(&self, id: NodeId) {
    if !self.listening() {
      return;
    }
    let mut at = self.taffy.parent(id);
    while let Some(parent) = at {
      if matches!(
        self.node(parent).map(|node| &node.kind),
        Some(NodeKind::Button(_))
      ) {
        // A label inside a slider renames nothing more.
        if exposed(self, parent) == parent {
          self.node_event(parent, AccessEvent::NameChanged);
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
    let Some(node) = tree.node(current) else {
      break;
    };
    if node.name == "Slider" {
      found = current;
    }
    at = tree.taffy.parent(current);
  }
  found
}

/// Whether slider `id`'s button is on, and the range it moves.
fn slider_button(tree: &Tree, id: NodeId) -> Option<(bool, Option<Range>)> {
  tree
    .taffy
    .children(id)
    .ok()?
    .into_iter()
    .find_map(|child| match &tree.node(child)?.kind {
      NodeKind::Button(button) => Some((button.enabled, button.value)),
      _ => None,
    })
}

/// The children of `id`, in Tab order, hidden ones too: their state
/// says they don't show.
fn exposed_children(tree: &Tree, id: NodeId) -> Vec<NodeId> {
  tree.taffy.children(id).unwrap_or_default()
}

impl Tree {
  /// `parent`'s children before a change, when a screen reader listens
  /// and has seen it: it was laid out and isn't inside a slider.
  pub(crate) fn access_children(&self, parent: NodeId) -> Option<Vec<NodeId>> {
    let seen = self.node(parent).is_some_and(|node| node.rect.is_some());
    (self.listening()
      && seen
      && exposed(self, parent) == parent
      && role(self, parent) != AccessRole::Slider)
      // Its groups still hold children removed from taffy before.
      .then(|| {
        self
          .node(parent)
          .map(|node| node.groups.concat())
          .unwrap_or_default()
      })
  }

  /// Tells a screen reader which children `parent` lost and gained.
  pub(crate) fn children_changed(&self, parent: NodeId, before: &[NodeId], after: &[NodeId]) {
    let removed = before
      .iter()
      .enumerate()
      .filter(|(_, id)| !after.contains(id));
    let added = after
      .iter()
      .enumerate()
      .filter(|(_, id)| !before.contains(id));
    let changes = removed
      .map(|change| (change, false))
      .chain(added.map(|change| (change, true)));
    for ((index, &child), added) in changes {
      // Neither is inside a slider, and a removed child is gone from
      // taffy, so their ids are their own.
      let id = |window, node: NodeId| AccessId {
        window,
        node: node.into(),
      };
      self.access_event(|window| AccessEvent::Children {
        parent: id(window, parent),
        child: id(window, child),
        index,
        added,
      });
    }
  }
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
