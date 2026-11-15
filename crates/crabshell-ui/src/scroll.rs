//! [`ScrollView`], a box that shows part of a taller child, and
//! [`ListView`], a scrolling list that builds only the rows in view.

use std::hash::Hash;

use crabshell_reactive::{Memo, Signal, untrack};

use crate::cx::Cx;
use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::widgets::{
  Column, Common, Element, ElementChild, ElementKind, Stack, Widget, visibility_methods,
};

/// Logical pixels a mouse wheel scrolls per notch.
pub(crate) const PIXELS_PER_NOTCH: f32 = 48.0;

pub(crate) struct ScrollProps {
  pub(crate) offset: Option<Signal<f32>>,
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
      scroll: ScrollProps { offset: None },
      common: Common::default(),
      child: Box::new(child),
    }
  }

  /// Shares the offset: how many logical pixels of the child are
  /// scrolled out of view above. Scrolling writes it, and setting it
  /// scrolls. Values past either end show that end.
  pub fn offset(mut self, offset: Signal<f32>) -> ScrollView {
    self.scroll.offset = Some(offset);
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
      kind: ElementKind::Scroll(scroll),
      children: vec![ElementChild::One(Box::new(child.into_element()))],
    }
  }
}

/// A scrolling list of rows of one height that builds only the rows in
/// view, so a list of thousands costs what a screenful does. Offsets are
/// `f32`, which stays exact to about 16 million pixels: some 700,000 rows
/// of 24 pixels. It follows a
/// list of items as [`children_keyed`](Column::children_keyed) does: each
/// row is kept while its item's key stays in the list.
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
}

impl ListView {
  /// The default row height and list height, in logical pixels.
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
      },
      common: Common::default(),
    }
  }

  /// Sets every row's height in logical pixels.
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
  /// whenever it changes. `None` scrolls nothing.
  pub fn current(mut self, index: impl IntoProp<Option<usize>>) -> ListView {
    self.props.current = Some(index.into_prop());
    self
  }

  /// Shares the offset, as [`ScrollView::offset`] does.
  pub fn offset(mut self, offset: Signal<f32>) -> ListView {
    self.props.offset = Some(offset);
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
struct Built(Option<Element>);

impl Widget for Built {
  fn into_element(self: Box<Self>) -> Element {
    self
      .0
      .unwrap_or_else(|| Box::new(Stack::new()).into_element())
  }
}

fn build<T, K, W>(
  cx: &Cx,
  props: ListProps,
  mut items: impl FnMut() -> Vec<T> + 'static,
  mut key: impl FnMut(&T) -> K + 'static,
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
  } = props;
  let offset = offset.unwrap_or_else(|| cx.signal(0.0));
  let row_height = cx.memo(prop_fn(row_height, ListView::DEFAULT_ROW_HEIGHT));
  let height = cx.memo(prop_fn(height, ListView::DEFAULT_HEIGHT));
  let style = style.map(Prop::shared);
  // The height rows show in: the list's, less its padding and border.
  let inner = {
    let style = style.clone();
    cx.memo(move || {
      let own = style.as_ref().map(|style| style()).unwrap_or_default();
      let padding = own.padding.unwrap_or_default();
      let border = own.border.map_or(0.0, |border| border.width.max(0.0));
      let insets = padding.top as f32 + padding.bottom as f32 + 2.0 * border;
      (height.get() as f32 - insets).max(0.0)
    })
  };
  let all = cx.signal(Vec::<T>::new());
  cx.effect(move || all.set(items()));
  let count = cx.memo(move || all.with(Vec::len));
  // The offset as the scroll view shows it: past either end, that end.
  let shown = move || {
    let total = count.get() as f32 * row_height.get().max(1) as f32;
    let most = (total - inner.get()).max(0.0);
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
    let row = row_height.get().max(1) as f32;
    let top = shown();
    let first = ((top / row).floor() as usize).saturating_sub(1);
    let last = (((top + inner.get()) / row).ceil() as usize + 1).min(count.get());
    first..last.max(first)
  });
  if let Some(current) = current {
    let current = current.shared();
    cx.effect(move || {
      let Some(index) = current() else {
        return;
      };
      let row = row_height.get() as f32;
      let (top, bottom) = (index as f32 * row, (index + 1) as f32 * row);
      let viewport = inner.get();
      let now = untrack(shown);
      let next = if top < now {
        top
      } else if bottom > now + viewport {
        bottom - viewport
      } else {
        now
      };
      if next != now {
        offset.set(next);
      }
    });
  }
  let rows = Column::new()
    .style(move || Style::new().height(count.get() as f32 * row_height.get() as f32))
    .child(
      Stack::new()
        .style(move || Style::new().height(range.get().start as f32 * row_height.get() as f32)),
    )
    .children_keyed(
      move || {
        let range = range.get();
        all.with(|all| {
          let end = range.end.min(all.len());
          all[range.start.min(end)..end].to_vec()
        })
      },
      move |item| key(item),
      move |cx, item| {
        Stack::new()
          .style(move || Style::new().height(row_height.get() as f32))
          .child(view(cx, item))
      },
    );
  let mut scroll = ScrollView::new(rows).offset(offset);
  scroll = scroll.style(move || {
    let own = style.as_ref().map(|style| style()).unwrap_or_default();
    own.height(height.get() as f32)
  });
  Box::new(scroll).into_element()
}

/// `prop` as a closure, or `default` when unset.
fn prop_fn<T: Clone + 'static>(prop: Option<Prop<T>>, default: T) -> impl FnMut() -> T + 'static {
  let prop = prop.map(Prop::shared);
  move || match prop.as_ref() {
    Some(prop) => prop(),
    None => default.clone(),
  }
}
