//! Scroll views under the pointer: which one a scroll reaches, the wheel
//! and touchpad along its axis, dragging the child, flings and overshoot
//! on the animation clock, and the scroll bar's handle and track.

use std::rc::Rc;
use std::time::Instant;

use crabshell_reactive::{Signal, untrack};

use crate::animation::{self, Spring};
use crate::geometry::{Point, Rect};
use crate::input::{DRAG_THRESHOLD, Pointer, ScrollSource, Wheel, WheelAxis, local};
use crate::scroll::PIXELS_PER_NOTCH;
use crate::tree::{Axis, Change, NodeId, NodeKind, ScrollNode, Tree};

/// How fast a fling slows, in logical pixels a second each second: Qt's
/// `flickDeceleration`.
const DECELERATION: f32 = 1500.0;

/// The fastest a fling starts, in logical pixels a second: Qt's
/// `maximumFlickVelocity`.
const MAX_SPEED: f32 = 2500.0;

/// A release slower than this, in logical pixels a second, doesn't fling.
const MIN_FLING: f32 = 50.0;

/// How far back from a release, in milliseconds, the moves that set a
/// fling's speed go.
const SAMPLE_MS: u32 = 100;

/// The most a child is shown past an end, as a fraction of the box, and
/// how hard the pull is at first: a pixel moves it half a pixel.
const OVERSHOOT_LIMIT: f32 = 0.25;
const RESISTANCE: f32 = 0.5;

/// What brings an overshoot back: as fast as it can without passing the
/// end.
const SPRING_BACK: Spring = Spring::new(150.0, 1.0);

/// A spring-back nearer than this many pixels to the end and slower than
/// this many a second is at rest.
const REST: (f32, f32) = (0.5, 10.0);

/// The most time one tick moves a motion, in seconds, so a stall doesn't
/// throw the child far.
const MAX_STEP: f32 = 0.25;

/// A scroll view's motion from a touchpad, a drag or the clock.
#[derive(Default)]
pub(crate) struct Motion {
  /// The moves of the current touchpad gesture or drag, in logical
  /// pixels with their times in milliseconds, of the last 100 ms.
  samples: Vec<(u32, f32)>,
  /// How far past an end the touchpad or drag has gone, before the
  /// resistance that `overshoot` shows it with.
  raw: f32,
  /// Counts motions started and stopped: a motion on the clock whose
  /// number is no longer this one stops.
  generation: u64,
}

/// What a motion on the clock is doing.
#[derive(Clone, Copy)]
enum Phase {
  /// Moving at `v` logical pixels a second, slowing down.
  Fling { v: f32 },
  /// Springing back from `x` past an end, at `v`.
  Spring { x: f32, v: f32 },
}

/// A left press over scroll views, which a move past the threshold turns
/// into a drag of one view's child.
pub(crate) struct Flick {
  /// The views under the press, innermost first, each stopped by it.
  views: Vec<NodeId>,
  at: Point,
  last: Point,
  /// The view dragged, once one is.
  view: Option<NodeId>,
  /// Whether a move may still start a drag: not over a button that drags
  /// itself, nor once a move found no view to drag.
  may_drag: bool,
  /// A finger pressed, which drags a view whose `mouse_drag` is off.
  touch: bool,
}

/// A scroll bar held by the left button.
#[derive(Clone, Copy)]
pub(crate) struct BarDrag {
  pub(crate) view: NodeId,
  /// Which of the view's axes the bar runs along.
  axis: usize,
  /// Where the press was along the bar, in the view's own coordinates.
  from: f32,
  /// The offset then.
  offset: f32,
  /// On the handle, which drags; beside it, it paged.
  handle: bool,
}

impl Pointer {
  /// Forgets removed scroll views.
  pub(crate) fn forget_views(&mut self, removed: &[NodeId]) {
    if let Some(flick) = self.flick.as_mut() {
      flick.views.retain(|id| !removed.contains(id));
      if flick.view.is_some_and(|id| removed.contains(&id)) {
        flick.view = None;
        flick.may_drag = false;
      }
    }
    if self.gesture.is_some_and(|id| removed.contains(&id)) {
      self.gesture = None;
    }
    if self.bar.is_some_and(|bar| removed.contains(&bar.view)) {
      self.bar = None;
    }
  }
}

/// Offsets to write once the tree is released, one an axis.
type Writes = [Option<(Signal<f32>, f32)>; 2];

