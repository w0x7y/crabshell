//! [`Slider`], a handle dragged along a track, made of a [`Button`] and
//! boxes that share the track's free space by `grow`, so the handle sits
//! at its fraction of the way with no size worked out by hand.

use std::cell::Cell;
use std::rc::Rc;

use crabshell_reactive::{Memo, Signal, untrack};

use crate::cx::Cx;
use crate::input::{Drag, DragPhase, PIXELS_PER_STEP, Wheel};
use crate::keyboard::{Key, KeyEvent};
use crate::prop::{IntoProp, Prop};
use crate::scroll::Built;
use crate::style::Style;
use crate::widgets::{Align, Button, Column, Common, Element, Row, Stack, Widget};

/// A shared style property, or the empty style when unset.
type StyleFn = Rc<dyn Fn() -> Style>;

/// A value picked by dragging a handle along a track, as Qt's `Slider`.
/// The slider writes the value back to its signal, and setting the signal
/// moves the handle.
///
/// A press anywhere on the slider moves the value there and drags it from
/// there. The wheel moves it one step a notch, or a twentieth of the range
/// when it has no step, and a touchpad in proportion; so do the arrow keys
/// while it has the keyboard focus, which Tab gives it. The value written
/// is always within the range and on a step, and only a change is
/// written. A value set from outside the range shows at that end, and NaN
/// at the start.
///
/// It draws nothing until styled: `style` sets the track's box, 120 by 4
/// logical pixels unless it sets a size, `fill_style` the part from the
/// start to the handle, and `handle_style` the handle, 16 by 16 unless it
/// sets a size. The slider is as large as the track, or the handle where
/// that is larger.
///
/// ```
/// use crabshell_ui::{Color, Cx, Slider, Style};
///
/// const TRACK: Style = Style::new().width(200.0).height(4.0).radius(2.0)
///     .background(Color::rgb(0x45, 0x47, 0x5a));
/// const FILL: Style = Style::new().radius(2.0).background(Color::rgb(0x89, 0xb4, 0xfa));
/// const HANDLE: Style = Style::new().radius(8.0).background(Color::rgb(0xcd, 0xd6, 0xf4));
///
/// fn volume(cx: &Cx) -> Slider {
///     let volume = cx.signal(50.0);
///     Slider::new(volume).range(0.0, 100.0).step(5.0)
///         .style(TRACK).fill_style(FILL).handle_style(HANDLE)
/// }
/// ```
pub struct Slider {
  props: SliderProps,
  common: Common,
}

struct SliderProps {
  value: Signal<f32>,
  min: Option<Prop<f32>>,
  max: Option<Prop<f32>>,
  step: f32,
  vertical: Option<Prop<bool>>,
  enabled: Option<Prop<bool>>,
  style: Option<Prop<Style>>,
  fill_style: Option<Prop<Style>>,
  handle_style: Option<Prop<Style>>,
  dragging: Option<Signal<bool>>,
}

/// The track's size when its style sets none, along and across.
const TRACK: (f32, f32) = (120.0, 4.0);

/// The handle's size when its style sets none.
const HANDLE: f32 = 16.0;

/// The parts of the range the wheel and the keys move a slider without a
/// step.
const PARTS: f32 = 20.0;

impl Slider {
  /// A slider showing `value` and writing it back as it is dragged.
  pub fn new(value: Signal<f32>) -> Slider {
    Slider {
      props: SliderProps {
        value,
        min: None,
        max: None,
        step: 0.0,
        vertical: None,
        enabled: None,
        style: None,
        fill_style: None,
        handle_style: None,
        dragging: None,
      },
      common: Common::default(),
    }
  }

  /// Sets the values at the start and the end, 0 and 1 by default. A range
  /// whose end isn't above its start holds only the start.
  pub fn range(mut self, min: impl IntoProp<f32>, max: impl IntoProp<f32>) -> Slider {
    self.props.min = Some(min.into_prop());
    self.props.max = Some(max.into_prop());
    self
  }

  /// Snaps the value to whole steps from the start of the range. 0, the
  /// default, or a step that isn't a number above 0, slides freely.
  pub fn step(mut self, step: f32) -> Slider {
    self.props.step = step;
    self
  }

  /// Makes the slider run from the bottom up instead of left to right.
  pub fn vertical(mut self, vertical: impl IntoProp<bool>) -> Slider {
    self.props.vertical = Some(vertical.into_prop());
    self
  }

  /// Turns the slider on or off, as [`Button::enabled`]: off, nothing
  /// moves it and Tab skips it.
  pub fn enabled(mut self, enabled: impl IntoProp<bool>) -> Slider {
    self.props.enabled = Some(enabled.into_prop());
    self
  }

