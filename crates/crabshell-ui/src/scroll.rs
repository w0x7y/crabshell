//! [`ScrollView`], a box that shows part of a taller child.

use crabshell_reactive::Signal;

use crate::prop::{IntoProp, Prop};
use crate::style::Style;
use crate::widgets::{Element, ElementChild, ElementKind, Widget};

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
  style: Option<Prop<Style>>,
  child: Box<dyn Widget>,
}

impl ScrollView {
  /// A scroll view of `child`.
  pub fn new(child: impl Widget) -> ScrollView {
    ScrollView {
      scroll: ScrollProps { offset: None },
      style: None,
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
    self.style = Some(style.into_prop());
    self
  }
}

impl Widget for ScrollView {
  fn into_element(self: Box<Self>) -> Element {
    let ScrollView {
      scroll,
      style,
      child,
    } = *self;
    Element {
      name: "ScrollView",
      style,
      kind: ElementKind::Scroll(scroll),
      children: vec![ElementChild::One(Box::new(child.into_element()))],
    }
  }
}