fn write(writes: Writes) {
  for (signal, offset) in writes.into_iter().flatten() {
    signal.try_set(offset);
  }
}

impl Axis {
  /// The offset to scroll from: the signal's, not the last placing's, so
  /// scrolls and writes since then add up, within the content as of the
  /// last placing. `None` once the signal's owner is gone.
  pub(crate) fn current(&self) -> Option<f32> {
    let now = untrack(|| self.signal.try_get())?;
    Some(if now.is_nan() {
      0.0
    } else {
      now.clamp(0.0, self.most)
    })
  }

  /// `delta`'s part along the axis.
  fn part(&self, delta: Point) -> f32 {
    if self.horizontal { delta.x } else { delta.y }
  }

  /// `rect`'s start and length along the axis.
  fn span(&self, rect: Rect) -> (f32, f32) {
    if self.horizontal {
      (rect.x, rect.width)
    } else {
      (rect.y, rect.height)
    }
  }

  fn limit(&self) -> f32 {
    (self.length * OVERSHOOT_LIMIT).max(0.0)
  }

  /// The overshoot shown for `raw` logical pixels past an end: half as far
  /// at first, then less and less, nearing the limit.
  fn rubber(&self, raw: f32) -> f32 {
    let limit = self.limit();
    if limit <= 0.0 || !raw.is_finite() {
      return 0.0;
    }
    raw.signum() * limit * (1.0 - (-RESISTANCE * raw.abs() / limit).exp())
  }

  /// How far past an end shows `shown`: `rubber` turned around.
  fn unrubber(&self, shown: f32) -> f32 {
    let limit = self.limit();
    if limit <= 0.0 || shown == 0.0 {
      return 0.0;
    }
    let part = (shown.abs() / limit).min(0.999);
    -shown.signum() * limit / RESISTANCE * (1.0 - part).ln()
  }

  /// Moves the child `delta` logical pixels at `time`: past an end it goes
  /// on with resistance if it `overshoots`. Returns the offset to write,
  /// if it changed, and whether the overshoot shown did. `None` once the
  /// signal's owner is gone.
  fn push(&mut self, delta: f32, time: u32, overshoots: bool) -> Option<(Option<f32>, bool)> {
    let now = self.current()?;
    let total = now + self.motion.raw + delta;
    let offset = total.clamp(0.0, self.most);
    self.motion.raw = if overshoots { total - offset } else { 0.0 };
    let samples = &mut self.motion.samples;
    samples.retain(|&(then, _)| time.wrapping_sub(then) <= SAMPLE_MS);
    samples.push((time, delta));
    let shown = self.rubber(self.motion.raw);
    let moved = std::mem::replace(&mut self.overshoot, shown) != shown;
    Some(((offset != now).then_some(offset), moved))
  }

  /// The speed of the moves in the 100 ms before `time`, in logical
  /// pixels a second, no faster than a fling starts.
  fn speed(&self, time: u32) -> f32 {
    let recent: Vec<(u32, f32)> = self
      .motion
      .samples
      .iter()
      .copied()
      .filter(|&(then, _)| time.wrapping_sub(then) <= SAMPLE_MS)
      .collect();
    let (Some(first), Some(last)) = (recent.first(), recent.last()) else {
      return 0.0;
    };
    let span = last.0.wrapping_sub(first.0);
    if span == 0 {
      return 0.0;
    }
    // The first move happened before the span began.
    let distance: f32 = recent[1..].iter().map(|&(_, delta)| delta).sum();
    (distance * 1000.0 / span as f32).clamp(-MAX_SPEED, MAX_SPEED)
  }

  /// Whether `delta` can still move the offset, rather than only push
  /// past an end.
  fn can_move(&self, delta: f32) -> bool {
    self
      .current()
      .is_some_and(|now| (delta > 0.0 && now < self.most) || (delta < 0.0 && now > 0.0))
  }
}

impl ScrollNode {
  /// Stops the motion on the clock where it is, and the gesture's speed;
  /// a touch carries on from the overshoot shown.
  fn halt(&mut self) {
    self.set_moving(false);
    for axis in &mut self.axes {
      axis.motion.generation += 1;
      axis.motion.samples.clear();
      axis.motion.raw = axis.unrubber(axis.overshoot);
    }
  }

  /// Stops any motion and shows no overshoot, at once.
  pub(crate) fn stop_overshoot(&mut self) -> Change {
    self.set_moving(false);
    let mut change = Change::None;
    for axis in &mut self.axes {
      axis.motion.generation += 1;
      axis.motion.samples.clear();
      axis.motion.raw = 0.0;
      if std::mem::replace(&mut axis.overshoot, 0.0) != 0.0 {
        change = Change::Place;
      }
    }
    change
  }

