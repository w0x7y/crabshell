//! Unit tests: per_output's bookkeeping and the shell's settings, without
//! a compositor.

mod colors;
mod control;
mod logging;
mod per_output;
mod persist;
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

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
  use std::sync::{Arc, Mutex};

  #[derive(Clone, Default)]
  struct Logs(Arc<Mutex<Vec<u8>>>);

  impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
      self.0.lock().expect("log lock").extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = logs.0.lock().expect("log lock").clone();
  String::from_utf8_lossy(&bytes).into_owned()
}
