//! Pointer input: hit testing, hover and pressed state, double clicks,
//! scrolling and calling button handlers.
//!
//! The platform turns `wl_pointer` events into calls on [`Window`]. Handlers
//! run after the window's tree is released, inside `catch_unwind`, so a
//! panicking handler is logged and disabled and the bar keeps running.

use std::any::Any;
use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use crabshell_reactive::{Signal, untrack};

use crate::cx::Cx;
use crate::geometry::{Point, Rect};
use crate::keyboard::KeyHandler;
use crate::scroll::PIXELS_PER_NOTCH;
use crate::tree::{ButtonNode, Change, Node, NodeId, NodeKind, Tree, borrow};
use crate::window::Window;

/// Logical pixels of touchpad scrolling per scroll step.
const PIXELS_PER_STEP: f32 = 15.0;

/// `axis_value120` units per mouse-wheel notch.
const VALUE120_PER_STEP: i32 = 120;

/// The most time between two clicks of a double click, in milliseconds:
/// Qt's `mouseDoubleClickInterval`.
const DOUBLE_CLICK_MS: u32 = 400;

/// The farthest apart two clicks of a double click are, in logical
/// pixels: Qt's `mouseDoubleClickDistance`.
const DOUBLE_CLICK_DISTANCE: f32 = 5.0;

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

pub(crate) type ClickHandler = Rc<Handler<dyn Fn(&Cx)>>;
type ScrollHandler = Rc<Handler<dyn Fn(&Cx, i32)>>;
pub(crate) type WheelHandler = Rc<Handler<dyn Fn(&Cx, Wheel)>>;

/// A button's handlers.
#[derive(Default)]
pub(crate) struct Handlers {
  pub(crate) click: Option<ClickHandler>,
  pub(crate) press: Option<ClickHandler>,
  pub(crate) release: Option<ClickHandler>,
  pub(crate) double_click: Option<ClickHandler>,
  pub(crate) right_click: Option<ClickHandler>,
  pub(crate) middle_click: Option<ClickHandler>,
  pub(crate) back: Option<ClickHandler>,
  pub(crate) forward: Option<ClickHandler>,
  pub(crate) scroll: Option<ScrollHandler>,
  pub(crate) wheel: Option<WheelHandler>,
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
  /// The side button for going back (`BTN_SIDE` or `BTN_BACK`); it runs
  /// `on_back`.
  Back,
  /// The side button for going forward (`BTN_EXTRA` or `BTN_FORWARD`); it
  /// runs `on_forward`.
  Forward,
}

/// Where scrolling came from, as `wl_pointer.axis_source` says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScrollSource {
  /// A mouse wheel, which turns in notches. The default.
  #[default]
  Wheel,
  /// Fingers on a touchpad, which scroll by pixels. When they lift, the
  /// last event has `stop` set: the moment a kinetic scroll takes over.
  Finger,
  /// Something else that scrolls by pixels, such as a trackpoint.
  Continuous,
  /// A wheel tilted sideways, in notches.
  WheelTilt,
}

/// One axis of a [`Wheel`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelAxis {
  /// How far, in logical pixels: positive down or right, as content
  /// moves under a scroll offset that grows.
  pub pixels: f32,
  /// How far in wheel notches, positive down or right:
  /// `axis_value120` over 120, so a high-resolution wheel gives
  /// fractions. 0 for a touchpad.
  pub notches: f32,
  /// The scroll on this axis ended: the fingers lifted
  /// (`wl_pointer.axis_stop`).
  pub stop: bool,
}

/// One scroll event on both axes, as [`Button::on_wheel`](crate::Button::on_wheel)
/// gets it and the platform hands it to [`Window::pointer_scroll`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wheel {
  /// What scrolled.
  pub source: ScrollSource,
  /// Sideways.
  pub x: WheelAxis,
  /// Up and down.
  pub y: WheelAxis,
}