  /// Moves the child `delta` logical pixels at `time`, as a touchpad or a
  /// drag does, each axis by its part: past an end it goes on with
  /// resistance. Returns the offsets to write and what the overshoot
  /// repaints.
  fn push(&mut self, delta: Point, time: u32) -> (Writes, Change) {
    let overshoots = self.overshoots;
    let (mut writes, mut change, mut pushed) = ([None; 2], Change::None, false);
    for (slot, axis) in writes.iter_mut().zip(self.active_mut()) {
      let Some((offset, moved)) = axis.push(axis.part(delta), time, overshoots) else {
        continue;
      };
      pushed = true;
      *slot = offset.map(|offset| (axis.signal, offset));
      if moved {
        change = Change::Place;
      }
    }
    if pushed {
      self.set_moving(true);
    }
    (writes, change)
  }

  /// Tells a list whether the view is moving.
  fn set_moving(&self, moving: bool) {
    if let Some(cell) = &self.moving {
      cell.set(moving);
    }
  }

  /// Whether `delta` can still move the offset along an axis, rather
  /// than only push past an end.
  fn can_move(&self, delta: Point) -> bool {
    self
      .active()
      .iter()
      .any(|axis| axis.can_move(axis.part(delta)))
  }

  /// Whether the view scrolls along x, or else along y.
  fn runs(&self, horizontal: bool) -> bool {
    self
      .active()
      .iter()
      .any(|axis| axis.horizontal == horizontal)
  }

