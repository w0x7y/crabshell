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

use crabshell_reactive::{Signal, untrack};

use crate::access::AccessEvent;
use crate::cx::Cx;
use crate::edit::Keyed;
use crate::input::{Handler, run_isolated};
use crate::select::copy_key;
use crate::text_input::AcceptHandler;
use crate::tree::{Change, NodeId, NodeKind, Tree, borrow};
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

/// What a key asks of the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clipboard {
  /// Offer this text as the clipboard's.
  Copy(String),
  /// Read the clipboard and [`paste`](Window::paste) it into the focused
  /// field.
  Paste,
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

/// How many lines PageUp and PageDown move in a text area outside a
/// scroll view.
const PAGE_LINES: f32 = 10.0;

pub(crate) type KeyHandler = Rc<Handler<dyn Fn(&Cx, &KeyEvent) -> bool>>;

/// Who has the keyboard in a window.
#[derive(Default)]
pub(crate) struct Keyboard {
  /// The window has the keyboard.
  pub(crate) entered: bool,
  /// The focused button or field, kept while the keyboard is elsewhere.
  pub(crate) focused: Option<NodeId>,
  /// The link span of the focused text that Tab stopped on.
  pub(crate) link: Option<usize>,
  /// The window's own `on_key`.
  pub(crate) handler: Option<KeyHandler>,
  /// The modifiers held as the platform last said, which a press reads.
  pub(crate) modifiers: Modifiers,
}

impl Keyboard {
  /// Forgets nodes that were removed from the tree.
  pub(crate) fn forget(&mut self, removed: &[NodeId]) {
    if self.focused.is_some_and(|id| removed.contains(&id)) {
      self.focused = None;
      self.link = None;
    }
  }
}

impl Window {
  /// The keyboard entered the window: its focused button shows its focus
  /// style again.
  pub fn keyboard_enter(&self) {
    let mut tree = borrow(&self.tree);
    if !std::mem::replace(&mut tree.keyboard.entered, true) {
      let root = tree.root;
      tree.node_event(root, |id| AccessEvent::Active(id, true));
    }
    let focused = tree.keyboard.focused;
    tree.show_focus(focused, true);
  }

  /// The keyboard left the window. The focused button stays focused for
  /// when it comes back.
  pub fn keyboard_leave(&self) {
    let mut tree = borrow(&self.tree);
    let was = std::mem::replace(&mut tree.keyboard.entered, false);
    let focused = tree.keyboard.focused;
    tree.show_focus(focused, false);
    if was {
      let root = tree.root;
      tree.node_event(root, |id| AccessEvent::Active(id, false));
    }
  }

  /// The modifiers held on the seat, which the platform tells before a
  /// pointer button goes down: Shift and a press extend a selection.
  pub fn set_modifiers(&self, modifiers: Modifiers) {
    borrow(&self.tree).keyboard.modifiers = modifiers;
  }

  /// A key was pressed or repeated. A focused text field edits with it
  /// first. Then the focused button's `on_key` gets it, then the
  /// window's; a handler returns `true` when it used the key. Otherwise
  /// Tab and Shift+Tab move the focus between buttons and fields, and
  /// Enter or Space clicks the focused button.
  ///
  /// Returns what the platform should do with the clipboard: copy a
  /// field's selection on Ctrl+C or Ctrl+X, or paste into the focused
  /// field with [`paste`](Self::paste) on Ctrl+V.
  pub fn key(&self, event: &KeyEvent) -> Option<Clipboard> {
    let typed = borrow(&self.tree).input_key(event);
    match typed {
      Some(Typed::Edited(signal, text, clipboard)) => {
        // A signal whose owner is gone just isn't written.
        signal.try_set(text);
        return clipboard;
      }
      Some(Typed::Clipboard(clipboard)) => return Some(clipboard),
      Some(Typed::Moved) => return None,
      Some(Typed::Accept(call)) => {
        call.run();
        return None;
      }
      None => {}
    }
    let (button, window) = {
      let tree = self.tree.borrow();
      (tree.focused_key_call(), tree.window_key_call())
    };
    for call in [button, window].into_iter().flatten() {
      if call.run(event) {
        return None;
      }
    }
    let click = borrow(&self.tree).default_key(event);
    if let Some(click) = click {
      click.run();
    }
    None
  }