/// The cursor the platform should show: every shape of
/// `wp_cursor_shape_device_v1`, named as in CSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CursorShape {
  /// The default arrow.
  #[default]
  Default,
  /// A context menu is available.
  ContextMenu,
  /// Help is available.
  Help,
  /// A pointing hand, over a button.
  Pointer,
  /// Busy, but still taking input.
  Progress,
  /// Busy; wait.
  Wait,
  /// A cell or cells can be picked.
  Cell,
  /// A crosshair.
  Crosshair,
  /// An I-beam, over a text field.
  Text,
  /// An I-beam for vertical text.
  VerticalText,
  /// Dropping makes an alias.
  Alias,
  /// Dropping copies.
  Copy,
  /// Something moves.
  Move,
  /// Can't be dropped here.
  NoDrop,
  /// Not allowed.
  NotAllowed,
  /// Can be grabbed.
  Grab,
  /// Being grabbed.
  Grabbing,
  /// Resizing the east edge.
  EResize,
  /// Resizing the north edge.
  NResize,
  /// Resizing the north-east corner.
  NeResize,
  /// Resizing the north-west corner.
  NwResize,
  /// Resizing the south edge.
  SResize,
  /// Resizing the south-east corner.
  SeResize,
  /// Resizing the south-west corner.
  SwResize,
  /// Resizing the west edge.
  WResize,
  /// Resizing east and west.
  EwResize,
  /// Resizing north and south.
  NsResize,
  /// Resizing north-east and south-west.
  NeswResize,
  /// Resizing north-west and south-east.
  NwseResize,
  /// A column can be resized sideways.
  ColResize,
  /// A row can be resized up and down.
  RowResize,
  /// Scrolls in any direction.
  AllScroll,
  /// Zooms in.
  ZoomIn,
  /// Zooms out.
  ZoomOut,
  /// Dropping asks which action to take. Needs version 2 of the
  /// protocol; on version 1 a context menu cursor shows.
  DndAsk,
  /// Moves or resizes in any direction. Needs version 2 of the protocol;
  /// on version 1 the move cursor shows.
  AllResize,
}

/// A pointer button held on a button, until it comes up.
#[derive(Clone, Copy)]
struct Press {
  id: NodeId,
  button: PointerButton,
  /// Where it went down, in window coordinates.
  at: Point,
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
  pressed: Option<Press>,
  /// The last left click, with the time it came up and where it went
  /// down, which a second one may make a double click.
  last_click: Option<(NodeId, u32, Point)>,
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
    let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.inside)
      .into_iter()
      .partition(|(id, _)| removed.contains(id));
    for (_, signal) in gone {
      // Unless a button still here shares it.
      if !kept.iter().any(|(_, other)| *other == signal) {
        signal.try_set(false);
      }
    }
    self.inside = kept;
    if self
      .pressed
      .is_some_and(|press| removed.contains(&press.id))
    {
      self.pressed = None;
    }
    if self
      .last_click
      .is_some_and(|(id, _, _)| removed.contains(&id))
    {
      self.last_click = None;
    }
  }
}

impl Tree {
  /// Lets go of hidden nodes as of removed ones, and shows the hovered or
  /// pressed button among them as neither, so it shows again unhovered.
  pub(crate) fn pointer_hide(&mut self, hidden: &[NodeId]) {
    let held = [
      self.pointer.hovered,
      self.pointer.pressed.map(|press| press.id),
    ];
    self.pointer.forget(hidden);
    for id in held.into_iter().flatten().filter(|id| hidden.contains(id)) {
      self.update_button_style(id, |button| {
        button.hovered = false;
        button.pressed = false;
      });
    }
  }
}

