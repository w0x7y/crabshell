//! The services map a context carries: shared by every context made from
//! it, keyed by type.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;

use crabshell_reactive::Runtime;

use super::Harness;
use crate::{Button, Cx, Point, PointerButton, Row, Text};

/// Stands in for a service such as the clock or the Sway connection.
struct Counter(Cell<u32>);

/// Another service type, to show they are kept apart.
struct Name(&'static str);

#[test]
fn a_service_reaches_every_child_context() {
  let root = Cx::new(Runtime::current().new_owner());
  assert!(root.service::<Counter>().is_none(), "nothing provided yet");
  let child = root.child();
  root.provide(Counter(Cell::new(1)));
  // Children made before or after the service was provided see it.
  let grandchild = child.child();
  for cx in [&root, &child, &grandchild] {
    let counter = cx.service::<Counter>().expect("provided at the root");
    counter.0.set(counter.0.get() + 1);
  }
  assert_eq!(
    root.service::<Counter>().map(|counter| counter.0.get()),
    Some(4)
  );
  assert!(root.service::<Name>().is_none(), "keyed by type");
}

#[test]
fn providing_a_type_again_replaces_it() {
  let root = Cx::new(Runtime::current().new_owner());
  let first = root.provide(Name("first"));
  root.child().provide(Name("second"));
  assert_eq!(root.service::<Name>().map(|name| name.0), Some("second"));
  // Whoever kept the first still has it.
  assert_eq!(first.0, "first");
}

#[test]
fn a_buttons_handler_reaches_the_services() {
  let mut h = Harness::new();
  h.cx.provide(Counter(Cell::new(0)));
  let seen = Rc::new(Cell::new(0));
  let saw = Rc::clone(&seen);
  let button = Button::new(Text::new("+")).on_click(move |cx| {
    if let Some(counter) = cx.service::<Counter>() {
      counter.0.set(counter.0.get() + 1);
      saw.set(counter.0.get());
    }
  });
  let window = h.window(h.panel().child(Row::new().child(button)), 200.0, 30.0);
  window.pointer_motion(Point::new(4.0, 15.0));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(seen.get(), 1);
}

/// A service that holds a context, as Phase 3 services will, and looks
/// at the services when it is dropped (here: logs the context).
struct Holder(Cx);

impl Drop for Holder {
  fn drop(&mut self) {
    tracing::debug!("dropping a service of {:?}", self.0);
  }
}

#[test]
fn a_replaced_service_is_dropped_after_the_map_is_released() {
  let root = Cx::new(Runtime::current().new_owner());
  root.provide(Holder(root.child()));
  // Replacing it drops the old one, the map's only reference.
  let replaced = panic::catch_unwind(AssertUnwindSafe(|| {
    // Debug-format the context so the subscriber-free test still runs
    // the borrow (tracing skips formatting with no subscriber).
    struct Peek(Cx);
    impl Drop for Peek {
      fn drop(&mut self) {
        let _ = format!("{:?}", self.0);
      }
    }
    root.provide(Peek(root.child()));
    root.provide(Peek(root.child()));
  }));
  assert!(
    replaced.is_ok(),
    "provide panicked while dropping the old service"
  );
}
