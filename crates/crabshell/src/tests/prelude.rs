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
