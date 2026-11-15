//! Scroll views under the pointer: which one a scroll reaches, the wheel
//! and touchpad along its axis, and its scroll bar's handle and track.

use crabshell_reactive::untrack;

use crate::geometry::{Point, Rect};
use crate::input::{Scrolled, Wheel, WheelAxis, local};
use crate::scroll::PIXELS_PER_NOTCH;
use crate::tree::{NodeId, NodeKind, ScrollNode, Tree};

/// A scroll bar's handle held by the left button.
#[derive(Clone, Copy)]
pub(crate) struct BarDrag {
  pub(crate) view: NodeId,
  /// Where the press was along the bar, in the view's own coordinates.
  from: f32,
  /// The offset then.
  offset: f32,
}

impl ScrollNode {
  /// The offset to scroll from: the signal's, not the last layout's, so
  /// scrolls and writes since then add up, within the content as of the
  /// last layout. `None` once the signal's owner is gone.
  pub(crate) fn current(&self) -> Option<f32> {
    let now = untrack(|| self.signal.try_get())?;
    Some(if now.is_nan() {
      0.0
    } else {
      now.clamp(0.0, self.most)
    })
  }

  /// Point `at`'s place along the view's axis.
  fn along(&self, at: Point) -> f32 {
    if self.horizontal { at.x } else { at.y }
  }

  /// `rect`'s start and length along the view's axis.
  fn span(&self, rect: Rect) -> (f32, f32) {
    if self.horizontal {
      (rect.x, rect.width)
    } else {
      (rect.y, rect.height)
    }
  }

  /// How far `wheel` scrolls this view, in logical pixels: a wheel's
  /// notches 48 pixels each, else a touchpad's pixels. A horizontal view
  /// takes sideways scrolling, and a turned wheel when there is none.
  pub(crate) fn wheel_pixels(&self, wheel: Wheel) -> Option<f32> {
    let notched = |axis: WheelAxis| {
      (axis.notches != 0.0 && axis.notches.is_finite()).then_some(axis.notches * PIXELS_PER_NOTCH)
    };
    let smooth =
      |axis: WheelAxis| (axis.pixels != 0.0 && axis.pixels.is_finite()).then_some(axis.pixels);
    if self.horizontal {
      notched(wheel.x)
        .or_else(|| smooth(wheel.x))
        .or_else(|| notched(wheel.y))
    } else {
      notched(wheel.y).or_else(|| smooth(wheel.y))
    }
  }
}

impl Tree {
  /// The scroll views under `point`, innermost first, where their
  /// ancestors' clips let the pointer reach.
  pub(crate) fn scroll_views_at(&self, point: Point) -> Vec<NodeId> {
    self.scroll_chain(self.root, point)
  }

  fn scroll_chain(&self, id: NodeId, point: Point) -> Vec<NodeId> {
    let Some(node) = self.node(id) else {
      return Vec::new();
    };
    let Some(point) = local(node, point) else {
      return Vec::new();
    };
    // As in hit testing: the subtree's area for the children, the box for
    // itself.
    if !node.inner_bounds.contains(point) {
      return Vec::new();
    }
    let mut chain = self
      .drawn_children(id)
      .into_iter()
      .rev()
      .map(|child| self.scroll_chain(child, point))
      .find(|chain| !chain.is_empty())
      .unwrap_or_default();
    if node.rect.is_some_and(|rect| rect.contains(point))
      && matches!(node.kind, NodeKind::Scroll(_))
    {
      chain.push(id);
    }
    chain
  }

  /// Scrolls the innermost scroll view under the pointer that `wheel`
  /// can still move, as far as its content goes.
  pub(crate) fn scroll_view(&mut self, wheel: Wheel) -> Option<Scrolled> {
    let position = self.pointer.position?;
    self.scroll_views_at(position).into_iter().find_map(|id| {
      let NodeKind::Scroll(scroll) = &self.node(id)?.kind else {
        return None;
      };
      let pixels = scroll.wheel_pixels(wheel)?;
      let now = scroll.current()?;
      let offset = (now + pixels).clamp(0.0, scroll.most);
      (offset != now).then_some(Scrolled::View(scroll.signal, offset))
    })
  }

  /// A left press at `at` on a scroll bar: on the handle it starts
  /// dragging it, and beside it pages toward the press. Returns whether
  /// a bar took it.
  pub(crate) fn press_bar(&mut self, at: Point) -> bool {
    for id in self.scroll_views_at(at) {
      let Some(point) = self.local_point(id, at) else {
        continue;
      };
      let Some(NodeKind::Scroll(scroll)) = self.node(id).map(|node| &node.kind) else {
        continue;
      };
      let (Some((track, handle)), Some(rect)) = (scroll.handle, self.node(id).and_then(|n| n.rect))
      else {
        continue;
      };
      if !track.contains(point) {
        continue;
      }
      let Some(offset) = scroll.current() else {
        return true;
      };
      let from = scroll.along(point);
      if handle.contains(point) {
        self.pointer.bar = Some(BarDrag {
          view: id,
          from,
          offset,
        });
      } else {
        let (start, _) = scroll.span(handle);
        let (_, page) = scroll.span(rect);
        let to = if from < start {
          offset - page
        } else {
          offset + page
        };
        let to = to.clamp(0.0, scroll.most);
        if to != offset {
          scroll.signal.try_set(to);
        }
      }
      return true;
    }
    false
  }

  /// The pointer moved to `at` while dragging a bar's handle: the offset
  /// follows it, the whole track's room standing for the whole content.
  pub(crate) fn bar_motion(&mut self, at: Point) {
    let Some(drag) = self.pointer.bar else {
      return;
    };
    let Some(point) = self.local_point(drag.view, at) else {
      return;
    };
    let Some(NodeKind::Scroll(scroll)) = self.node(drag.view).map(|node| &node.kind) else {
      return;
    };
    let Some((track, handle)) = scroll.handle else {
      return;
    };
    let room = scroll.span(track).1 - scroll.span(handle).1;
    if room <= 0.0 {
      return;
    }
    let to = (drag.offset + (scroll.along(point) - drag.from) * scroll.most / room)
      .clamp(0.0, scroll.most);
    if scroll.current() != Some(to) {
      scroll.signal.try_set(to);
    }
  }
}
