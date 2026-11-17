//! [`GridView`], a scrolling grid of cells of one size that builds only
//! the rows of cells in view.

use std::cell::Cell;
use std::hash::Hash;
use std::rc::Rc;

use crabshell_reactive::{Memo, Signal, untrack};

use crate::cx::Cx;
use crate::geometry::{Point, Size};
use crate::prop::{IntoProp, Prop};
use crate::scroll::{self, Built, ScrollView, Spot, highlight, keep_in_view, prop_fn};
use crate::style::Style;
use crate::widgets::{
  Column, Common, Element, ElementKind, Row, Stack, Widget, visibility_methods,
};

/// A scrolling grid of cells of one size, in as many columns as fit its
/// width and at least one, for app and wallpaper pickers. It builds only
/// the rows of cells in view, so a grid of thousands costs what a
/// screenful does, and follows a list of items as
/// [`children_keyed`](Column::children_keyed) does: each cell is kept
/// while its item's key stays in the list. When its width changes the
/// number of columns, the cell at the top left stays in view.
///
/// ```
/// use crabshell_ui::{GridView, Icon};
///
/// let apps = GridView::new(
///     || vec!["firefox", "foot", "gimp"],
///     |name| *name,
///     |_cx, name| Icon::new(name.get()).size(48),
/// )
/// .cell_size(72, 72)
/// .height(288);
/// ```
pub struct GridView {
  /// Builds the grid once it has a context for its signals.
  build: Build,
  props: GridViewProps,
  common: Common,
}

/// Builds a grid's element from its properties, with a context.
type Build = Box<dyn FnOnce(&Cx, GridViewProps) -> Element>;

struct GridViewProps {
  cell_width: Option<Prop<u32>>,
  cell_height: Option<Prop<u32>>,
  height: Option<Prop<u32>>,
  current: Option<Prop<Option<usize>>>,
  offset: Option<Signal<f32>>,
  style: Option<Prop<Style>>,
  highlight: Option<Prop<Option<Style>>>,
  scroll_bar: Option<Prop<Option<Style>>>,
  kinetic: Option<Prop<bool>>,
  overshoot: Option<Prop<bool>>,
  mouse_drag: Option<Prop<bool>>,
  horizontal: Option<Prop<bool>>,
}

impl GridView {
  /// The default cell width and height, in logical pixels, as Qt's.
  pub const DEFAULT_CELL: u32 = 100;
  /// The default height of the grid, in logical pixels.
  pub const DEFAULT_HEIGHT: u32 = 240;

  /// A grid with a cell for each of `items`, built by `view` and kept by
  /// `key`, as in [`children_keyed`](Column::children_keyed).
  pub fn new<T, K, W>(
    items: impl FnMut() -> Vec<T> + 'static,
    key: impl FnMut(&T) -> K + 'static,
    view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
  ) -> GridView
  where
    T: Clone + PartialEq + 'static,
    K: Eq + Hash + 'static,
    W: Widget,
  {
    GridView {
      build: Box::new(move |cx, props| build(cx, props, items, key, view)),
      props: GridViewProps {
        cell_width: None,
        cell_height: None,
        height: None,
        current: None,
        offset: None,
        style: None,
        highlight: None,
        scroll_bar: None,
        kinetic: None,
        overshoot: None,
        mouse_drag: None,
        horizontal: None,
      },
      common: Common::default(),
    }
  }

  /// Fills each column top to bottom, then the next, and scrolls
  /// sideways, as Qt's `GridView.FlowTopToBottom`. [`height`](Self::height)
  /// then means the grid's width, its height comes from its style or its
  /// parent, and a column holds as many cells as fit it, at least one.
  /// [`current`](Self::current), the highlight and the offset go along
  /// the columns. Off by default.
  pub fn horizontal(mut self, horizontal: impl IntoProp<bool>) -> GridView {
    self.props.horizontal = Some(horizontal.into_prop());
    self
  }

