//! Widget properties that take either a plain value or a closure.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::menu::MenuEntry;
use crate::region::Region;
use crate::style::Style;
use crate::widgets::{Align, Fit, IconSource, Justify};
use crate::window::{Anchors, Edge, KeyboardFocus, Layer, Margins};

/// A widget property: either a value set once, or a binding that reruns
/// whenever a signal it read changes.
///
/// You never build one yourself. Widget methods take `impl IntoProp<T>`, so
/// `.height(30)` sets a value and `.height(move || ...)` makes a binding.
pub struct Prop<T>(pub(crate) PropKind<T>);

impl<T: Clone + 'static> Prop<T> {
  /// The property as a closure that can be called again and shared, as a
  /// widget built from other widgets hands one property to many of them.
  /// A binding stays one: whatever calls it subscribes to the signals it
  /// reads.
  pub fn shared(self) -> Rc<dyn Fn() -> T> {
    match self.0 {
      PropKind::Value(value) => Rc::new(move || value.clone()),
      PropKind::Binding(compute) => {
        let compute = RefCell::new(compute);
        Rc::new(move || (compute.borrow_mut())())
      }
    }
  }
}

pub(crate) enum PropKind<T> {
  Value(T),
  Binding(Box<dyn FnMut() -> T>),
}

/// Anything a widget property accepts: a value of type `T`, or a closure
/// that returns one.
///
/// Plain values work for the property types this crate uses, and a string
/// slice works where a `String` is expected. Any `FnMut() -> T` closure
/// works too: it runs in an effect, so it reruns when a signal it read
/// changes.
pub trait IntoProp<T> {
  /// Wraps the value or closure.
  fn into_prop(self) -> Prop<T>;
}

// A closure is a binding. This impl can sit next to the value impls below
// because Rust knows that no plain type like `u32` implements `FnMut`.
impl<T, F> IntoProp<T> for F
where
  F: FnMut() -> T + 'static,
{
  fn into_prop(self) -> Prop<T> {
    Prop(PropKind::Binding(Box::new(self)))
  }
}

/// Lets each listed type be passed as a plain value.
macro_rules! value_props {
    ($($ty:ty),* $(,)?) => {
        $(
            impl IntoProp<$ty> for $ty {
                fn into_prop(self) -> Prop<$ty> {
                    Prop(PropKind::Value(self))
                }
            }
        )*
    };
}

value_props!(
  bool,
  u32,
  i32,
  String,
  Style,
  Align,
  Justify,
  Anchors,
  Layer,
  Margins,
  KeyboardFocus,
  Option<Region>,
  PathBuf,
  Fit,
  IconSource,
  Vec<MenuEntry>,
);

impl IntoProp<String> for &str {
  fn into_prop(self) -> Prop<String> {
    Prop(PropKind::Value(self.to_owned()))
  }
}

impl IntoProp<Option<Region>> for Region {
  fn into_prop(self) -> Prop<Option<Region>> {
    Prop(PropKind::Value(Some(self)))
  }
}

impl IntoProp<Anchors> for Edge {
  fn into_prop(self) -> Prop<Anchors> {
    Prop(PropKind::Value(Anchors::from(self)))
  }
}

/// An icon name, such as `"audio-volume-high"`.
impl IntoProp<IconSource> for &str {
  fn into_prop(self) -> Prop<IconSource> {
    Prop(PropKind::Value(IconSource::Name(self.to_owned())))
  }
}

/// An icon name, as tray items send them.
impl IntoProp<IconSource> for String {
  fn into_prop(self) -> Prop<IconSource> {
    Prop(PropKind::Value(IconSource::Name(self)))
  }
}

impl IntoProp<PathBuf> for &Path {
  fn into_prop(self) -> Prop<PathBuf> {
    Prop(PropKind::Value(self.to_owned()))
  }
}

impl IntoProp<PathBuf> for &str {
  fn into_prop(self) -> Prop<PathBuf> {
    Prop(PropKind::Value(PathBuf::from(self)))
  }
}

impl IntoProp<PathBuf> for String {
  fn into_prop(self) -> Prop<PathBuf> {
    Prop(PropKind::Value(PathBuf::from(self)))
  }
}

/// An SVG or PNG file.
impl IntoProp<IconSource> for &Path {
  fn into_prop(self) -> Prop<IconSource> {
    Prop(PropKind::Value(IconSource::File(self.to_owned())))
  }
}

/// An SVG or PNG file.
impl IntoProp<IconSource> for PathBuf {
  fn into_prop(self) -> Prop<IconSource> {
    Prop(PropKind::Value(IconSource::File(self)))
  }
}