  /// Sets the track's style, its size included.
  pub fn style(mut self, style: impl IntoProp<Style>) -> Slider {
    self.props.style = Some(style.into_prop());
    self
  }

  /// Sets the style of the part of the track from the start to the
  /// handle's middle. Its size comes from the value.
  pub fn fill_style(mut self, style: impl IntoProp<Style>) -> Slider {
    self.props.fill_style = Some(style.into_prop());
    self
  }

  /// Sets the handle's style, its width and height included.
  pub fn handle_style(mut self, style: impl IntoProp<Style>) -> Slider {
    self.props.handle_style = Some(style.into_prop());
    self
  }

  /// Shares whether the slider is being dragged: `true` from the press
  /// until the release, as Qt's `pressed`. Written only on a change.
  pub fn dragging(mut self, dragging: Signal<bool>) -> Slider {
    self.props.dragging = Some(dragging);
    self
  }

  crate::widgets::visibility_methods!();
}

impl Widget for Slider {
  fn into_element(self: Box<Self>) -> Element {
    let Slider { props, common } = *self;
    let mut once = Some(props);
    // A keyed list of one gives the slider a context for its memos.
    let mut element = Box::new(Stack::new().children_keyed(
      || vec![()],
      |_| (),
      move |cx, _| {
        Built(
          once
            .take()
            .map(|props| Box::new(build(cx, props)).into_element()),
        )
      },
    ))
    .into_element();
    element.name = "Slider";
    element.common = common;
    element
  }
}

/// Where a slider is and how it moves, shared by its handlers.
#[derive(Clone, Copy)]
struct Range {
  value: Signal<f32>,
  min: Memo<f32>,
  max: Memo<f32>,
  step: f32,
}

impl Range {
  /// The start and end, the end never below the start. A bound that
  /// isn't a number is 0.
  fn bounds(self) -> (f32, f32) {
    let number = |n: f32| if n.is_nan() { 0.0 } else { n };
    let min = number(self.min.get());
    (min, number(self.max.get()).max(min))
  }

  /// How far along the value is, from 0 to 1.
  fn fraction(self) -> f32 {
    let (min, max) = self.bounds();
    let value = self.value.get();
    if max > min && !value.is_nan() {
      ((value - min) / (max - min)).clamp(0.0, 1.0)
    } else {
      0.0
    }
  }

  /// One step of the wheel or the keys.
  fn unit(self) -> f32 {
    let (min, max) = self.bounds();
    if self.snaps() {
      self.step
    } else {
      (max - min) / PARTS
    }
  }

  fn snaps(self) -> bool {
    self.step.is_finite() && self.step > 0.0
  }

  /// The value read now, NaN as the start, outside any binding.
  fn now(self) -> f32 {
    untrack(|| {
      let (min, _) = self.bounds();
      self.value.try_get().filter(|v| !v.is_nan()).unwrap_or(min)
    })
  }

  /// Writes `value`, within the range and on a step, if that changes it.
  fn write(self, value: f32) {
    untrack(|| {
      let (min, max) = self.bounds();
      let mut value = if value.is_nan() { min } else { value };
      if self.snaps() {
        value = min + ((value - min) / self.step).round() * self.step;
      }
      let value = value.max(min).min(max);
      if self.value.try_get() != Some(value) {
        self.value.try_set(value);
      }
    });
  }

  /// Writes the value `t` of the way along.
  fn write_fraction(self, t: f32) {
    let (min, max) = untrack(|| self.bounds());
    self.write(min + t.clamp(0.0, 1.0) * (max - min));
  }
}