  /// Sets every cell's width and height in logical pixels, 100 by
  /// default. A cell's content is laid out in that box; a size of 0
  /// counts as 1.
  pub fn cell_size(mut self, width: impl IntoProp<u32>, height: impl IntoProp<u32>) -> GridView {
    self.props.cell_width = Some(width.into_prop());
    self.props.cell_height = Some(height.into_prop());
    self
  }

  /// Sets the grid's height in logical pixels, or its width in a
  /// [`horizontal`](Self::horizontal) grid. The other side comes from its
  /// style or its parent.
  pub fn height(mut self, height: impl IntoProp<u32>) -> GridView {
    self.props.height = Some(height.into_prop());
    self
  }

  /// Keeps the cell at this index in view, scrolling as little as needed
  /// whenever it, the height or the number of columns changes. An index
  /// past the end is scrolled to once the items reach it; `None` scrolls
  /// nothing. While the grid moves, a change scrolls nothing, as in
  /// [`ListView::current`](crate::ListView::current).
  pub fn current(mut self, index: impl IntoProp<Option<usize>>) -> GridView {
    self.props.current = Some(index.into_prop());
    self
  }

  /// Draws a box in this style behind the [`current`](Self::current)
  /// cell, as [`ListView::highlight`](crate::ListView::highlight) does.
  pub fn highlight(mut self, style: impl IntoProp<Option<Style>>) -> GridView {
    self.props.highlight = Some(style.into_prop());
    self
  }

  /// Shares the offset, as [`ScrollView::offset`] does.
  pub fn offset(mut self, offset: Signal<f32>) -> GridView {
    self.props.offset = Some(offset);
    self
  }

  /// Keeps the grid moving after a fling, as [`ScrollView::kinetic`]
  /// does. On by default.
  pub fn kinetic(mut self, kinetic: impl IntoProp<bool>) -> GridView {
    self.props.kinetic = Some(kinetic.into_prop());
    self
  }

  /// Lets a mouse drag the grid's cells, as [`ScrollView::mouse_drag`]
  /// does. On by default.
  pub fn mouse_drag(mut self, mouse_drag: impl IntoProp<bool>) -> GridView {
    self.props.mouse_drag = Some(mouse_drag.into_prop());
    self
  }

  /// Lets the grid go past its ends, as [`ScrollView::overshoot`] does.
  /// On by default.
  pub fn overshoot(mut self, overshoot: impl IntoProp<bool>) -> GridView {
    self.props.overshoot = Some(overshoot.into_prop());
    self
  }

  /// Draws a scroll bar, as [`ScrollView::scroll_bar`] does.
  pub fn scroll_bar(mut self, style: impl IntoProp<Option<Style>>) -> GridView {
    self.props.scroll_bar = Some(style.into_prop());
    self
  }

  /// Sets the grid's style.
  pub fn style(mut self, style: impl IntoProp<Style>) -> GridView {
    self.props.style = Some(style.into_prop());
    self
  }

  visibility_methods!();
}

