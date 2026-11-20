//! The prelude gives a config what its own widgets need.

use crabshell_reactive::Runtime;

use crate::prelude::*;

/// A config's own widget that takes a value or a closure, as the toolkit's do.
fn label(text: impl IntoProp<String>) -> Text {
  Text::new(text).max_lines(1)
}

#[test]
fn a_config_helper_takes_a_value_or_a_closure_through_the_prelude() {
  let cx = Cx::new(Runtime::current().new_owner());
  let count = cx.signal(1);
  let _fixed = label("fixed");
  let _bound = label(move || format!("{} new", count.get()));
}

/// A shake a config keeps in a `const`, as the lock screen's.
const SHAKE: [Step<f32>; 2] = [
  Step::to(-14.0, Transition::new(70).easing(Easing::OutQuad)),
  Step::to(0.0, Transition::new(95)),
];

#[test]
fn a_config_keeps_a_sequence_in_a_const_through_the_prelude() {
  let cx = Cx::new(Runtime::current().new_owner());
  let shakes = cx.signal(0_u32);
  let shake = cx.sequence(0.0, move || shakes.get(), SHAKE);
  assert_eq!(shake.value().get(), 0.0);
}
