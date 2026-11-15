//! Scroll views under the pointer: which one a scroll reaches, the wheel
//! and touchpad along its axis, dragging the child, flings and overshoot
//! on the animation clock, and the scroll bar's handle and track.

use std::rc::Rc;
use std::time::Instant;

use crabshell_reactive::untrack;

use crate::animation::{self, Spring};
use crate::geometry::{Point, Rect};
use crate::input::{Pointer, ScrollSource, Wheel, WheelAxis, local};
use crate::scroll::PIXELS_PER_NOTCH;
use crate::tree::{Change, NodeId, NodeKind, ScrollNode, Tree};

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

/// A scroll bar's handle held by the left button.
#[derive(Clone, Copy)]
pub(crate) struct BarDrag {
  pub(crate) view: NodeId,
  /// Where the press was along the bar, in the view's own coordinates.
  from: f32,
  /// The offset then.
  offset: f32,
}

impl Pointer {
  /// Forgets removed scroll views.
  pub(crate) fn forget_views(&mut self, removed: &[NodeId]) {
    if self.gesture.is_some_and(|id| removed.contains(&id)) {
      self.gesture = None;
    }
    if self.bar.is_some_and(|bar| removed.contains(&bar.view)) {
      self.bar = None;
    }
  }
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

  /// Stops the motion on the clock where it is, and the gesture's speed;
  /// a touch carries on from the overshoot shown.
  fn halt(&mut self) {
    self.motion.generation += 1;
    self.motion.samples.clear();
    self.motion.raw = self.unrubber(self.overshoot);
  }

  /// Stops any motion and shows no overshoot, at once.
  pub(crate) fn stop_overshoot(&mut self) -> Change {
    self.motion.generation += 1;
    self.motion.samples.clear();
    self.motion.raw = 0.0;
    if std::mem::replace(&mut self.overshoot, 0.0) != 0.0 {
      Change::Place
    } else {
      Change::None
    }
  }

  /// Moves the child `delta` logical pixels at `time`, as a touchpad or a
  /// drag does: past an end it goes on with resistance. Returns the
  /// offset to write, if it changed, and what the overshoot repaints.
  fn push(&mut self, delta: f32, time: u32) -> (Option<f32>, Change) {
    let Some(now) = self.current() else {
      return (None, Change::None);
    };
    let total = now + self.motion.raw + delta;
    let offset = total.clamp(0.0, self.most);
    self.motion.raw = if self.overshoots { total - offset } else { 0.0 };
    let samples = &mut self.motion.samples;
    samples.retain(|&(then, _)| time.wrapping_sub(then) <= SAMPLE_MS);
    samples.push((time, delta));
    let shown = self.rubber(self.motion.raw);
    let change = if std::mem::replace(&mut self.overshoot, shown) != shown {
      Change::Place
    } else {
      Change::None
    };
    ((offset != now).then_some(offset), change)
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
  /// can still move, at `time`. A wheel's notch moves it at once, within
  /// its content. A touchpad moves it past an end with resistance, and
  /// its `stop` lets the view go: back from past an end, or on at the
  /// speed it moved.
  pub(crate) fn scroll_view(&mut self, wheel: Wheel, time: u32) {
    let finger = wheel.source == ScrollSource::Finger;
    if let Some((id, pixels)) = self.wheel_target(wheel, finger) {
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
      } else {
        let mut write = None;
        self.update_scroll(id, |scroll| {
          let change = scroll.stop_overshoot();
          if let Some(now) = scroll.current() {
            let offset = (now + pixels).clamp(0.0, scroll.most);
            write = (offset != now).then_some((scroll.signal, offset));
          }
          change
        });
        if let Some((signal, offset)) = write {
          signal.try_set(offset);
        }
      }
    }
    if finger
      && (wheel.x.stop || wheel.y.stop)
      && let Some(id) = self.pointer.gesture.take()
    {
      self.release_view(id, Some(time));
    }
  }

