//! [`ScrollView`], a box that shows part of a taller child, and
//! [`ListView`], a scrolling list that builds only the rows in view.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;

use crabshell_reactive::{Memo, Signal, untrack};

use crate::cx::Cx;
use crate::geometry::{Point, Size};
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::widgets::{
  Column, Common, Element, ElementChild, ElementKind, Stack, Widget, visibility_methods,
};

/// Logical pixels a mouse wheel scrolls per notch.
pub(crate) const PIXELS_PER_NOTCH: f32 = 48.0;

/// The scroll bar's thickness when its style sets no `width`.
pub(crate) const BAR_WIDTH: f32 = 6.0;

/// The shortest a scroll bar's handle gets, so a long list's stays
/// grabbable.
pub(crate) const BAR_MIN_LENGTH: f32 = 16.0;

pub(crate) struct ScrollProps {
  pub(crate) offset: Option<Signal<f32>>,
  pub(crate) horizontal: Option<Prop<bool>>,
  pub(crate) kinetic: Option<Prop<bool>>,
  pub(crate) overshoot: Option<Prop<bool>>,
  pub(crate) scroll_bar: Option<Prop<Option<Style>>>,
  /// What a list moved the offset by to keep the rows shown still, which
  /// a fling takes as its own motion rather than a write that stops it.
  pub(crate) shifts: Option<Rc<Cell<f32>>>,
}

/// A box that shows part of its child, which may be taller, and scrolls
/// it with the mouse wheel or touchpad. Children are cut to the box. Give
/// it a height through its style, or let its parent size it.
///
/// ```
/// use crabshell_ui::{Column, ScrollView, Style, Text};
///
/// let log = ScrollView::new(Column::new().child(Text::new("first line")))
///     .style(Style::new().height(200.0));
/// ```
pub struct ScrollView {
  scroll: ScrollProps,
  common: Common,
  child: Box<dyn Widget>,
}

impl ScrollView {
  /// A scroll view of `child`.
  pub fn new(child: impl Widget) -> ScrollView {
    ScrollView {
      scroll: ScrollProps {
        offset: None,
        horizontal: None,
        kinetic: None,
        overshoot: None,
        scroll_bar: None,
        shifts: None,
      },
      common: Common::default(),
      child: Box::new(child),
    }
  }

  /// Shares the offset: how many logical pixels of the child are
  /// scrolled out of view above, or to the left in a horizontal view.
  /// Scrolling writes it, and setting it scrolls, stopping a fling. Values
  /// past either end show that end.
  pub fn offset(mut self, offset: Signal<f32>) -> ScrollView {
    self.scroll.offset = Some(offset);
    self
  }

  /// Scrolls sideways instead: the child is laid out at its full width
  /// rather than its full height, and the wheel, tilted or turned, and the
  /// touchpad's sideways motion scroll it. Both at once isn't offered.
  pub fn horizontal(mut self, horizontal: impl IntoProp<bool>) -> ScrollView {
    self.scroll.horizontal = Some(horizontal.into_prop());
    self
  }

  /// Keeps the child moving after a touchpad fling or a drag of the child
  /// ends, slowing down by 1500 logical pixels a second each second from
  /// at most 2500 a second, as Qt's `Flickable`. On by default. A mouse
  /// wheel's notch still moves 48 pixels at once.
  pub fn kinetic(mut self, kinetic: impl IntoProp<bool>) -> ScrollView {
    self.scroll.kinetic = Some(kinetic.into_prop());
    self
  }

  /// Lets a touchpad, a drag or a fling carry the child past either end,
  /// against a pull that grows with the distance and stops it within a
  /// quarter of the box, and springs it back once let go, as Qt's
  /// `DragAndOvershootBounds`. Only shown: the offset stays within the
  /// content. On by default.
  pub fn overshoot(mut self, overshoot: impl IntoProp<bool>) -> ScrollView {
    self.scroll.overshoot = Some(overshoot.into_prop());
    self
  }