  /// How far `wheel` scrolls this view, in logical pixels along x and y: a
  /// wheel's notches 48 pixels each, else a touchpad's pixels. A view
  /// scrolling only sideways takes a turned wheel when there is no
  /// sideways scrolling.
  pub(crate) fn wheel_pixels(&self, wheel: Wheel) -> Option<Point> {
    let notched = |axis: WheelAxis| {
      (axis.notches != 0.0 && axis.notches.is_finite()).then_some(axis.notches * PIXELS_PER_NOTCH)
    };
    let smooth =
      |axis: WheelAxis| (axis.pixels != 0.0 && axis.pixels.is_finite()).then_some(axis.pixels);
    let along = |axis: WheelAxis| notched(axis).or_else(|| smooth(axis));
    let (x, y) = match self.active() {
      [main] if main.horizontal => (along(wheel.x).or_else(|| notched(wheel.y)), None),
      [_] => (None, along(wheel.y)),
      _ => (along(wheel.x), along(wheel.y)),
    };
    (x.is_some() || y.is_some()).then(|| Point::new(x.unwrap_or(0.0), y.unwrap_or(0.0)))
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

  fn scroll_node(&self, id: NodeId) -> Option<&ScrollNode> {
    match &self.node(id)?.kind {
      NodeKind::Scroll(scroll) => Some(scroll),
      _ => None,
    }
  }

  /// Scrolls the innermost scroll view under the pointer that `wheel`
  /// can still move, at `time`. A wheel's notch moves it at once, within
  /// its content. A touchpad moves it past an end with resistance.
  pub(crate) fn scroll_view(&mut self, wheel: Wheel, time: u32) {
    let finger = wheel.source == ScrollSource::Finger;
    let Some((id, pixels)) = self.wheel_target(wheel, finger) else {
      return;
    };
    if finger {
      if self.pointer.gesture != Some(id) {
        if let Some(last) = self.pointer.gesture.replace(id) {
          self.release_view(last, None);
        }
        self.update_scroll(id, |scroll| {
          scroll.halt();
          Change::None
        });
      }
      self.push_view(id, pixels, time);
      return;
    }
    let mut writes = [None; 2];
    self.update_scroll(id, |scroll| {
      let change = scroll.stop_overshoot();
      for (slot, axis) in writes.iter_mut().zip(scroll.active()) {
        if let Some(now) = axis.current() {
          let offset = (now + axis.part(pixels)).clamp(0.0, axis.most);
          *slot = (offset != now).then_some((axis.signal, offset));
        }
      }
      change
    });
    write(writes);
  }

  /// The view `wheel` scrolls and by how much along x and y: the
  /// innermost under the pointer it can still move, else for a touchpad
  /// the innermost it can push past an end, or the one it already did.
  fn wheel_target(&self, wheel: Wheel, finger: bool) -> Option<(NodeId, Point)> {
    let chain = self.scroll_views_at(self.pointer.position?);
    let past = self
      .pointer
      .gesture
      .filter(|id| finger && chain.contains(id))
      .filter(|&id| {
        self
          .scroll_node(id)
          .is_some_and(|scroll| scroll.active().iter().any(|axis| axis.motion.raw != 0.0))
      });
    if let Some(id) = past {
      return Some((id, self.scroll_node(id)?.wheel_pixels(wheel)?));
    }
    let moves = chain.iter().find_map(|&id| {
      let scroll = self.scroll_node(id)?;
      let pixels = scroll.wheel_pixels(wheel)?;
      scroll.can_move(pixels).then_some((id, pixels))
    });
    moves.or_else(|| {
      chain.iter().filter(|_| finger).find_map(|&id| {
        let scroll = self.scroll_node(id)?;
        Some((
          id,
          scroll.wheel_pixels(wheel).filter(|_| scroll.overshoots)?,
        ))
      })
    })
  }

  /// Moves view `id`'s child `delta` logical pixels along x and y at
  /// `time`, as a touchpad or a drag does.
  fn push_view(&mut self, id: NodeId, delta: Point, time: u32) {
    let mut writes = [None; 2];
    self.update_scroll(id, |scroll| {
      let (pushed, change) = scroll.push(delta, time);
      writes = pushed;
      change
    });
    write(writes);
  }

  /// Lets go of view `id` after a touchpad gesture or a drag that ended
  /// at `time`, if it moved it: past an end each axis springs back, and
  /// otherwise a kinetic view goes on at the speed it moved.
  pub(crate) fn release_view(&mut self, id: NodeId, time: Option<u32>) {
    let Some(NodeKind::Scroll(scroll)) = self.node_mut(id).map(|node| &mut node.kind) else {
      return;
    };
    let kinetic = scroll.kinetic;
    let mut phases = [None; 2];
    for (phase, axis) in phases.iter_mut().zip(scroll.active_mut()) {
      let speed = time.map_or(0.0, |time| axis.speed(time));
      axis.motion.samples.clear();
      axis.motion.raw = 0.0;
      *phase = if axis.overshoot != 0.0 {
        Some(Phase::Spring {
          x: axis.overshoot,
          v: 0.0,
        })
      } else if kinetic && speed.abs() >= MIN_FLING {
        Some(Phase::Fling { v: speed })
      } else {
        None
      };
    }
    if phases.iter().all(Option::is_none) {
      scroll.set_moving(false);
    }
    for (axis, phase) in phases.into_iter().enumerate() {
      if let Some(phase) = phase {
        self.start_motion(id, axis, phase);
      }
    }
  }

  /// Moves view `id` along `axis` on the animation clock from `phase`
  /// until it rests, another motion replaces it, or the offset is written
  /// from outside.
  fn start_motion(&mut self, id: NodeId, axis: usize, mut phase: Phase) {
    let tree = self.this.clone();
    let Some(node) = self.node_mut(id) else {
      return;
    };
    let label: Rc<str> = format!("{}.kinetic", node.path).into();
    let busy = node.cx.busy();
    let NodeKind::Scroll(scroll) = &mut node.kind else {
      return;
    };
    let Some(mut written) = scroll.axes[axis].current() else {
      return;
    };
    if let Some(shifts) = scroll.shifts.as_ref().filter(|_| axis == 0) {
      shifts.set(0.0);
    }
    let motion = &mut scroll.axes[axis].motion;
    motion.generation += 1;
    let generation = motion.generation;
    let mut previous: Option<Instant> = None;
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
        let seconds = previous.map_or(0.0, |previous| {
          now
            .saturating_duration_since(previous)
            .as_secs_f32()
            .min(MAX_STEP)
        });
        previous = Some(now);
        let step = Step {
          id,
          axis,
          generation,
          seconds,
        };
        tree.step_motion(step, &mut phase, &mut written)
      }),
    );
  }

  /// One tick of a motion. `written` is the offset it last wrote. Returns
  /// whether it still moves.
  fn step_motion(&mut self, step: Step, phase: &mut Phase, written: &mut f32) -> bool {
    let mut running = false;
    let mut write = None;
    self.update_scroll(step.id, |scroll| {
      let ScrollNode {
        axes,
        overshoots,
        shifts,
        ..
      } = scroll;
      let axis = &mut axes[step.axis];
      if axis.motion.generation != step.generation {
        return Change::None;
      }
      let Some(offset) = axis.current() else {
        return Change::None;
      };
      if let Some(shifts) = shifts.as_ref().filter(|_| step.axis == 0) {
        *written += shifts.take();
      }
      // A write from outside, or content that shrank under it, stops it.
      if offset != *written {
        return scroll.stop_overshoot();
      }
      let seconds = step.seconds;
      let mut to = offset;
      let mut overshoot = axis.overshoot;
      match phase {
        Phase::Fling { v } => {
          let slowed = v.signum() * (v.abs() - DECELERATION * seconds).max(0.0);
          let moved = offset + (*v + slowed) / 2.0 * seconds;
          to = moved.clamp(0.0, axis.most);
          if moved != to && *overshoots {
            // Past an end, the spring takes over at the speed it had.
            overshoot = (moved - to).clamp(-axis.limit(), axis.limit());
            *phase = Phase::Spring {
              x: overshoot,
              v: slowed,
            };
            running = true;
          } else {
            *phase = Phase::Fling { v: slowed };
            running = moved == to && slowed != 0.0;
          }
        }
        Phase::Spring { x, v } => {
          let (mut x64, mut v64) = (f64::from(*x), f64::from(*v));
          SPRING_BACK.advance(&mut x64, &mut v64, 0.0, f64::from(seconds));
          let limit = axis.limit();
          let clamped = (x64 as f32).clamp(-limit, limit);
          let rest = clamped.abs() < REST.0 && (v64 as f32).abs() < REST.1;
          *x = if rest { 0.0 } else { clamped };
          *v = v64 as f32;
          overshoot = *x;
          running = !rest;
        }
      }
      if to != offset {
        *written = to;
        write = Some((axis.signal, to));
      }
      let moved = std::mem::replace(&mut axis.overshoot, overshoot) != overshoot;
      if !running {
        scroll.set_moving(false);
      }
      if moved { Change::Place } else { Change::None }
    });
    if let Some((signal, offset)) = write {
      signal.try_set(offset);
    }
    running
  }

  /// A left press at `at`, by a finger if `touch`: the scroll views under
  /// it stop where they are, and unless it is on a button that drags, a
  /// move may drag one.
  pub(crate) fn press_views(&mut self, at: Point, drags: bool, touch: bool) {
    let mut views = self.scroll_views_at(at);
    views.retain(|&id| self.press_reaches(id));
    for &id in &views {
      self.update_scroll(id, |scroll| {
        scroll.halt();
        Change::None
      });
    }
    self.pointer.flick = (!views.is_empty()).then_some(Flick {
      views,
      at,
      last: at,
      view: None,
      may_drag: !drags,
      touch,
    });
  }

  /// The left button held moved to `at` at `time`. Past the threshold
  /// along a view's axis, the innermost view along it that can move, or
  /// else push past an end, follows the pointer from the press, and the
  /// button pressed lets go without a click. Returns whether a view's
  /// child is dragged.
  pub(crate) fn flick_motion(&mut self, at: Point, time: u32) -> bool {
    let Some(flick) = self.pointer.flick.as_mut() else {
      return false;
    };
    let last = std::mem::replace(&mut flick.last, at);
    if let Some(id) = flick.view {
      let delta = self.drag_delta(id, last, at);
      self.push_view(id, delta, time);
      return true;
    }
    let (dx, dy) = (at.x - flick.at.x, at.y - flick.at.y);
    if !flick.may_drag || dx.abs().max(dy.abs()) <= DRAG_THRESHOLD {
      return false;
    }
    let (from, views, touch) = (flick.at, flick.views.clone(), flick.touch);
    let sideways = dx.abs() > dy.abs();
    let along: Vec<(NodeId, Point)> = views
      .into_iter()
      .filter(|&id| {
        self
          .scroll_node(id)
          .is_some_and(|scroll| scroll.runs(sideways) && (touch || scroll.mouse_drag))
      })
      .map(|id| (id, self.drag_delta(id, from, at)))
      .collect();
    let can =
      |id: NodeId, test: &dyn Fn(&ScrollNode) -> bool| self.scroll_node(id).is_some_and(test);
    // Only the way the drag mostly goes picks the view.
    let mostly = |delta: Point| {
      if sideways {
        Point::new(delta.x, 0.0)
      } else {
        Point::new(0.0, delta.y)
      }
    };
    let picked = along
      .iter()
      .find(|&&(id, delta)| can(id, &|scroll| scroll.can_move(mostly(delta))))
      .or_else(|| {
        along
          .iter()
          .find(|&&(id, _)| can(id, &|scroll| scroll.overshoots))
      })
      .copied();
    let Some((id, delta)) = picked else {
      if let Some(flick) = self.pointer.flick.as_mut() {
        flick.may_drag = false;
      }
      return false;
    };
    if let Some(flick) = self.pointer.flick.as_mut() {
      flick.view = Some(id);
    }
    self.cancel_press();
    self.push_view(id, delta, time);
    true
  }

  /// How far a drag of view `id`'s child from `from` to `to`, in window
  /// coordinates, scrolls it along x and y in its own coordinates.
  fn drag_delta(&self, id: NodeId, from: Point, to: Point) -> Point {
    match (self.local_point(id, from), self.local_point(id, to)) {
      (Some(from), Some(to)) => Point::new(from.x - to.x, from.y - to.y),
      _ => Point::default(),
    }
  }

  /// The left button came up at `time`, or the pointer left with no
  /// time: the view dragged flings or springs back, and the others the
  /// press stopped spring back.
  pub(crate) fn flick_release(&mut self, time: Option<u32>) {
    let Some(flick) = self.pointer.flick.take() else {
      return;
    };
    for id in flick.views {
      let time = time.filter(|_| flick.view == Some(id));
      self.release_view(id, time);
    }
  }

  /// A left press at `at` on a scroll bar: on the handle it starts
  /// dragging it, and beside it pages toward the press. Returns whether
  /// a bar took it.
  pub(crate) fn press_bar(&mut self, at: Point) -> bool {
    for id in self.scroll_views_at(at) {
      if !self.press_reaches(id) {
        continue;
      }
      let Some(point) = self.local_point(id, at) else {
        continue;
      };
      let (Some(scroll), Some(rect)) = (self.scroll_node(id), self.node(id).and_then(|n| n.rect))
      else {
        continue;
      };
      let held = scroll.active().iter().enumerate().find_map(|(n, axis)| {
        let (track, handle) = axis.handle?;
        track.contains(point).then_some((n, handle))
      });
      let Some((n, handle)) = held else {
        continue;
      };
      let axis = &scroll.axes[n];
      let Some(offset) = axis.current() else {
        return true;
      };
      let from = axis.part(point);
      let on_handle = handle.contains(point);
      let (start, _) = axis.span(handle);
      let (_, page) = axis.span(rect);
      let to = if on_handle {
        offset
      } else if from < start {
        offset - page
      } else {
        offset + page
      };
      let (to, signal) = (to.clamp(0.0, axis.most), axis.signal);
      // It stops at once, as under any press, until the bar lets go.
      self.update_scroll(id, |scroll| {
        scroll.halt();
        Change::None
      });
      self.pointer.bar = Some(BarDrag {
        view: id,
        axis: n,
        from,
        offset,
        handle: on_handle,
      });
      if to != offset {
        signal.try_set(to);
      }
      return true;
    }
    false
  }

  /// Whether a press may go to scroll view `id`: nothing is hovered, or
  /// what is lies inside the view, not drawn over it from outside.
  fn press_reaches(&self, id: NodeId) -> bool {
    self
      .pointer
      .hovered
      .is_none_or(|hovered| self.path_to(hovered).contains(&id))
  }

  /// The pointer moved to `at` while dragging a bar's handle: the offset
  /// follows it, the whole track's room standing for the whole content.
  pub(crate) fn bar_motion(&mut self, at: Point) {
    let Some(drag) = self.pointer.bar.filter(|bar| bar.handle) else {
      return;
    };
    let Some(point) = self.local_point(drag.view, at) else {
      return;
    };
    let Some(axis) = self
      .scroll_node(drag.view)
      .map(|scroll| &scroll.axes[drag.axis])
    else {
      return;
    };
    let Some((track, handle)) = axis.handle else {
      return;
    };
    let room = axis.span(track).1 - axis.span(handle).1;
    if room <= 0.0 {
      return;
    }
    let to =
      (drag.offset + (axis.part(point) - drag.from) * axis.most / room).clamp(0.0, axis.most);
    if axis.current() != Some(to) {
      axis.signal.try_set(to);
    }
  }
}

/// What one tick of a motion moves: view `id` along `axis`, in its motion
/// number `generation`, `seconds` after the last tick.
struct Step {
  id: NodeId,
  axis: usize,
  generation: u64,
  seconds: f32,
}