impl Widget for GridView {
  fn into_element(self: Box<Self>) -> Element {
    let GridView {
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
    element.name = "GridView";
    element.common = common;
    element
  }
}

fn build<T, K, W>(
  cx: &Cx,
  props: GridViewProps,
  mut items: impl FnMut() -> Vec<T> + 'static,
  key: impl FnMut(&T) -> K + 'static,
  mut view: impl FnMut(&Cx, Memo<T>) -> W + 'static,
) -> Element
where
  T: Clone + PartialEq + 'static,
  K: Eq + Hash + 'static,
  W: Widget,
{
  let GridViewProps {
    cell_width,
    cell_height,
    height,
    current,
    offset,
    style,
    highlight: highlight_style,
    scroll_bar,
    kinetic,
    overshoot,
    mouse_drag,
    horizontal,
  } = props;
  let offset = offset.unwrap_or_else(|| cx.signal(0.0));
  let cell_width = cx.memo(prop_fn(cell_width, GridView::DEFAULT_CELL));
  let cell_height = cx.memo(prop_fn(cell_height, GridView::DEFAULT_CELL));
  let cell = move || {
    Size::new(
      cell_width.get().max(1) as f32,
      cell_height.get().max(1) as f32,
    )
  };
  let horizontal = cx.memo(prop_fn(horizontal, false));
  // A cell's length along the way the grid scrolls, and across it.
  let length = move || along(horizontal.get(), cell());
  let breadth = move || along(!horizontal.get(), cell());
  let height = cx.memo(prop_fn(height, GridView::DEFAULT_HEIGHT));
  let style = style.map(Prop::shared);
  // What the grid's padding and border take, across and down.
  let sides = {
    let style = style.clone();
    cx.memo(move || {
      let own = style.as_ref().map(|style| style()).unwrap_or_default();
      let padding = own.padding.unwrap_or_default();
      let border = 2.0 * own.border.map_or(0.0, |border| border.width.max(0.0));
      Size::new(
        (padding.left + padding.right) as f32 + border,
        (padding.top + padding.bottom) as f32 + border,
      )
    })
  };
  // The length cells show in: the grid's, less its padding and border.
  let inner =
    cx.memo(move || (height.get() as f32 - along(horizontal.get(), sides.get())).max(0.0));
  // The grid's own size, which its parent or style sets and its cells
  // don't, 0 until the first layout.
  let outer = cx.signal(Size::default());
  // The room across the grid that cells fit in.
  let width = cx.memo(move || {
    let across = !horizontal.get();
    (along(across, outer.get()) - along(across, sides.get())).max(0.0)
  });
  // Cells across: the columns, or the rows of a horizontal grid. Then
  // `rows` are the lines of cells along it.
  let columns = cx.memo(move || ((width.get() / breadth()) as usize).max(1));
  let all = cx.signal(Vec::<T>::new());
  // How often the items changed, for the highlight.
  let changes = cx.signal(0_u64);
  cx.effect(move || {
    all.set(items());
    changes.update(|n| *n += 1);
  });
  let count = cx.memo(move || all.with(Vec::len));
  let rows = move || count.get().div_ceil(columns.get());
  // The offset as the scroll view shows it: past either end, that end.
  let shown = move || {
    let most = (rows() as f32 * length() - inner.get()).max(0.0);
    let offset = offset.get();
    if offset.is_nan() {
      0.0
    } else {
      offset.clamp(0.0, most)
    }
  };
  // The rows of cells in view, and one more each side.
  let range = cx.memo(move || {
    let row = length();
    let top = shown();
    let first = ((top / row).floor() as usize).saturating_sub(1);
    let last = (((top + inner.get()) / row).ceil() as usize + 1).min(rows());
    first..last.max(first)
  });
  // A new number of columns keeps the cell at the top left in view, and
  // then the current one. The current one is shown too when it, the
  // height or the cells' height changes, or when the items first reach
  // it, but not for a width that keeps the columns.
  let moving = Rc::new(Cell::new(false));
  let current = current.map(Prop::shared);
  {
    let current = current.clone();
    let moving = Rc::clone(&moving);
    let mut was = None;
    let mut asked = None;
    cx.effect(move || {
      let columns = columns.get();
      let index = current.as_ref().and_then(|current| current());
      let viewport = inner.get();
      let row = length();
      let laid_out = width.get() > 0.0;
      let count = count.get();
      untrack(|| {
        let now = shown();
        let mut next = now;
        let raw = offset.get();
        let regrid = was.is_some_and(|was| was != columns);
        if let Some(was) = was.filter(|_| regrid) {
          // Where the old columns showed it, before the new ones clamp it.
          let most = (count.div_ceil(was) as f32 * row - viewport).max(0.0);
          let before = if raw.is_nan() {
            0.0
          } else {
            raw.clamp(0.0, most)
          };
          let top = (before / row).floor();
          let first = top as usize * was;
          next = (first / columns) as f32 * row + (before - top * row);
        }
        // An offset set before the first layout is the config's own.
        if laid_out {
          was = Some(columns);
        }
        let wanted = index.filter(|&index| index < count);
        let ask = (index, viewport, row);
        let fresh = asked.replace((ask, wanted.is_some())) != Some((ask, true));
        // A view in motion goes on, as Qt's does.
        if let Some(index) = wanted.filter(|_| (regrid || fresh) && !moving.get()) {
          let start = (index / columns) as f32 * row;
          next = keep_in_view(start, start + row, viewport, next);
        }
        // A regrid also puts right an offset past the new end.
        if next != now || (regrid && next != raw) {
          offset.set(next);
        }
      });
    });
  }
  let mut content = Column::new()
    .style(move || scroll::along(horizontal.get(), Style::new(), rows() as f32 * length()));
  if let (Some(style), Some(current)) = (highlight_style, current) {
    let spot = cx.memo(move || {
      let index = current()?;
      if index >= count.get() {
        return None;
      }
      let (columns, size) = (columns.get(), cell());
      let (line, place) = ((index / columns) as f32, (index % columns) as f32);
      Some(Spot {
        at: if horizontal.get() {
          Point::new(line * size.width, place * size.height)
        } else {
          Point::new(place * size.width, line * size.height)
        },
        size,
        target: (index, changes.get()),
      })
    });
    content = content.child(highlight(cx, cx.memo(prop_fn(Some(style), None)), spot));
  }
  let mut cells = Box::new(
    Row::new()
      .wrap(true)
      .style(move || {
        let across = columns.get() as f32 * breadth();
        scroll::along(!horizontal.get(), Style::new(), across).shrink(0.0)
      })
      .children_keyed(
        move || {
          let (range, columns) = (range.get(), columns.get());
          all.with(|all| {
            let end = (range.end * columns).min(all.len());
            all[(range.start * columns).min(end)..end].to_vec()
          })
        },
        key,
        move |cx, item| {
          Stack::new()
            .style(move || {
              let size = cell();
              Style::new()
                .width(size.width)
                .height(size.height)
                .shrink(0.0)
            })
            .child(view(cx, item))
        },
      ),
  )
  .into_element();
  // A horizontal grid's cells fill a column, then wrap to the next.
  if let ElementKind::Flex(flex) = &mut cells.kind {
    flex.column_binding = Some((move || horizontal.get()).into_prop());
  }
  let content = content
    .child(Stack::new().style(move || {
      let start = range.get().start as f32 * length();
      scroll::along(horizontal.get(), Style::new(), start).shrink(0.0)
    }))
    .child(Built(Some(cells)));
  let mut content = Box::new(content).into_element();
  if let ElementKind::Flex(flex) = &mut content.kind {
    flex.column_binding = Some((move || !horizontal.get()).into_prop());
  }
  let mut scroll = ScrollView::new(Built(Some(content)))
    .offset(offset)
    .horizontal(move || horizontal.get());
  scroll.scroll.moving = Some(moving);
  if let Some(bar) = scroll_bar {
    scroll = scroll.scroll_bar(bar);
  }
  if let Some(kinetic) = kinetic {
    scroll = scroll.kinetic(kinetic);
  }
  if let Some(overshoot) = overshoot {
    scroll = scroll.overshoot(overshoot);
  }
  if let Some(mouse_drag) = mouse_drag {
    scroll = scroll.mouse_drag(mouse_drag);
  }
  scroll = scroll.style(move || {
    let own = style.as_ref().map(|style| style()).unwrap_or_default();
    // As CSS's `min-width: 0`: the cells don't hold the grid wide.
    let own = match (horizontal.get(), own.min_width, own.min_height) {
      (false, None, _) => own.min_width(0.0),
      (true, _, None) => own.min_height(0.0),
      _ => own,
    };
    scroll::along(horizontal.get(), own, height.get() as f32)
  });
  let mut scroll = Box::new(scroll).into_element();
  // Measured outside the cells, so their row can't hold the grid wide.
  scroll.common.measure = Some(Rc::new(move |size: Size| {
    // Run with no effect running; a grid gone by then is left alone.
    if outer.try_get() != Some(size) {
      outer.try_set(size);
    }
  }));
  scroll
}

/// `size`'s height, or its width if `horizontal`: its length along a
/// grid that scrolls that way.
fn along(horizontal: bool, size: Size) -> f32 {
  if horizontal { size.width } else { size.height }
}