  /// The path of the focused button, for tests and logs.
  pub fn focused(&self) -> Option<String> {
    let tree = self.tree.borrow();
    let id = tree.keyboard.focused?;
    tree.node(id).map(|node| node.path.to_string())
  }
}

impl Tree {
  /// Marks `id`, a button or field, as showing focus or not. Unlike a
  /// hover change, a layout this causes checks the hover again: buttons
  /// may have moved under the pointer.
  fn show_focus(&mut self, id: Option<NodeId>, shown: bool) {
    let Some(id) = id else {
      return;
    };
    // A focused link draws lit; text fields show no ring either.
    if let Some(NodeKind::Text(_)) = self.node(id).map(|node| &node.kind) {
      let link = self.keyboard.link.filter(|_| shown);
      self.light_link(id, true, link);
      return;
    }
    let was = match self.node(id).map(|node| &node.kind) {
      Some(NodeKind::Input(input)) => input.focused,
      Some(NodeKind::Button(button)) => button.focused,
      _ => return,
    };
    if was == shown {
      return;
    }
    if matches!(
      self.node(id).map(|node| &node.kind),
      Some(NodeKind::Input(_))
    ) {
      self.update(id, |node| match &mut node.kind {
        NodeKind::Input(input) => {
          input.focused = shown;
          Change::Paint
        }
        _ => Change::None,
      });
    } else {
      self.update_button_style(id, |button| button.focused = shown);
    }
    self.node_event(id, |id| AccessEvent::Focused(id, shown));
  }

  /// Moves the focus to `id`. It shows once the window has the keyboard.
  /// A hidden node, or one inside a hidden widget, is refused.
  pub(crate) fn focus(&mut self, id: Option<NodeId>) {
    self.focus_link(id, None);
  }

  /// Drops the focus from text `id` if the link span it is on is no
  /// longer a link, so Tab starts again from the first stop.
  pub(crate) fn drop_stale_link_focus(&mut self, id: NodeId) {
    let (Some(focused), Some(link)) = (self.keyboard.focused, self.keyboard.link) else {
      return;
    };
    let still = match self.node(id).map(|node| &node.kind) {
      Some(NodeKind::Text(text)) => text.spans.get(link).is_some_and(|span| span.link.is_some()),
      _ => false,
    };
    if focused == id && !still {
      self.focus(None);
    }
  }

  /// Moves the focus to `id`, on its link span `link` if it is a text.
  fn focus_link(&mut self, id: Option<NodeId>, link: Option<usize>) {
    if id.is_some_and(|id| !self.shown(id)) {
      return;
    }
    if self.keyboard.focused == id {
      if self.keyboard.link != link {
        self.keyboard.link = link;
        let entered = self.keyboard.entered;
        self.show_focus(id, entered);
      }
      return;
    }
    let old = std::mem::replace(&mut self.keyboard.focused, id);
    // What an input method was composing there is dropped with it.
    if let Some(old) = old
      && matches!(self.node(old).map(|node| &node.kind), Some(NodeKind::Input(input)) if input.preedit.is_some())
    {
      self.update_input(old, |input| {
        input.preedit = None;
        input.refresh();
      });
    }
    if let Some(old) = old {
      self.drop_selection(old);
    }
    self.show_focus(old, false);
    self.keyboard.link = link;
    let entered = self.keyboard.entered;
    self.show_focus(id, entered);
    // The next layout scrolls a newly focused field's caret into view.
    if let Some(id) = id {
      self.update_input(id, |_| {});
    }
  }