  /// Draws a scroll bar while the child is longer than the box: a handle
  /// along the right edge, or the bottom of a horizontal view, inside the
  /// border, as long as the part shown and placed by the offset, over
  /// the child. Its style's `background` and `radius` draw the handle,
  /// and its `width`, 6 by default, is its thickness. Dragging the handle
  /// scrolls, and a press beside it pages toward the press. `None`, the
  /// default, draws none.
  pub fn scroll_bar(mut self, style: impl IntoProp<Option<Style>>) -> ScrollView {
    self.scroll.scroll_bar = Some(style.into_prop());
    self
  }

  /// Sets the style. A scroll view cuts its child to the inside of its
  /// border whatever `clip` says.
  pub fn style(mut self, style: impl IntoProp<Style>) -> ScrollView {
    self.common.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for ScrollView {
  fn into_element(self: Box<Self>) -> Element {
    let ScrollView {
      scroll,
      common,
      child,
    } = *self;
    Element {
      name: "ScrollView",
      common,
      kind: ElementKind::Scroll(Box::new(scroll)),
      children: vec![ElementChild::One(Box::new(child.into_element()))],
    }
  }
}

/// A scrolling list that builds only the rows in view, so a list of
/// thousands costs what a screenful does. Offsets are `f32`, which stays
/// exact to about 16 million pixels: some 700,000 rows of 24 pixels. It
/// follows a list of items as [`children_keyed`](Column::children_keyed)
/// does: each row is kept while its item's key stays in the list.
///
/// Rows may differ in height. Each is at least
/// [`row_height`](Self::row_height) tall and grows to fit its content,
/// and its height, measured once it is laid out, is kept by its key. A row
/// not built yet counts as `row_height`, so set it near the usual row's
/// height. A row that starts above the view grows upward, so the rows
/// shown stay where they are.
///
/// ```
/// use crabshell_ui::{ListView, Text};
///
/// let apps = ListView::new(
///     || (1..=500).map(|n| format!("app {n}")).collect::<Vec<_>>(),
///     |name| name.clone(),
///     |_cx, name| Text::new(move || name.get()),
/// )
/// .row_height(28)
/// .height(280);
/// ```
pub struct ListView {
  /// Builds the list once it has a context for its signals.
  build: Build,
  props: ListProps,
  common: Common,
}

/// Builds a list's element from its properties, with a context.
type Build = Box<dyn FnOnce(&Cx, ListProps) -> Element>;

struct ListProps {
  row_height: Option<Prop<u32>>,
  height: Option<Prop<u32>>,
  current: Option<Prop<Option<usize>>>,
  offset: Option<Signal<f32>>,
  style: Option<Prop<Style>>,
  scroll_bar: Option<Prop<Option<Style>>>,
  kinetic: Option<Prop<bool>>,
  overshoot: Option<Prop<bool>>,
  horizontal: Option<Prop<bool>>,
  highlight: Option<Prop<Option<Style>>>,
}

impl ListView {
  /// The default row height, in logical pixels.
  pub const DEFAULT_ROW_HEIGHT: u32 = 24;
  /// The default height of the list, in logical pixels.
  pub const DEFAULT_HEIGHT: u32 = 240;

  /// A list with a row for each of `items`, built by `view` and kept by
  /// `key`, as in [`children_keyed`](Column::children_keyed).
  pub fn new<T, K, W>(
    items: impl FnMut() -> Vec<T> + 'static,
    key: impl FnMut(&T) -> K + 'static,
    view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
  ) -> ListView
  where
    T: Clone + PartialEq + 'static,
    K: Eq + Hash + 'static,
    W: Widget,
  {
    ListView {
      build: Box::new(move |cx, props| build(cx, props, items, key, view)),
      props: ListProps {
        row_height: None,
        height: None,
        current: None,
        offset: None,
        style: None,
        scroll_bar: None,
        kinetic: None,
        overshoot: None,
        horizontal: None,
        highlight: None,
      },
      common: Common::default(),
    }
  }

  /// Runs the rows left to right in a horizontal scroll view instead.
  /// [`row_height`](Self::row_height) and [`height`](Self::height) then
  /// mean each row's width and the list's, and each row is as tall as the
  /// list. Turning it measures every row again.
  pub fn horizontal(mut self, horizontal: impl IntoProp<bool>) -> ListView {
    self.props.horizontal = Some(horizontal.into_prop());
    self
  }

  /// Sets the height in logical pixels that each row has at least, and
  /// that a row not yet built counts as. 24 by default.
  pub fn row_height(mut self, height: impl IntoProp<u32>) -> ListView {
    self.props.row_height = Some(height.into_prop());
    self
  }

  /// Sets the list's height in logical pixels.
  pub fn height(mut self, height: impl IntoProp<u32>) -> ListView {
    self.props.height = Some(height.into_prop());
    self
  }

  /// Keeps the row at this index in view, scrolling as little as needed
  /// whenever it changes. A row not built yet is found by `row_height`,
  /// then shown whole once it is measured. `None`, or an index past the
  /// end, scrolls nothing.
  pub fn current(mut self, index: impl IntoProp<Option<usize>>) -> ListView {
    self.props.current = Some(index.into_prop());
    self
  }

  /// Draws a box in this style behind the [`current`](Self::current)
  /// row, as long as the row and across the list. When the current row
  /// changes, the box moves to it over the style's `transition` rather
  /// than jumping, and takes its length at once. It is drawn moved, so a
  /// move repaints but doesn't lay the window out. No current row, or
  /// `None`, the default, draws none.
  pub fn highlight(mut self, style: impl IntoProp<Option<Style>>) -> ListView {
    self.props.highlight = Some(style.into_prop());
    self
  }

  /// Shares the offset, as [`ScrollView::offset`] does.
  pub fn offset(mut self, offset: Signal<f32>) -> ListView {
    self.props.offset = Some(offset);
    self
  }

  /// Keeps the list moving after a fling, as [`ScrollView::kinetic`]
  /// does. On by default.
  pub fn kinetic(mut self, kinetic: impl IntoProp<bool>) -> ListView {
    self.props.kinetic = Some(kinetic.into_prop());
    self
  }

  /// Lets the list go past its ends, as [`ScrollView::overshoot`] does.
  /// On by default.
  pub fn overshoot(mut self, overshoot: impl IntoProp<bool>) -> ListView {
    self.props.overshoot = Some(overshoot.into_prop());
    self
  }

  /// Draws a scroll bar, as [`ScrollView::scroll_bar`] does.
  pub fn scroll_bar(mut self, style: impl IntoProp<Option<Style>>) -> ListView {
    self.props.scroll_bar = Some(style.into_prop());
    self
  }

  /// Sets the list's style.
  pub fn style(mut self, style: impl IntoProp<Style>) -> ListView {
    self.props.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for ListView {
  fn into_element(self: Box<Self>) -> Element {
    let ListView {
      build,
      props,
      common,
    } = *self;
    let mut once = Some((build, props));
    // A keyed list of one is how a widget gets a context to make its
    // signals with.
    let mut element = Box::new(Stack::new().children_keyed(
      || vec![()],
      |_| (),
      move |cx, _| Built(once.take().map(|(build, props)| build(cx, props))),
    ))
    .into_element();
    element.name = "ListView";
    element.common = common;
    element
  }
}

/// An element already described, as a widget.
pub(crate) struct Built(pub(crate) Option<Element>);

impl Widget for Built {
  fn into_element(self: Box<Self>) -> Element {
    self
      .0
      .unwrap_or_else(|| Box::new(Stack::new()).into_element())
  }
}

/// What a list knows of its rows' lengths.
struct Lengths<K> {
  /// Each item's index, by key; a repeated key keeps the first.
  index: HashMap<K, usize>,
  /// Each item's measured length, by index, once its row was laid out.
  measured: Vec<Option<f32>>,
  /// Rows measured since the last look: their index and length.
  pending: Vec<(usize, f32)>,
  /// A current row scrolled to by the estimate, shown again once it is
  /// measured.
  seek: Option<usize>,
}

/// Where each row starts if a row not measured is `estimate` long, and
/// where the last ends.
fn starts(measured: &[Option<f32>], estimate: f32) -> Vec<f32> {
  let mut starts = Vec::with_capacity(measured.len() + 1);
  let mut at = 0.0;
  starts.push(at);
  for length in measured {
    at += length.unwrap_or(estimate);
    starts.push(at);
  }
  starts
}

/// The offset that shows `start..end` in a view `viewport` long at `now`,
/// scrolling as little as it can.
pub(crate) fn keep_in_view(start: f32, end: f32, viewport: f32, now: f32) -> f32 {
  if start < now {
    start
  } else if end > now + viewport {
    end - viewport
  } else {
    now
  }
}

fn build<T, K, W>(
  cx: &Cx,
  props: ListProps,
  mut items: impl FnMut() -> Vec<T> + 'static,
  key: impl FnMut(&T) -> K + 'static,
  mut view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
) -> Element
where
  T: Clone + PartialEq + 'static,
  K: Eq + Hash + 'static,
  W: Widget,
{
  let ListProps {
    row_height,
    height,
    current,
    offset,
    style,
    scroll_bar,
    kinetic,
    overshoot,
    horizontal,
    highlight: highlight_style,
  } = props;
  let offset = offset.unwrap_or_else(|| cx.signal(0.0));
  let horizontal = cx.memo(prop_fn(horizontal, false));
  let row_height = cx.memo(prop_fn(row_height, ListView::DEFAULT_ROW_HEIGHT));
  let estimate = move || row_height.get().max(1) as f32;
  let height = cx.memo(prop_fn(height, ListView::DEFAULT_HEIGHT));
  let style = style.map(Prop::shared);
  // The length rows show in: the list's, less its padding and border.
  let inner = {
    let style = style.clone();
    cx.memo(move || {
      let own = style.as_ref().map(|style| style()).unwrap_or_default();
      let padding = own.padding.unwrap_or_default();
      let border = own.border.map_or(0.0, |border| border.width.max(0.0));
      let sides = if horizontal.get() {
        padding.left + padding.right
      } else {
        padding.top + padding.bottom
      };
      (height.get() as f32 - sides as f32 - 2.0 * border).max(0.0)
    })
  };
  // Shared by the list, its keyed rows and each row, which never call it
  // inside one another.
  let key = Rc::new(RefCell::new(key));
  let lengths = Rc::new(RefCell::new(Lengths {
    index: HashMap::new(),
    measured: Vec::new(),
    pending: Vec::new(),
    seek: None,
  }));
  // Set whenever `measured` changes, and whenever a row is measured.
  let remeasure = cx.signal(());
  let measured = cx.signal(());
  // How often the items changed.
  let changes = cx.signal(0_u64);
  let all = cx.signal(Vec::<T>::new());
  {
    let (key, lengths) = (Rc::clone(&key), Rc::clone(&lengths));
    cx.effect(move || {
      let list = items();
      let keys: Vec<K> = list.iter().map(|item| (key.borrow_mut())(item)).collect();
      let mut lengths = lengths.borrow_mut();
      let old = std::mem::take(&mut lengths.measured);
      let mut index = HashMap::with_capacity(keys.len());
      let mut measured = Vec::with_capacity(keys.len());
      for (at, item_key) in keys.into_iter().enumerate() {
        measured.push(lengths.index.get(&item_key).and_then(|&was| old[was]));
        index.entry(item_key).or_insert(at);
      }
      lengths.index = index;
      lengths.measured = measured;
      // Measured against the old indexes.
      lengths.pending.clear();
      drop(lengths);
      all.set(list);
      changes.update(|n| *n += 1);
      remeasure.set(());
    });
  }
  // Turned, every length measured is the other way.
  {
    let lengths = Rc::clone(&lengths);
    let mut was = None;
    cx.effect(move || {
      let now = horizontal.get();
      if was.replace(now).is_some_and(|was| was != now) {
        lengths.borrow_mut().measured.fill(None);
        remeasure.set(());
      }
    });
  }
  let count = cx.memo(move || all.with(Vec::len));
  let starts = {
    let lengths = Rc::clone(&lengths);
    cx.memo(move || {
      remeasure.get();
      starts(&lengths.borrow().measured, estimate())
    })
  };
  let total = move || starts.with(|starts| starts.last().copied().unwrap_or(0.0));
  // The offset as the scroll view shows it: past either end, that end.
  let shown = move || {
    let most = (total() - inner.get()).max(0.0);
    let offset = offset.get();
    if offset.is_nan() {
      0.0
    } else {
      offset.clamp(0.0, most)
    }
  };
  // The rows in view, and one more each side so a short scroll shows no
  // gap before the next layout.
  let range = cx.memo(move || {
    let top = shown();
    let bottom = top + inner.get();
    starts.with(|starts| {
      let rows = starts.len() - 1;
      let first = starts
        .partition_point(|&start| start <= top)
        .saturating_sub(2);
      let last = (starts.partition_point(|&start| start < bottom) + 1).min(rows);
      first..last.max(first)
    })
  });
  // Rows measured: a row that starts above the view grows or shrinks
  // upward, so the rows shown stay where they are.
  let shifts = Rc::new(Cell::new(0.0));
  {
    let lengths = Rc::clone(&lengths);
    let shifts = Rc::clone(&shifts);
    cx.effect(move || {
      measured.get();
      let (old, top, estimate) = untrack(|| (starts.get(), shown(), estimate()));
      let mut lengths = lengths.borrow_mut();
      let mut changed = false;
      let mut shift = 0.0;
      for (at, length) in std::mem::take(&mut lengths.pending) {
        let Some(slot) = lengths.measured.get_mut(at) else {
          continue;
        };
        if *slot == Some(length) {
          continue;
        }
        if old[at] < top {
          shift += length - slot.unwrap_or(estimate);
        }
        *slot = Some(length);
        changed = true;
      }
      let seek = lengths
        .seek
        .filter(|&at| lengths.measured.get(at).is_some_and(Option::is_some));
      if seek.is_some() {
        lengths.seek = None;
      }
      drop(lengths);
      if !changed {
        return;
      }
      remeasure.set(());
      if shift != 0.0 {
        // A fling goes on from the moved offset.
        shifts.set(shifts.get() + shift);
        offset.set(top + shift);
      }
      if let Some(at) = seek {
        untrack(|| {
          let (start, end) = starts.with(|starts| (starts[at], starts[at + 1]));
          let now = shown();
          let next = keep_in_view(start, end, inner.get(), now);
          if next != now {
            offset.set(next);
          }
        });
      }
    });
  }
  let current = current.map(Prop::shared);
  if let Some(current) = current.clone() {
    let lengths = Rc::clone(&lengths);
    cx.effect(move || {
      let index = current();
      // A change of estimate or height shows the row again.
      row_height.get();
      let viewport = inner.get();
      let Some(index) = index.filter(|&index| index < untrack(|| count.get())) else {
        lengths.borrow_mut().seek = None;
        return;
      };
      let (start, end) = untrack(|| starts.with(|starts| (starts[index], starts[index + 1])));
      let now = untrack(shown);
      let next = keep_in_view(start, end, viewport, now);
      if next != now {
        offset.set(next);
      }
      let mut lengths = lengths.borrow_mut();
      let unmeasured = lengths.measured.get(index).is_some_and(Option::is_none);
      lengths.seek = unmeasured.then_some(index);
    });
  }
  let mut rows = Column::new().style(move || along(horizontal.get(), Style::new(), total()));
  // How wide the rows are, or how tall in a horizontal list, for the
  // highlight.
  let cross = cx.signal(0.0_f32);
  if let (Some(style), Some(current)) = (highlight_style, current) {
    let spot = cx.memo(move || {
      let index = current()?;
      let (start, end) =
        starts.with(|starts| Some((*starts.get(index)?, *starts.get(index + 1)?)))?;
      let target = (index, changes.get());
      let (length, across) = (end - start, cross.get());
      Some(if horizontal.get() {
        Spot {
          at: Point::new(start, 0.0),
          size: Size::new(length, across),
          target,
        }
      } else {
        Spot {
          at: Point::new(0.0, start),
          size: Size::new(across, length),
          target,
        }
      })
    });
    rows = rows.child(highlight(cx, cx.memo(prop_fn(Some(style), None)), spot));
  }
  let rows = rows
    .child(Stack::new().style(move || {
      let first = starts.with(|starts| starts[range.get().start]);
      along(horizontal.get(), Style::new(), first).shrink(0.0)
    }))
    .children_keyed(
      move || {
        let range = range.get();
        all.with(|all| {
          let end = range.end.min(all.len());
          all[range.start.min(end)..end].to_vec()
        })
      },
      {
        let key = Rc::clone(&key);
        move |item| (key.borrow_mut())(item)
      },
      move |cx, item| {
        let own = (key.borrow_mut())(&item.get_untracked());
        let lengths = Rc::clone(&lengths);
        let measure = move |size: Size| {
          let mut lengths = lengths.borrow_mut();
          if let Some(&at) = lengths.index.get(&own) {
            let length = if horizontal.get_untracked() {
              size.width
            } else {
              size.height
            };
            lengths.pending.push((at, length));
            drop(lengths);
            measured.set(());
          }
        };
        let row = Stack::new()
          .style(move || {
            let least = row_height.get() as f32;
            let style = Style::new().shrink(0.0);
            if horizontal.get() {
              style.min_width(least)
            } else {
              style.min_height(least)
            }
          })
          .child(view(cx, item));
        let mut row = Box::new(row).into_element();
        row.common.measure = Some(Rc::new(measure));
        Built(Some(row))
      },
    );
  let mut rows = Box::new(rows).into_element();
  rows.common.measure = Some(Rc::new(move |size: Size| {
    let across = if horizontal.get_untracked() {
      size.height
    } else {
      size.width
    };
    if cross.get_untracked() != across {
      cross.set(across);
    }
  }));
  if let ElementKind::Flex(flex) = &mut rows.kind {
    flex.column_binding = Some((move || !horizontal.get()).into_prop());
  }
  let mut scroll = ScrollView::new(Built(Some(rows)))
    .offset(offset)
    .horizontal(move || horizontal.get());
  scroll.scroll.shifts = Some(shifts);
  if let Some(bar) = scroll_bar {
    scroll = scroll.scroll_bar(bar);
  }
  if let Some(kinetic) = kinetic {
    scroll = scroll.kinetic(kinetic);
  }
  if let Some(overshoot) = overshoot {
    scroll = scroll.overshoot(overshoot);
  }
  scroll = scroll.style(move || {
    let own = style.as_ref().map(|style| style()).unwrap_or_default();
    along(horizontal.get(), own, height.get() as f32)
  });
  Box::new(scroll).into_element()
}

/// `style` with its width, if `horizontal`, or else its height, set to
/// `length`.
fn along(horizontal: bool, style: Style, length: f32) -> Style {
  if horizontal {
    style.width(length)
  } else {
    style.height(length)
  }
}

/// Where a view's highlight goes in its content: the current item's
/// corner and size.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Spot {
  pub(crate) at: Point,
  pub(crate) size: Size,
  /// The item it is on: its index and how often the items changed. A
  /// spot that moves with the same target, as a row is measured, jumps.
  pub(crate) target: (usize, u64),
}

/// A box drawn in `style` at `spot`, behind what follows it. The outer
/// box is moved there by a paint translate over the style's transition,
/// and the inner one takes the size and the style, so a move fades
/// without a layout while a new size comes at once.
pub(crate) fn highlight(cx: &Cx, style: Memo<Option<Style>>, spot: Memo<Option<Spot>>) -> Stack {
  // A new size alone leaves the move as it is.
  let place = cx.memo(move || spot.get().map(|spot| (spot.at, spot.target)));
  let shape = Stack::new().style(move || {
    let own = style.get().unwrap_or_default();
    match spot.get() {
      Some(spot) => own.width(spot.size.width).height(spot.size.height),
      None => own,
    }
  });
  let mut last = None;
  Stack::new()
    .visible(move || style.with(Option::is_some) && spot.with(Option::is_some))
    .style(move || {
      // Hidden, so where doesn't matter, and shown again it jumps.
      let Some((at, target)) = place.get() else {
        return Style::new().left(0.0).top(0.0);
      };
      let moved = last.replace(target) != Some(target);
      let mover = Style::new().left(0.0).top(0.0).translate(at.x, at.y);
      match style.get().and_then(|style| style.transition) {
        Some(transition) if moved => mover.transition(transition),
        _ => mover,
      }
    })
    .child(shape)
}

/// `prop` as a closure, or `default` when unset.
pub(crate) fn prop_fn<T: Clone + 'static>(
  prop: Option<Prop<T>>,
  default: T,
) -> impl FnMut() -> T + 'static {
  let prop = prop.map(Prop::shared);
  move || match prop.as_ref() {
    Some(prop) => prop(),
    None => default.clone(),
  }
}