  /// The view `wheel` scrolls and by how much: the innermost under the
  /// pointer it can still move, else for a touchpad the innermost it can
  /// push past an end, or the one it already did.
  fn wheel_target(&self, wheel: Wheel, finger: bool) -> Option<(NodeId, f32)> {
    let chain = self.scroll_views_at(self.pointer.position?);
    let scroll = |id: NodeId| match self.node(id).map(|node| &node.kind) {
      Some(NodeKind::Scroll(scroll)) => Some(&**scroll),
      _ => None,
    };
    let past = self
      .pointer
      .gesture
      .filter(|id| finger && chain.contains(id))
      .filter(|&id| scroll(id).is_some_and(|scroll| scroll.motion.raw != 0.0));
    if let Some(id) = past {
      return Some((id, scroll(id)?.wheel_pixels(wheel)?));
    }
    let moves = chain.iter().find_map(|&id| {
      let pixels = scroll(id)?.wheel_pixels(wheel)?;
      scroll(id)?.can_move(pixels).then_some((id, pixels))
    });
    moves.or_else(|| {
      chain.iter().filter(|_| finger).find_map(|&id| {
        let scroll = scroll(id)?;
        Some((
          id,
          scroll.wheel_pixels(wheel).filter(|_| scroll.overshoots)?,
        ))
      })
    })
  }

  /// Moves view `id`'s child `delta` logical pixels at `time`, as a
  /// touchpad or a drag does.
  fn push_view(&mut self, id: NodeId, delta: f32, time: u32) {
    let mut write = None;
    self.update_scroll(id, |scroll| {
      let (offset, change) = scroll.push(delta, time);
      write = offset.map(|offset| (scroll.signal, offset));
      change
    });
    if let Some((signal, offset)) = write {
      signal.try_set(offset);
    }
  }

  /// Lets go of view `id` after a touchpad gesture or a drag that ended
  /// at `time`, if it moved it: past an end it springs back, and
  /// otherwise a kinetic view goes on at the speed it moved.
  pub(crate) fn release_view(&mut self, id: NodeId, time: Option<u32>) {
    let Some(NodeKind::Scroll(scroll)) = self.node_mut(id).map(|node| &mut node.kind) else {
      return;
    };
    let speed = time.map_or(0.0, |time| scroll.speed(time));
    scroll.motion.samples.clear();
    scroll.motion.raw = 0.0;
    let phase = if scroll.overshoot != 0.0 {
      Phase::Spring {
        x: scroll.overshoot,
        v: 0.0,
      }
    } else if scroll.kinetic && speed.abs() >= MIN_FLING {
      Phase::Fling { v: speed }
    } else {
      return;
    };
    self.start_motion(id, phase);
  }

  /// Moves view `id` on the animation clock from `phase` until it rests,
  /// another motion replaces it, or the offset is written from outside.
  fn start_motion(&mut self, id: NodeId, mut phase: Phase) {
    let tree = self.this.clone();
    let Some(node) = self.node_mut(id) else {
      return;
    };
    let label: Rc<str> = format!("{}.kinetic", node.path).into();
    let NodeKind::Scroll(scroll) = &mut node.kind else {
      return;
    };
    let Some(mut written) = scroll.current() else {
      return;
    };
    scroll.motion.generation += 1;
    let generation = scroll.motion.generation;
    let mut previous: Option<Instant> = None;
    animation::start(
      label,
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
        tree.step_motion(id, generation, &mut phase, &mut written, seconds)
      }),
    );
  }

  /// One tick of view `id`'s motion number `generation`, `seconds` after
  /// the last. `written` is the offset it last wrote. Returns whether it
  /// still moves.
  fn step_motion(
    &mut self,
    id: NodeId,
    generation: u64,
    phase: &mut Phase,
    written: &mut f32,
    seconds: f32,
  ) -> bool {
    let mut running = false;
    let mut write = None;
    self.update_scroll(id, |scroll| {
      if scroll.motion.generation != generation {
        return Change::None;
      }
      let Some(offset) = scroll.current() else {
        return Change::None;
      };
      // A write from outside, or content that shrank under it, stops it.
      if offset != *written {
        return scroll.stop_overshoot();
      }
      let mut to = offset;
      let mut overshoot = scroll.overshoot;
      match phase {
        Phase::Fling { v } => {
          let slowed = v.signum() * (v.abs() - DECELERATION * seconds).max(0.0);
          let moved = offset + (*v + slowed) / 2.0 * seconds;
          to = moved.clamp(0.0, scroll.most);
          if moved != to && scroll.overshoots {
            // Past an end, the spring takes over at the speed it had.
            overshoot = (moved - to).clamp(-scroll.limit(), scroll.limit());
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
          let limit = scroll.limit();
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
        write = Some((scroll.signal, to));
      }
      if std::mem::replace(&mut scroll.overshoot, overshoot) != overshoot {
        Change::Place
      } else {
        Change::None
      }
    });
    if let Some((signal, offset)) = write {
      signal.try_set(offset);
    }
    running
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