fn build(cx: &Cx, props: SliderProps) -> Button {
  let SliderProps {
    value,
    min,
    max,
    step,
    vertical,
    enabled,
    style,
    fill_style,
    handle_style,
    dragging,
  } = props;
  let range = Range {
    value,
    min: cx.memo(shared_or(min, 0.0)),
    max: cx.memo(shared_or(max, 1.0)),
    step,
  };
  let vertical = cx.memo(shared_or(vertical, false));
  let style = shared_style(style);
  let fill_style = shared_style(fill_style);
  let handle = {
    let handle_style = shared_style(handle_style);
    cx.memo(move || {
      Style::new()
        .width(HANDLE)
        .height(HANDLE)
        .merge(handle_style())
        .shrink(0.0)
    })
  };
  let fraction = cx.memo(move || range.fraction());

  let on_drag = move |_: &Cx, drag: Drag| {
    if let Some(dragging) = dragging {
      let held = drag.phase != DragPhase::End;
      if untrack(|| dragging.try_get()) != Some(held) {
        dragging.try_set(held);
      }
    }
    if drag.phase == DragPhase::End {
      return;
    }
    let vertical = untrack(|| vertical.get());
    let handle = untrack(|| handle.get());
    let (at, length, size) = if vertical {
      (drag.position.y, drag.size.height, handle.height)
    } else {
      (drag.position.x, drag.size.width, handle.width)
    };
    let size = size.unwrap_or(HANDLE).max(0.0);
    // The handle's middle follows the pointer.
    let travel = length - size;
    let t = if travel > 0.0 {
      (at - size / 2.0) / travel
    } else if length > 0.0 {
      at / length
    } else {
      0.0
    };
    range.write_fraction(if vertical { 1.0 - t } else { t });
  };
  // Whole steps of a wheel or touchpad that haven't added up yet.
  let partial = Rc::new(Cell::new(0.0_f32));
  let on_wheel = move |_: &Cx, wheel: Wheel| {
    // Up and right move it up.
    let notches = wheel.x.notches - wheel.y.notches;
    let steps = if notches != 0.0 {
      notches
    } else {
      (wheel.x.pixels - wheel.y.pixels) / PIXELS_PER_STEP
    };
    if !steps.is_finite() {
      return;
    }
    let unit = untrack(|| range.unit());
    if range.snaps() {
      let total = partial.get() + steps;
      let whole = total.trunc();
      partial.set(total - whole);
      if whole != 0.0 {
        range.write(range.now() + whole * unit);
      }
    } else if steps != 0.0 {
      range.write(range.now() + steps * unit);
    }
    if wheel.x.stop || wheel.y.stop {
      partial.set(0.0);
    }
  };
  let on_key = move |_: &Cx, event: &KeyEvent| {
    let steps = match event.key {
      Key::Left | Key::Down => -1.0,
      Key::Right | Key::Up => 1.0,
      _ => return false,
    };
    range.write(range.now() + steps * untrack(|| range.unit()));
    true
  };

  let body = |column: bool| {
    let parts = Parts {
      style: Rc::clone(&style),
      fill_style: Rc::clone(&fill_style),
      handle,
      fraction,
      column,
    };
    parts.area().visible(move || vertical.get() == column)
  };
  let mut button = Button::new(Stack::new().child(body(false)).child(body(true)))
    .on_drag(on_drag)
    .on_wheel(on_wheel)
    .on_key(on_key);
  if let Some(enabled) = enabled {
    button = button.enabled(enabled);
  }
  button
}

/// What one orientation of a slider is drawn from.
struct Parts {
  style: StyleFn,
  fill_style: StyleFn,
  handle: Memo<Style>,
  fraction: Memo<f32>,
  /// Bottom to top, rather than left to right.
  column: bool,
}

impl Parts {
  /// The track with its fill, and over it the handle, in one cell.
  fn area(self) -> Stack {
    let Parts {
      style,
      fill_style,
      handle,
      fraction,
      column,
    } = self;
    let (along, across) = TRACK;
    let size = if column {
      Style::new().width(across).height(along)
    } else {
      Style::new().width(along).height(across)
    };
    let half = move || {
      let handle = handle.get();
      let size = if column { handle.height } else { handle.width };
      size.unwrap_or(HANDLE).max(0.0) / 2.0
    };
    // The free space past both halves of the handle goes before and after
    // it by the fraction, as it does around the handle below.
    let fill = Stack::new().style(move || fill_style().basis(half()).grow(fraction.get()));
    let rest = Stack::new().style(move || Style::new().basis(half()).grow(1.0 - fraction.get()));
    let track_style = move || {
      size
        .align_self(Align::Center)
        .justify_self(Align::Center)
        .merge(style())
    };
    let before = Stack::new().style(move || Style::new().basis(0.0).grow(fraction.get()));
    let after = Stack::new().style(move || Style::new().basis(0.0).grow(1.0 - fraction.get()));
    let knob = Stack::new().style(move || handle.get());
    if column {
      Stack::new()
        .child(
          Column::new()
            .reverse(true)
            .style(track_style)
            .child(fill)
            .child(rest),
        )
        .child(
          Column::new()
            .reverse(true)
            .align(Align::Center)
            .child(before)
            .child(knob)
            .child(after),
        )
    } else {
      Stack::new()
        .child(Row::new().style(track_style).child(fill).child(rest))
        .child(
          Row::new()
            .align(Align::Center)
            .child(before)
            .child(knob)
            .child(after),
        )
    }
  }
}

/// `prop` as a closure for a memo, or `default` when unset.
fn shared_or<T: Clone + 'static>(prop: Option<Prop<T>>, default: T) -> impl FnMut() -> T + 'static {
  let prop = prop.map(Prop::shared);
  move || prop.as_ref().map_or_else(|| default.clone(), |prop| prop())
}

fn shared_style(prop: Option<Prop<Style>>) -> StyleFn {
  match prop {
    Some(prop) => prop.shared(),
    None => Rc::new(Style::new),
  }
}
