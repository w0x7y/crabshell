//! Drags that cross a window's edge: a button's drag going on as the
//! platform's once the pointer leaves its window, and drags from other
//! windows and programs dropped on buttons here. The platform carries
//! them over `wl_data_device`; the window only finds targets and runs
//! handlers.

use std::any::{Any, TypeId};
use std::cell::Cell;
use std::rc::Rc;

use crate::geometry::Point;
use crate::input::{Action, DragPhase, PointerButton, run_all};
use crate::tree::{NodeId, borrow};
use crate::window::Window;

/// What a drag carries to and from other programs:
/// [`Button::drag_content`](crate::Button::drag_content) gives it, and
/// [`Button::on_drop_content`](crate::Button::on_drop_content) takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragContent {
  /// Text, offered as `text/plain;charset=utf-8` and `UTF8_STRING`.
  Text(String),
  /// URIs, such as `file:///home/me/a.png`, offered as `text/uri-list`.
  Uris(Vec<String>),
}

/// A drag that left its window, for the platform to carry on.
#[derive(Clone, Debug, PartialEq)]
pub struct DragOut {
  /// The type of what it carries, which `on_drop` targets in other
  /// windows are matched by.
  pub carried: TypeId,
  /// What other programs are offered, when it carries a [`DragContent`].
  pub content: Option<DragContent>,
}

/// The button a drag from elsewhere was dropped on, which takes the
/// value once it has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropSpot(NodeId);

/// A drag from another window or program over this one, at a point in
/// the window, carrying a value of a type.
#[derive(Clone, Copy)]
pub(crate) struct Incoming {
  pub(crate) at: Point,
  pub(crate) carried: TypeId,
}

impl Window {
  /// The held left-button drag carrying a value, once the pointer has
  /// gone past the window's edge. From then on it is the platform's: a
  /// pointer leave doesn't end it, and [`end_drag_out`](Self::end_drag_out)
  /// does. Returns it only once.
  pub fn take_drag_out(&self) -> Option<DragOut> {
    let mut tree = borrow(&self.tree);
    let at = tree.pointer.position?;
    let size = tree.size;
    if at.x >= 0.0 && at.y >= 0.0 && at.x < size.width && at.y < size.height {
      return None;
    }
    let press = tree
      .pointer
      .pressed
      .filter(|press| press.dragging && !press.out && press.button == PointerButton::Left)?;
    let value = tree.drag_value(press.id)?;
    if let Some(press) = tree.pointer.pressed.as_mut() {
      press.out = true;
    }
    tree.update_drop();
    Some(DragOut {
      carried: (*value).type_id(),
      content: value.downcast_ref::<DragContent>().cloned(),
    })
  }

  /// Whether a drag taken by [`take_drag_out`](Self::take_drag_out) is
  /// still held: its button wasn't removed, hidden or turned off.
  pub fn dragging_out(&self) -> bool {
    let tree = borrow(&self.tree);
    tree.pointer.pressed.is_some_and(|press| press.out)
  }

  /// The drag that went out moved to `at` over its own window, which
  /// its `on_drag` hears as a `Move`.
  pub fn drag_out_moved(&self, at: Point) {
    let call = {
      let mut tree = borrow(&self.tree);
      let Some(press) = tree.pointer.pressed.as_mut().filter(|press| press.out) else {
        return;
      };
      press.last = at;
      let press = *press;
      tree.drag_call(press, DragPhase::Move, at, None)
    };
    run_all(call.into_iter().collect());
  }

  /// Ends the drag that went out, as a release does: the button's
  /// `on_release` runs, then `drop` with what it carries now, which says
  /// whether a target's `on_drop` ran and returned, then its `End`. A
  /// button that went meanwhile runs nothing and drops nothing.
  pub fn end_drag_out(&self, drop: impl FnOnce(Rc<dyn Any>) -> bool) {
    let (release, value) = {
      let tree = borrow(&self.tree);
      let Some(press) = tree.pointer.pressed.filter(|press| press.out) else {
        return;
      };
      (
        tree.call(press.id, Action::Release),
        tree.drag_value(press.id),
      )
    };
    run_all(release.into_iter().collect());
    let landed = Rc::new(Cell::new(false));
    if let Some(value) = value {
      landed.set(drop(value));
    }
    let end = {
      let mut tree = borrow(&self.tree);
      match tree.pointer.pressed.is_some_and(|press| press.out) {
        true => tree.end_press(Some(landed)),
        false => None,
      }
    };
    run_all(end.into_iter().collect());
  }

  /// A drag from elsewhere entered or moved to `at`, carrying a value of
  /// type `carried`. Returns whether a button here would take it, which
  /// that button's `drop_hovered` shows.
  pub fn drag_over(&self, at: Point, carried: TypeId) -> bool {
    let mut tree = borrow(&self.tree);
    tree.pointer.incoming = Some(Incoming { at, carried });
    tree.update_drop();
    tree.pointer.drop_over.is_some()
  }

  /// Whether a button here takes the drag from elsewhere now, which a
  /// layout or a hidden button may have changed under a still drag.
  pub fn drag_takes(&self) -> bool {
    let tree = borrow(&self.tree);
    tree.pointer.incoming.is_some() && tree.pointer.drop_over.is_some()
  }

  /// The drag from elsewhere left the window.
  pub fn drag_leave(&self) {
    let mut tree = borrow(&self.tree);
    if tree.pointer.incoming.take().is_some() {
      tree.update_drop();
    }
  }

  /// The drag from elsewhere was dropped where it last was. Forgets it,
  /// and returns the button that takes it, if any.
  pub fn drag_drop(&self) -> Option<DropSpot> {
    let mut tree = borrow(&self.tree);
    tree.pointer.incoming?;
    let target = tree.drop_target();
    tree.pointer.incoming = None;
    tree.update_drop();
    target.map(DropSpot)
  }

  /// Gives `value` to the `on_drop` of the button at `spot`, if it is
  /// still shown, turned on and takes the value's type. Returns whether
  /// the handler ran and returned.
  pub fn drop_into(&self, spot: DropSpot, value: Rc<dyn Any>) -> bool {
    let landed = Rc::new(Cell::new(false));
    let call = {
      let tree = borrow(&self.tree);
      let carried = (*value).type_id();
      // A hidden button's placed rectangle is forgotten.
      let shown = tree.node(spot.0).is_some_and(|node| node.rect.is_some());
      let takes = shown
        && tree.enabled(spot.0)
        && tree.has(spot.0, |h| {
          h.drop
            .as_ref()
            .is_some_and(|target| target.accepts == carried && !target.handler.disabled())
        });
      takes
        .then(|| tree.call(spot.0, Action::Drop(value, Rc::clone(&landed))))
        .flatten()
    };
    run_all(call.into_iter().collect());
    landed.get()
  }
}
