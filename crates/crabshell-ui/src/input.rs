//! Pointer input: hit testing, hover and pressed state, and calling button
//! handlers.
//!
//! The platform turns `wl_pointer` events into calls on [`Window`]. Handlers
//! run after the window's tree is released, inside `catch_unwind`, so a
//! panicking handler is logged and disabled and the bar keeps running.

use std::any::Any;
use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use crabshell_reactive::{Signal, untrack};

use crate::cx::Cx;
use crate::geometry::{Point, Rect};
use crate::keyboard::KeyHandler;
use crate::scroll::PIXELS_PER_NOTCH;
use crate::tree::{ButtonNode, Change, NodeId, NodeKind, Tree, borrow};
use crate::window::Window;

/// Logical pixels of touchpad scrolling per scroll step.
const PIXELS_PER_STEP: f32 = 15.0;

/// `axis_value120` units per mouse-wheel notch.
const VALUE120_PER_STEP: i32 = 120;

/// A user's event handler, and whether it was disabled after panicking.
pub(crate) struct Handler<F: ?Sized> {
  disabled: Cell<bool>,
  f: F,
}

impl<F> Handler<F> {
  pub(crate) fn new(f: F) -> Handler<F> {
    Handler {
      disabled: Cell::new(false),
      f,
    }
  }
}

type ClickHandler = Rc<Handler<dyn Fn(&Cx)>>;
type ScrollHandler = Rc<Handler<dyn Fn(&Cx, i32)>>;

/// A button's handlers.
#[derive(Default)]
pub(crate) struct Handlers {
  pub(crate) click: Option<ClickHandler>,
  pub(crate) press: Option<ClickHandler>,
  pub(crate) right_click: Option<ClickHandler>,
  pub(crate) middle_click: Option<ClickHandler>,
  pub(crate) scroll: Option<ScrollHandler>,
  pub(crate) key: Option<KeyHandler>,
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerButton {
  /// The primary button; it runs `on_click`.
  Left,
  /// The secondary button; it runs `on_right_click`.
  Right,
  /// The middle button; it runs `on_middle_click`.
  Middle,
}

/// One scroll event on the vertical axis, as `wl_pointer` reports it.
/// Positive values scroll down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollDelta {
  /// A mouse wheel, in `axis_value120` units: 120 per notch, less for a
  /// high-resolution wheel.
  Wheel(i32),
  /// Smooth scrolling, such as from a touchpad, in logical pixels.
  Pixels(f32),
}

/// The cursor the platform should show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
  /// The default arrow.
  Default,
  /// A pointing hand, over a button.
  Pointer,
  /// An I-beam, over a text field.
  Text,
}

/// Where the pointer is and what it is doing.
#[derive(Default)]
pub(crate) struct Pointer {
  position: Option<Point>,
  /// The button under the pointer.
  hovered: Option<NodeId>,
  /// The buttons with a `hovered` signal that the pointer is inside: the
  /// hovered one and those around it.
  inside: Vec<(NodeId, Signal<bool>)>,
  /// The button a pointer button went down on, until it comes up.
  pressed: Option<(NodeId, PointerButton)>,
  /// Scrolling that hasn't added up to a whole step yet.
  wheel: i32,
  pixels: f32,
  /// A hover or press change asked for the pending layout.
  caused_layout: bool,
}

impl Pointer {
  /// Forgets nodes that were removed from the tree.
  pub(crate) fn forget(&mut self, removed: &[NodeId]) {
    if self.hovered.is_some_and(|id| removed.contains(&id)) {
      self.hovered = None;
    }
    self.inside.retain(|(id, signal)| {
      let gone = removed.contains(id);
      if gone {
        signal.try_set(false);
      }
      !gone
    });
    if self.pressed.is_some_and(|(id, _)| removed.contains(&id)) {
      self.pressed = None;
    }
  }
}

impl Window {
  /// The pointer entered the window or moved, to `position` in logical
  /// pixels. Returns the cursor to show.
  pub fn pointer_motion(&self, position: Point) -> CursorShape {
    let mut tree = borrow(&self.tree);
    tree.pointer.position = Some(position);
    tree.hover_at(position);
    tree.cursor()
  }

  /// The pointer left the window.
  pub fn pointer_leave(&self) {
    let mut tree = borrow(&self.tree);
    tree.pointer.position = None;
    tree.set_pressed(None);
    tree.set_hovered(None, &[]);
  }

  /// A pointer button went down or up. Releasing it over the button it
  /// went down on runs that button's `on_click`, `on_right_click` or
  /// `on_middle_click`.
  pub fn pointer_button(&self, button: PointerButton, pressed: bool) {
    let call = borrow(&self.tree).button(button, pressed);
    if let Some(call) = call {
      call.run();
    }
  }

