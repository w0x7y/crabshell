//! Unit tests: per_output's bookkeeping and the shell's settings, without
//! a compositor.

mod logging;
mod per_output;
mod switch_log;

use std::panic::Location;

use crabshell_ui::Cx;

use crate::per_output::{Build, PerOutput};

/// Registers a builder as `cx.per_output` does, at the caller's line, with
/// nothing to do when its context goes.
trait Register {
  fn add(&mut self, cx: Cx, build: Build) -> usize;
}

impl<Id> Register for PerOutput<Id> {
  #[track_caller]
  fn add(&mut self, cx: Cx, build: Build) -> usize {
    self.register(cx, build, Location::caller(), Box::new(|| {}))
  }
}
