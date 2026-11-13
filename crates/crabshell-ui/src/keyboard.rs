//! Keyboard input: the focused button, Tab between buttons, and key
//! handlers.
//!
//! The platform calls [`Window::keyboard_enter`], [`Window::key`] and
//! [`Window::keyboard_leave`] for the window that has the keyboard. A key
//! goes to the focused button's `on_key`, then to the window's, and if
//! neither handled it, Tab moves the focus and Enter or Space clicks the
//! focused button. Handlers run with the tree released, inside
//! `catch_unwind`.

use std::rc::Rc;

use crabshell_reactive::untrack;

use crate::cx::Cx;
use crate::input::{Handler, run_isolated};
use crate::tree::{NodeId, NodeKind, Tree, borrow};
use crate::window::Window;

/// A key press, or a repeat of a key held down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyEvent {
  /// Which key, as the keyboard layout names it.
  pub key: Key,
  /// The text it types, if any, with Shift and the layout applied:
  /// `"A"` for Shift+a. Keys like Enter and Ctrl combinations type none.
  pub text: Option<String>,
  /// The modifiers held.
  pub modifiers: Modifiers,
  /// Whether this is a repeat of a key held down.
  pub repeat: bool,
}

/// A key, as the keyboard layout names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
  /// Enter or Return, on the main keys or the keypad.
  Enter,
  /// Escape.
  Escape,
  /// Tab, with or without Shift.
  Tab,
  /// Backspace.
  Backspace,
  /// Delete.
  Delete,
  /// The left arrow.
  Left,
  /// The right arrow.
  Right,
  /// The up arrow.
  Up,
  /// The down arrow.
  Down,
  /// Home.
  Home,
  /// End.
  End,
  /// Page Up.
  PageUp,
  /// Page Down.
  PageDown,
  /// A function key, from F1.
  Function(u8),
  /// A key that stands for a character, such as `'a'`, `'A'` with Shift,
  /// or `' '` for the space bar.
  Character(char),
  /// Any other key, by its XKB keysym, such as `0xffe1` for the left
  /// Shift.
  Other(u32),
}

/// The modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
  /// Control.
  pub ctrl: bool,
  /// Alt.
  pub alt: bool,
  /// Shift.
  pub shift: bool,
  /// The logo key, also called Super or the Windows key.
  pub logo: bool,
}

pub(crate) type KeyHandler = Rc<Handler<dyn Fn(&Cx, &KeyEvent) -> bool>>;

/// Who has the keyboard in a window.
#[derive(Default)]
pub(crate) struct Keyboard {
  /// The focused button, kept while the keyboard is elsewhere.
  pub(crate) focused: Option<NodeId>,
  /// The window's own `on_key`.
  pub(crate) handler: Option<KeyHandler>,
}

impl Keyboard {
  /// Forgets nodes that were removed from the tree.
  pub(crate) fn forget(&mut self, removed: &[NodeId]) {
    if self.focused.is_some_and(|id| removed.contains(&id)) {
      self.focused = None;
    }
  }
}

impl Window {
  /// The keyboard entered the window: its focused button shows its focus
  /// style again.
  pub fn keyboard_enter(&self) {
    let mut tree = borrow(&self.tree);
    let focused = tree.keyboard.focused;
    tree.show_focus(focused, true);
  }

  /// The keyboard left the window. The focused button stays focused for
  /// when it comes back.
  pub fn keyboard_leave(&self) {
    let mut tree = borrow(&self.tree);
    let focused = tree.keyboard.focused;
    tree.show_focus(focused, false);
  }

  /// A key was pressed or repeated. The focused button's `on_key` gets it
  /// first, then the window's; a handler returns `true` when it used the
  /// key. Otherwise Tab and Shift+Tab move the focus between buttons, and
  /// Enter or Space clicks the focused one.
  pub fn key(&self, event: &KeyEvent) {
    let (button, window) = {
      let tree = self.tree.borrow();
      (tree.focused_key_call(), tree.window_key_call())
    };
    for call in [button, window].into_iter().flatten() {
      if call.run(event) {
        return;
      }
    }
    let click = borrow(&self.tree).default_key(event);
    if let Some(click) = click {
      click.run();
    }
  }

  /// The path of the focused button, for tests and logs.
  pub fn focused(&self) -> Option<String> {
    let tree = self.tree.borrow();
    let id = tree.keyboard.focused?;
    tree.node(id).map(|node| node.path.to_string())
  }
}

impl Tree {
  /// Marks `id`, a button, as showing focus or not. Unlike a hover
  /// change, a layout this causes checks the hover again: buttons may
  /// have moved under the pointer.
  fn show_focus(&mut self, id: Option<NodeId>, shown: bool) {
    if let Some(id) = id {
      self.update_button_style(id, |button| button.focused = shown);
    }
  }

  /// Moves the focus to `id`. Keys, which move it, come only while the
  /// window has the keyboard, so the new focus shows.
  fn focus(&mut self, id: Option<NodeId>) {
    let old = std::mem::replace(&mut self.keyboard.focused, id);
    if old != id {
      self.show_focus(old, false);
      self.show_focus(id, true);
    }
  }

  /// The buttons in the order they are laid out: depth first, in child
  /// order.
  fn focusable(&self) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut pending = vec![self.root];
    while let Some(id) = pending.pop() {
      if let Some(node) = self.node(id)
        && matches!(node.kind, NodeKind::Button(_))
      {
        found.push(id);
      }
      let mut children = self.taffy.children(id).unwrap_or_default();
      children.reverse();
      pending.extend(children);
    }
    found
  }

  fn focused_key_call(&self) -> Option<KeyCall> {
    let id = self.keyboard.focused?;
    let node = self.node(id)?;
    let NodeKind::Button(button) = &node.kind else {
      return None;
    };
    // Reading the handlers doesn't make anything depend on them.
    let handler = untrack(|| button.handlers.try_with(|handlers| handlers.key.clone()))??;
    Some(KeyCall {
      cx: node.cx.clone(),
      path: Rc::clone(&node.path),
      handler,
    })
  }

  fn window_key_call(&self) -> Option<KeyCall> {
    let node = self.node(self.root)?;
    Some(KeyCall {
      cx: node.cx.clone(),
      path: Rc::clone(&node.path),
      handler: self.keyboard.handler.clone()?,
    })
  }

  /// What a key nobody handled does: Tab moves the focus, Enter and Space
  /// click the focused button.
  fn default_key(&mut self, event: &KeyEvent) -> Option<crate::input::Call> {
    match event.key {
      Key::Tab => {
        let buttons = self.focusable();
        let at = self
          .keyboard
          .focused
          .and_then(|id| buttons.iter().position(|button| *button == id));
        let count = buttons.len();
        let next = match (at, event.modifiers.shift) {
          _ if count == 0 => None,
          (None, false) => Some(0),
          (None, true) => Some(count - 1),
          (Some(at), false) => Some((at + 1) % count),
          (Some(at), true) => Some((at + count - 1) % count),
        };
        self.focus(next.map(|next| buttons[next]));
        None
      }
      Key::Enter | Key::Character(' ') => self.click(self.keyboard.focused?),
      _ => None,
    }
  }
}

/// A key handler to run once the tree is released.
struct KeyCall {
  cx: Cx,
  path: Rc<str>,
  handler: KeyHandler,
}

impl KeyCall {
  /// Whether the handler used the key. One that panicked didn't.
  fn run(self, event: &KeyEvent) -> bool {
    run_isolated(&self.path, "on_key", self.handler, |f| f(&self.cx, event)).unwrap_or(false)
  }
}