  /// The pointer scrolled. Once whole steps add up, the `on_scroll` of the
  /// button under the pointer runs with them. Without one, the innermost
  /// scroll view under the pointer scrolls.
  pub fn pointer_scroll(&self, delta: ScrollDelta) {
    let scrolled = borrow(&self.tree).scroll(delta);
    match scrolled {
      Some(Scrolled::Button(call)) => call.run(),
      // A signal whose owner is gone just doesn't scroll.
      Some(Scrolled::View(signal, offset)) => {
        signal.try_set(offset);
      }
      None => {}
    }
  }

  /// A touchpad scroll gesture ended (`wl_pointer.axis_stop`): scrolling
  /// that hasn't added up to a whole step is dropped, so it doesn't carry
  /// into the next gesture. A wheel's partial notch is kept.
  pub fn pointer_scroll_end(&self) {
    borrow(&self.tree).pointer.pixels = 0.0;
  }

  /// The cursor to show.
  pub fn cursor(&self) -> CursorShape {
    self.tree.borrow().cursor()
  }
}

impl Tree {
  /// The deepest button under `point` in `id`'s subtree, with the nodes on
  /// the way down to it, itself included, pushed onto `path`. Later
  /// children are drawn on top, so they are tried first.
  fn hit(&self, id: NodeId, point: Point, path: &mut Vec<NodeId>) -> Option<NodeId> {
    let node = self.node(id)?;
    let rect = node.rect.filter(|rect| rect.contains(point))?;
    let style = node.effective_style();
    // A clip hides its children under its border too.
    let border = style.border.map_or(0.0, |border| border.width.max(0.0));
    let inside = Rect::new(
      rect.x + border,
      rect.y + border,
      rect.width - 2.0 * border,
      rect.height - 2.0 * border,
    );
    let children = if node.clips() && !inside.contains(point) {
      Vec::new()
    } else {
      self.taffy.children(id).unwrap_or_default()
    };
    path.push(id);
    if let Some(hit) = children
      .into_iter()
      .rev()
      .find_map(|child| self.hit(child, point, path))
    {
      return Some(hit);
    }
    if matches!(node.kind, NodeKind::Button(_) | NodeKind::Input(_)) {
      return Some(id);
    }
    path.pop();
    None
  }

  /// Hovers the deepest button under `point`, and marks the buttons around
  /// it as having the pointer inside.
  fn hover_at(&mut self, point: Point) {
    let mut path = Vec::new();
    let hit = self.hit(self.root, point, &mut path);
    self.set_hovered(hit, &path);
  }

  fn cursor(&self) -> CursorShape {
    match self
      .pointer
      .hovered
      .and_then(|id| self.node(id))
      .map(|node| &node.kind)
    {
      Some(NodeKind::Input(_)) => CursorShape::Text,
      Some(_) => CursorShape::Pointer,
      None => CursorShape::Default,
    }
  }

  /// Hover checked again after a layout, since buttons may have moved
  /// under a pointer that didn't. Not after a layout a hover change asked
  /// for: a hover style that moves the button off the pointer would
  /// otherwise unhover it, move it back, hover it again, and so on.
  pub(crate) fn pointer_after_layout(&mut self) {
    if std::mem::take(&mut self.pointer.caused_layout) {
      return;
    }
    if let Some(position) = self.pointer.position {
      self.hover_at(position);
    }
  }

  /// `hovered` is the button under the pointer, and `path` the nodes from
  /// the root down to it.
  fn set_hovered(&mut self, hovered: Option<NodeId>, path: &[NodeId]) {
    self.set_inside(path);
    let old = self.pointer.hovered;
    if old == hovered {
      return;
    }
    self.pointer.hovered = hovered;
    self.pointer.wheel = 0;
    self.pointer.pixels = 0.0;
    if let Some(old) = old {
      self.set_button_state(old, |button| button.hovered = false);
    }
    if let Some(new) = hovered {
      self.set_button_state(new, |button| button.hovered = true);
    }
  }

  /// Writes `false` to the `hovered` signals of the buttons the pointer
  /// left and `true` to those it entered, among the nodes on `path`.
  fn set_inside(&mut self, path: &[NodeId]) {
    let inside: Vec<(NodeId, Signal<bool>)> = path
      .iter()
      .filter_map(|&id| match &self.node(id)?.kind {
        NodeKind::Button(button) => Some((id, button.hover_signal?)),
        _ => None,
      })
      .collect();
    let has = |list: &[(NodeId, Signal<bool>)], id: NodeId| list.iter().any(|(at, _)| *at == id);
    for (id, signal) in &self.pointer.inside {
      if !has(&inside, *id) {
        signal.try_set(false);
      }
    }
    for (id, signal) in &inside {
      if !has(&self.pointer.inside, *id) {
        signal.try_set(true);
      }
    }
    self.pointer.inside = inside;
  }