/// Milliseconds on a clock of this process, for events that come without
/// the compositor's time.
fn now_ms() -> u32 {
  static START: OnceLock<Instant> = OnceLock::new();
  // Wraps after 49 days, as `wl_pointer`'s times do.
  START.get_or_init(Instant::now).elapsed().as_millis() as u32
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

  /// The pointer left the window. A held button never clicks.
  pub fn pointer_leave(&self) {
    borrow(&self.tree).leave();
  }

  /// A pointer button went down or up, now. See
  /// [`pointer_button_at`](Self::pointer_button_at).
  pub fn pointer_button(&self, button: PointerButton, pressed: bool) {
    self.pointer_button_at(button, pressed, now_ms());
  }

  /// A pointer button went down or up at `time`, in milliseconds as
  /// `wl_pointer.button` counts them, which double clicks are timed by.
  /// Releasing the left button runs `on_release` of the button it went
  /// down on, then, over that button still, its `on_click` or
  /// `on_double_click`. The other buttons run `on_right_click`,
  /// `on_middle_click`, `on_back` or `on_forward` when released over the
  /// button they went down on.
  pub fn pointer_button_at(&self, button: PointerButton, pressed: bool, time: u32) {
    let calls = borrow(&self.tree).button(button, pressed, time);
    run_all(calls);
  }

  /// The pointer scrolled. A button under the pointer with `on_wheel`
  /// gets every event, and one with `on_scroll` the whole vertical steps
  /// as they add up. Without either, the innermost scroll view under the
  /// pointer scrolls vertically. A vertical `stop` drops a touchpad's
  /// partial step, so it doesn't carry into the next gesture; a wheel's
  /// partial notch is kept.
  pub fn pointer_scroll(&self, wheel: Wheel) {
    let scrolled = borrow(&self.tree).scroll(wheel);
    match scrolled {
      Some(Scrolled::Buttons(calls)) => run_all(calls),
      // A signal whose owner is gone just doesn't scroll.
      Some(Scrolled::View(signal, offset)) => {
        signal.try_set(offset);
      }
      None => {}
    }
  }

  /// The cursor to show.
  pub fn cursor(&self) -> CursorShape {
    self.tree.borrow().cursor()
  }
}

/// Runs the handlers an event called for, in order, each isolated.
fn run_all(calls: Vec<Call>) {
  for call in calls {
    call.run();
  }
}

fn distance(a: Point, b: Point) -> f32 {
  (a.x - b.x).hypot(a.y - b.y)
}