  /// What `event` does to the focused text field, if one is focused and
  /// uses the key.
  fn input_key(&mut self, event: &KeyEvent) -> Option<Typed> {
    let id = self.keyboard.focused?;
    let view = self.page_height(id);
    let input = match &mut self.node_mut(id)?.kind {
      NodeKind::Input(input) => input,
      NodeKind::Text(text) => return copy_key(text, event).map(Typed::Clipboard),
      _ => return None,
    };
    let page = view.unwrap_or(PAGE_LINES * input.text.buffer.metrics().line_height);
    let keyed = match input.edit.multiline {
      true => input.line_key(event, page),
      false => None,
    };
    let was = input.edit.selection();
    let keyed = keyed.unwrap_or_else(|| {
      // Any other move forgets the line's end and the place across.
      input.upstream = None;
      input.goal = None;
      input.edit.key(event, !input.password)
    });
    // A selection made with the keys becomes the primary selection.
    let selected = input
      .edit
      .selection()
      .filter(|now| keyed == Keyed::Moved && Some(now) != was.as_ref() && !input.password);
    let primary = selected.and_then(|range| input.edit.text.get(range).map(str::to_owned));
    let accept = keyed == Keyed::Accept;
    let typed = match keyed {
      Keyed::Unused => return None,
      Keyed::Moved => Typed::Moved,
      Keyed::Edited => Typed::Edited(input.signal, input.edit.text.clone(), None),
      Keyed::Copy(text) => Typed::Clipboard(Clipboard::Copy(text)),
      Keyed::Cut(text) => Typed::Edited(
        input.signal,
        input.edit.text.clone(),
        Some(Clipboard::Copy(text)),
      ),
      Keyed::Paste => Typed::Clipboard(Clipboard::Paste),
      Keyed::Accept => {
        let handler = untrack(|| input.on_accept.try_with(Option::clone)).flatten();
        let text = input.edit.text.clone();
        let node = self.node(id)?;
        // Without its own `on_accept`, Enter goes on to the window.
        Typed::Accept(AcceptCall {
          cx: node.cx.clone(),
          path: Rc::clone(&node.path),
          handler: handler?,
          text,
        })
      }
    };
    if !accept {
      self.update_input(id, |input| {
        input.refresh();
      });
    }
    if let Some(text) = primary {
      self.primary = Some(Clipboard::Copy(text));
    }
    Some(typed)
  }

  /// How far PageUp and PageDown move in node `id`: the height of the
  /// innermost scroll view around it that scrolls up and down, if any.
  fn page_height(&self, id: NodeId) -> Option<f32> {
    self
      .path_to(id)
      .into_iter()
      .rev()
      .find_map(|at| match &self.node(at)?.kind {
        NodeKind::Scroll(scroll) => scroll
          .active()
          .iter()
          .find(|axis| !axis.horizontal)
          .map(|axis| axis.length),
        _ => None,
      })
  }

  /// The enabled buttons, the fields and the links of texts with an
  /// `on_link`, in the order they are laid out: depth first, in child
  /// order, a text's links in text order.
  fn focusable(&self) -> Vec<(NodeId, Option<usize>)> {
    let mut found = Vec::new();
    let mut pending = vec![self.root];
    while let Some(id) = pending.pop() {
      // A hidden widget is skipped with everything in it.
      if self.node(id).is_some_and(|node| !node.visible) {
        continue;
      }
      match self.node(id).map(|node| &node.kind) {
        Some(NodeKind::Button(button)) if button.enabled => found.push((id, None)),
        Some(NodeKind::Input(_)) => found.push((id, None)),
        Some(NodeKind::Text(text)) => {
          found.extend(text.link_stops().into_iter().map(|link| (id, Some(link))));
        }
        _ => {}
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
        let at = self.keyboard.focused.and_then(|id| {
          let link = self.keyboard.link;
          buttons.iter().position(|&stop| stop == (id, link))
        });
        let count = buttons.len();
        let next = match (at, event.modifiers.shift) {
          _ if count == 0 => None,
          (None, false) => Some(0),
          (None, true) => Some(count - 1),
          (Some(at), false) => Some((at + 1) % count),
          (Some(at), true) => Some((at + count - 1) % count),
        };
        let (id, link) = next.map_or((None, None), |next| {
          (Some(buttons[next].0), buttons[next].1)
        });
        self.focus_link(id, link);
        None
      }
      Key::Enter | Key::Character(' ') => {
        let id = self.keyboard.focused?;
        match self.keyboard.link {
          Some(link) => self.link_call(id, link),
          None => self.click(id),
        }
      }
      _ => None,
    }
  }
}

/// What a key did in a text field, once the tree is released.
enum Typed {
  /// The text changed; the signal gets it. A cut also copies.
  Edited(Signal<String>, String, Option<Clipboard>),
  /// The platform should copy or paste.
  Clipboard(Clipboard),
  /// Only the caret or selection moved, already in the tree.
  Moved,
  /// Enter: the field's `on_accept` runs.
  Accept(AcceptCall),
}

/// A field's `on_accept`, to run once the tree is released.
struct AcceptCall {
  cx: Cx,
  path: Rc<str>,
  handler: AcceptHandler,
  text: String,
}

impl AcceptCall {
  fn run(self) {
    run_isolated(&self.path, "on_accept", self.handler, |f| {
      f(&self.cx, &self.text)
    });
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