  fn set_pressed(&mut self, pressed: Option<(NodeId, PointerButton)>) {
    let old = std::mem::replace(&mut self.pointer.pressed, pressed);
    if let Some((old, _)) = old {
      self.set_button_state(old, |button| button.pressed = false);
    }
    if let Some((new, _)) = pressed {
      self.set_button_state(new, |button| button.pressed = true);
    }
  }

  /// Changes a button's hover or pressed state, noting when that needs a
  /// layout.
  fn set_button_state(&mut self, id: NodeId, f: impl FnOnce(&mut ButtonNode)) {
    if self.update_button_style(id, f) == Change::Layout {
      self.pointer.caused_layout = true;
    }
  }

  fn button(&mut self, button: PointerButton, pressed: bool) -> Option<Call> {
    if pressed {
      if self.pointer.pressed.is_none()
        && let Some(target) = self.pointer.hovered
      {
        self.set_pressed(Some((target, button)));
        if button == PointerButton::Left {
          self.press_field(target);
          return self.call(target, Action::Press);
        }
      }
      return None;
    }
    let (target, held) = self.pointer.pressed?;
    if held != button {
      return None;
    }
    self.set_pressed(None);
    if self.pointer.hovered != Some(target) {
      return None;
    }
    let action = match button {
      PointerButton::Left => Action::Click,
      PointerButton::Right => Action::RightClick,
      PointerButton::Middle => Action::MiddleClick,
    };
    self.call(target, action)
  }

  fn scroll(&mut self, delta: ScrollDelta) -> Option<Scrolled> {
    let handled = self.pointer.hovered.filter(|&id| self.scrolls(id));
    let Some(target) = handled else {
      return self.scroll_view(delta);
    };
    // Steps down, rounded toward zero; the rest waits for more scrolling.
    let down = match delta {
      ScrollDelta::Wheel(value120) => {
        self.pointer.wheel = self.pointer.wheel.saturating_add(value120);
        let steps = self.pointer.wheel / VALUE120_PER_STEP;
        self.pointer.wheel -= steps * VALUE120_PER_STEP;
        steps
      }
      ScrollDelta::Pixels(pixels) => {
        if !pixels.is_finite() {
          return None;
        }
        self.pointer.pixels += pixels;
        let steps = (self.pointer.pixels / PIXELS_PER_STEP).trunc();
        self.pointer.pixels -= steps * PIXELS_PER_STEP;
        steps as i32
      }
    };
    if down == 0 {
      return None;
    }
    // Handlers count up as positive.
    self
      .call(target, Action::Scroll(down.saturating_neg()))
      .map(Scrolled::Button)
  }

  /// A press on field `id` focuses it and puts the caret under the
  /// pointer. Anything else is left alone.
  fn press_field(&mut self, id: NodeId) {
    let Some(position) = self.pointer.position else {
      return;
    };
    let Some(NodeKind::Input(input)) = self.node(id).map(|node| &node.kind) else {
      return;
    };
    let at = input.hit(position);
    self.focus(Some(id));
    self.update_input(id, |input| {
      input.edit.cursor = at;
      input.edit.anchor = None;
    });
  }

  /// Whether button `id` has an `on_scroll`.
  fn scrolls(&self, id: NodeId) -> bool {
    let Some(NodeKind::Button(button)) = self.node(id).map(|node| &node.kind) else {
      return false;
    };
    untrack(|| {
      button
        .handlers
        .try_with(|handlers| handlers.scroll.is_some())
    })
    .unwrap_or(false)
  }

  /// Scrolls the innermost scroll view under the pointer by `delta`, as
  /// far as its content goes.
  fn scroll_view(&mut self, delta: ScrollDelta) -> Option<Scrolled> {
    let position = self.pointer.position?;
    let id = self.scroll_view_at(self.root, position)?;
    let pixels = match delta {
      ScrollDelta::Wheel(value120) => value120 as f32 / VALUE120_PER_STEP as f32 * PIXELS_PER_NOTCH,
      ScrollDelta::Pixels(pixels) if pixels.is_finite() => pixels,
      ScrollDelta::Pixels(_) => return None,
    };
    let NodeKind::Scroll(scroll) = &self.node(id)?.kind else {
      return None;
    };
    // From the signal, not the last layout, so scrolls and writes since
    // then add up.
    let now = untrack(|| scroll.signal.try_get())?;
    let now = if now.is_nan() {
      0.0
    } else {
      now.clamp(0.0, scroll.most)
    };
    let offset = (now + pixels).clamp(0.0, scroll.most);
    (offset != now).then_some(Scrolled::View(scroll.signal, offset))
  }