impl Tree {
  /// The deepest button or field under `point`, in the coordinates of
  /// `id`'s parent, in `id`'s subtree, with the nodes on the way down to
  /// it, itself included, pushed onto `path`. Children drawn later are on
  /// top, so they are tried first.
  fn hit(&self, id: NodeId, point: Point, path: &mut Vec<NodeId>) -> Option<NodeId> {
    let node = self.node(id)?;
    let point = local(node, point)?;
    let rect = node.rect?;
    // Children reach past the box when they are moved or overflow it, so
    // the subtree's whole area is searched, and the box only for itself.
    if !node.inner_bounds.contains(point) {
      return None;
    }
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
      self.drawn_children(id)
    };
    path.push(id);
    if let Some(hit) = children
      .into_iter()
      .rev()
      .find_map(|child| self.hit(child, point, path))
    {
      return Some(hit);
    }
    if rect.contains(point) && matches!(node.kind, NodeKind::Button(_) | NodeKind::Input(_)) {
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

  /// The nodes from the root down to `id`, both included.
  fn path_to(&self, id: NodeId) -> Vec<NodeId> {
    let mut path = vec![id];
    while let Some(parent) = path.last().and_then(|&id| self.taffy.parent(id)) {
      path.push(parent);
    }
    path.reverse();
    path
  }

  fn cursor(&self) -> CursorShape {
    match self
      .pointer
      .hovered
      .and_then(|id| self.node(id))
      .map(|node| &node.kind)
    {
      Some(NodeKind::Input(_)) => CursorShape::Text,
      Some(NodeKind::Button(button)) => button.cursor,
      _ => CursorShape::Default,
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

  /// The buttons on `path` and what `pick` takes from each.
  fn path_buttons<T>(
    &self,
    path: &[NodeId],
    pick: impl Fn(&ButtonNode) -> Option<T>,
  ) -> Vec<(NodeId, T)> {
    path
      .iter()
      .filter_map(|&id| match &self.node(id)?.kind {
        NodeKind::Button(button) => Some((id, pick(button)?)),
        _ => None,
      })
      .collect()
  }

  /// Writes `false` to the `hovered` signals of the buttons the pointer
  /// left and `true` to those it entered, among the nodes on `path`.
  fn set_inside(&mut self, path: &[NodeId]) {
    let inside = self.path_buttons(path, |button| button.hover_signal);
    // Each signal is written once, and only on a change: `true` while any
    // button sharing it has the pointer inside.
    let holds = |list: &[(NodeId, Signal<bool>)], signal| list.iter().any(|(_, s)| *s == signal);
    let mut seen = Vec::new();
    for &(_, signal) in self.pointer.inside.iter().chain(&inside) {
      if seen.contains(&signal) {
        continue;
      }
      seen.push(signal);
      let now = holds(&inside, signal);
      if holds(&self.pointer.inside, signal) != now {
        signal.try_set(now);
      }
    }
    self.pointer.inside = inside;
  }

  fn set_pressed(&mut self, pressed: Option<Press>) {
    let old = std::mem::replace(&mut self.pointer.pressed, pressed);
    if let Some(old) = old {
      self.set_button_state(old.id, |button| button.pressed = false);
    }
    if let Some(new) = pressed {
      self.set_button_state(new.id, |button| button.pressed = true);
    }
  }

  /// Changes a button's hover or pressed state, noting when that needs a
  /// layout.
  fn set_button_state(&mut self, id: NodeId, f: impl FnOnce(&mut ButtonNode)) {
    if self.update_button_style(id, f) == Change::Layout {
      self.pointer.caused_layout = true;
    }
  }

  fn button(&mut self, button: PointerButton, pressed: bool, time: u32) -> Vec<Call> {
    if pressed {
      self.press(button)
    } else {
      self.release(button, time)
    }
  }

  fn press(&mut self, button: PointerButton) -> Vec<Call> {
    let (None, Some(id), Some(at)) = (
      self.pointer.pressed,
      self.pointer.hovered,
      self.pointer.position,
    ) else {
      return Vec::new();
    };
    self.set_pressed(Some(Press { id, button, at }));
    if button != PointerButton::Left {
      return Vec::new();
    }
    self.press_field(id);
    self.call(id, Action::Press).into_iter().collect()
  }

  fn release(&mut self, button: PointerButton, time: u32) -> Vec<Call> {
    let Some(press) = self.pointer.pressed.filter(|press| press.button == button) else {
      return Vec::new();
    };
    let mut calls = Vec::new();
    if button == PointerButton::Left {
      calls.extend(self.call(press.id, Action::Release));
    }
    self.set_pressed(None);
    if self.pointer.hovered != Some(press.id) {
      return calls;
    }
    let action = match button {
      PointerButton::Left => self.left_click(press.id, time, press.at),
      PointerButton::Right => Action::RightClick,
      PointerButton::Middle => Action::MiddleClick,
      PointerButton::Back => Action::Back,
      PointerButton::Forward => Action::Forward,
    };
    calls.extend(self.call(press.id, action));
    calls
  }

  /// A left click on `id`, coming up at `time` after going down at `at`:
  /// a double click if it has `on_double_click` and the click before was
  /// close enough in time and place.
  fn left_click(&mut self, id: NodeId, time: u32, at: Point) -> Action {
    let close = self.pointer.last_click.is_some_and(|(last, then, place)| {
      last == id
        && time.wrapping_sub(then) <= DOUBLE_CLICK_MS
        && distance(place, at) <= DOUBLE_CLICK_DISTANCE
    });
    if close && self.has(id, |h| h.double_click.is_some()) {
      self.pointer.last_click = None;
      Action::DoubleClick
    } else {
      self.pointer.last_click = Some((id, time, at));
      Action::Click
    }
  }

  /// The pointer left: a held button lets go without a click, and
  /// nothing is hovered.
  pub(crate) fn leave(&mut self) {
    self.pointer.position = None;
    self.set_pressed(None);
    self.set_hovered(None, &[]);
  }

  fn scroll(&mut self, wheel: Wheel) -> Option<Scrolled> {
    let wants = |h: &Handlers| h.scroll.is_some() || h.wheel.is_some();
    let target = self.pointer.hovered.filter(|&id| self.has(id, wants));
    let scrolled = match target {
      None => self.scroll_view(wheel),
      Some(target) => {
        let mut calls = Vec::new();
        if self.has(target, |h| h.wheel.is_some()) {
          calls.extend(self.call(target, Action::Wheel(wheel)));
        }
        if self.has(target, |h| h.scroll.is_some()) {
          let down = self.steps(wheel.y);
          // Handlers count up as positive.
          if down != 0 {
            calls.extend(self.call(target, Action::Scroll(down.saturating_neg())));
          }
        }
        Some(Scrolled::Buttons(calls))
      }
    };
    if wheel.y.stop {
      self.pointer.pixels = 0.0;
    }
    scrolled
  }

  /// The whole steps down that `axis` adds up to, rounded toward zero;
  /// the rest waits for more scrolling.
  fn steps(&mut self, axis: WheelAxis) -> i32 {
    if axis.notches != 0.0 && axis.notches.is_finite() {
      // `as` saturates a value too large.
      let value120 = (axis.notches * VALUE120_PER_STEP as f32).round() as i32;
      self.pointer.wheel = self.pointer.wheel.saturating_add(value120);
      let steps = self.pointer.wheel / VALUE120_PER_STEP;
      self.pointer.wheel -= steps * VALUE120_PER_STEP;
      steps
    } else if axis.pixels.is_finite() {
      self.pointer.pixels += axis.pixels;
      let steps = (self.pointer.pixels / PIXELS_PER_STEP).trunc();
      self.pointer.pixels -= steps * PIXELS_PER_STEP;
      steps as i32
    } else {
      0
    }
  }

  /// A press on field `id` focuses it and puts the caret under the
  /// pointer. Anything else is left alone.
  fn press_field(&mut self, id: NodeId) {
    let Some(position) = self
      .pointer
      .position
      .and_then(|at| self.local_point(id, at))
    else {
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

  /// Whether button `id` has the handlers `f` looks for.
  fn has(&self, id: NodeId, f: impl FnOnce(&Handlers) -> bool) -> bool {
    let Some(NodeKind::Button(button)) = self.node(id).map(|node| &node.kind) else {
      return false;
    };
    untrack(|| button.handlers.try_with(f)).unwrap_or(false)
  }

  /// Scrolls the innermost scroll view under the pointer vertically by
  /// `wheel`, as far as its content goes.
  fn scroll_view(&mut self, wheel: Wheel) -> Option<Scrolled> {
    let position = self.pointer.position?;
    let id = self.scroll_view_at(self.root, position)?;
    let pixels = match wheel.y {
      WheelAxis { notches, .. } if notches != 0.0 && notches.is_finite() => {
        notches * PIXELS_PER_NOTCH
      }
      WheelAxis { pixels, .. } if pixels != 0.0 && pixels.is_finite() => pixels,
      _ => return None,
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
    let point = local(node, point)?;
    // As in `hit`: the subtree's area for the children, the box for itself.
    if !node.inner_bounds.contains(point) {
      return None;
    }
    self
      .drawn_children(id)
      .into_iter()
      .rev()
      .find_map(|child| self.scroll_view_at(child, point))
      .or_else(|| {
        let inside = node.rect.is_some_and(|rect| rect.contains(point));
        (inside && matches!(node.kind, NodeKind::Scroll(_))).then_some(id)
      })
  }

  /// `point`, in window coordinates, in the coordinates node `id`'s
  /// rectangle is in, through the transforms of the node and its
  /// ancestors. `None` if one of them squashes it flat.
  fn local_point(&self, id: NodeId, point: Point) -> Option<Point> {
    self
      .path_to(id)
      .iter()
      .try_fold(point, |point, &id| local(self.node(id)?, point))
  }

  /// The smallest rectangle covering where `rect`, in the coordinates
  /// node `id`'s rectangle is in, is drawn in the window: through the
  /// transforms of the node and its ancestors.
  pub(crate) fn window_rect(&self, id: NodeId, rect: Rect) -> Rect {
    let mut rect = rect;
    let mut at = Some(id);
    while let Some(current) = at {
      if let Some(transform) = self.node(current).and_then(|node| node.transform) {
        rect = transform.map_rect(rect);
      }
      at = self.taffy.parent(current);
    }
    rect
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

/// `point`, in the coordinates of `node`'s parent, in the node's own:
/// moved back through its transform. `None` for a transform that squashes
/// it flat, as a scale of 0 does, which nothing can point at.
fn local(node: &Node, point: Point) -> Option<Point> {
  match node.transform {
    Some(transform) => Some(transform.invert()?.map(point)),
    None => Some(point),
  }
}

/// What a scroll does, once the tree is released.
enum Scrolled {
  /// Runs a button's `on_wheel` and `on_scroll`.
  Buttons(Vec<Call>),
  /// Writes a scroll view's offset.
  View(Signal<f32>, f32),
}

#[derive(Clone, Copy)]
enum Action {
  Click,
  Press,
  Release,
  DoubleClick,
  RightClick,
  MiddleClick,
  Back,
  Forward,
  Scroll(i32),
  Wheel(Wheel),
}

/// A handler to run once the tree is released.
pub(crate) struct Call {
  cx: Cx,
  path: Rc<str>,
  handlers: Signal<Handlers>,
  action: Action,
}

/// A handler picked for a [`Call`], with what it gets.
enum Picked {
  Plain(ClickHandler, &'static str),
  Scroll(ScrollHandler, i32),
  Wheel(WheelHandler, Wheel),
}

impl Call {
  pub(crate) fn run(self) {
    // Reading the handlers doesn't make anything depend on them.
    let picked = untrack(|| {
      self.handlers.try_with(|h| {
        let plain = |handler: &Option<ClickHandler>, name| {
          handler.clone().map(|handler| Picked::Plain(handler, name))
        };
        match self.action {
          Action::Click => plain(&h.click, "on_click"),
          Action::Press => plain(&h.press, "on_press"),
          Action::Release => plain(&h.release, "on_release"),
          Action::DoubleClick => plain(&h.double_click, "on_double_click"),
          Action::RightClick => plain(&h.right_click, "on_right_click"),
          Action::MiddleClick => plain(&h.middle_click, "on_middle_click"),
          Action::Back => plain(&h.back, "on_back"),
          Action::Forward => plain(&h.forward, "on_forward"),
          Action::Scroll(steps) => h.scroll.clone().map(|f| Picked::Scroll(f, steps)),
          Action::Wheel(wheel) => h.wheel.clone().map(|f| Picked::Wheel(f, wheel)),
        }
      })
    });
    let Some(Some(picked)) = picked else {
      return;
    };
    let (path, cx) = (&self.path, &self.cx);
    match picked {
      Picked::Plain(handler, name) => {
        run_isolated(path, name, handler, |f| f(cx));
      }
      Picked::Scroll(handler, steps) => {
        run_isolated(path, "on_scroll", handler, |f| f(cx, steps));
      }
      Picked::Wheel(handler, wheel) => {
        run_isolated(path, "on_wheel", handler, |f| f(cx, wheel));
      }
    }
  }
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