  /// The innermost scroll view under `point` in `id`'s subtree, where its
  /// ancestors' clips let the pointer reach.
  fn scroll_view_at(&self, id: NodeId, point: Point) -> Option<NodeId> {
    let node = self.node(id)?;
    if !node.rect.is_some_and(|rect| rect.contains(point)) {
      return None;
    }
    let children = self.taffy.children(id).unwrap_or_default();
    children
      .into_iter()
      .rev()
      .find_map(|child| self.scroll_view_at(child, point))
      .or_else(|| matches!(node.kind, NodeKind::Scroll(_)).then_some(id))
  }

  /// A click on button `id`, as from the keyboard.
  pub(crate) fn click(&self, id: NodeId) -> Option<Call> {
    self.call(id, Action::Click)
  }

  fn call(&self, id: NodeId, action: Action) -> Option<Call> {
    let node = self.node(id)?;
    let NodeKind::Button(button) = &node.kind else {
      return None;
    };
    Some(Call {
      cx: node.cx.clone(),
      path: Rc::clone(&node.path),
      handlers: button.handlers,
      action,
    })
  }
}

/// What a scroll does, once the tree is released.
enum Scrolled {
  /// Runs a button's `on_scroll`.
  Button(Call),
  /// Writes a scroll view's offset.
  View(Signal<f32>, f32),
}

#[derive(Clone, Copy)]
enum Action {
  Click,
  Press,
  RightClick,
  MiddleClick,
  Scroll(i32),
}

/// A handler to run once the tree is released.
pub(crate) struct Call {
  cx: Cx,
  path: Rc<str>,
  handlers: Signal<Handlers>,
  action: Action,
}

impl Call {
  pub(crate) fn run(self) {
    // Reading the handlers doesn't make anything depend on them.
    let handlers = untrack(|| {
      self.handlers.try_with(|handlers| match self.action {
        Action::Click => handlers.click.clone().map(Either::Click),
        Action::Press => handlers.press.clone().map(Either::Click),
        Action::RightClick => handlers.right_click.clone().map(Either::Click),
        Action::MiddleClick => handlers.middle_click.clone().map(Either::Click),
        Action::Scroll(_) => handlers.scroll.clone().map(Either::Scroll),
      })
    });
    let Some(Some(handler)) = handlers else {
      return;
    };
    match (handler, self.action) {
      (Either::Click(handler), action) => {
        let name = match action {
          Action::RightClick => "on_right_click",
          Action::Press => "on_press",
          Action::MiddleClick => "on_middle_click",
          _ => "on_click",
        };
        run_isolated(&self.path, name, handler, |f| f(&self.cx));
      }
      (Either::Scroll(handler), Action::Scroll(steps)) => {
        run_isolated(&self.path, "on_scroll", handler, |f| f(&self.cx, steps));
      }
      (Either::Scroll(_), _) => {}
    }
  }
}

enum Either {
  Click(ClickHandler),
  Scroll(ScrollHandler),
}

/// Calls `handler` through `call` unless it was disabled, and disables it if
/// it panics. Returns what it returned, or `None` if it didn't run or
/// panicked. The handler is dropped here too, isolated the same way: if
/// the call freed its button, this was the last reference.
pub(crate) fn run_isolated<F: ?Sized, R>(
  path: &str,
  name: &str,
  handler: Rc<Handler<F>>,
  call: impl FnOnce(&F) -> R,
) -> Option<R> {
  if handler.disabled.get() {
    return None;
  }
  let result = match panic::catch_unwind(AssertUnwindSafe(|| call(&handler.f))) {
    Ok(result) => Some(result),
    Err(payload) => {
      handler.disabled.set(true);
      tracing::error!(
        "handler {path}.{name} panicked and is disabled: {}",
        panic_message(payload.as_ref())
      );
      None
    }
  };
  if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(move || drop(handler))) {
    tracing::error!(
      "dropping handler {path}.{name} panicked: {}",
      panic_message(payload.as_ref())
    );
  }
  result
}

pub(crate) fn panic_message(payload: &(dyn Any + Send)) -> &str {
  if let Some(message) = payload.downcast_ref::<&str>() {
    message
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message
  } else {
    "(panic payload is not a string)"
  }
}
