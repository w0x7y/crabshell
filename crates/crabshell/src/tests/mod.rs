//! Unit tests: per_output's bookkeeping and the shell's settings, without
//! a compositor.

mod clipboard;
mod colors;
mod control;
mod icon_theme;
mod ipc;
mod logging;
mod per_output;
mod persist;
mod prelude;
mod startup;
mod switch_log;
#[cfg(target_env = "gnu")]
mod tuned;
mod window_manager;

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

  static GLOBAL: std::sync::Once = std::sync::Once::new();
  GLOBAL.call_once(|| {
    let _ = tracing::subscriber::set_global_default(Undecided);
  });
  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = logs.0.lock().expect("log lock").clone();
  String::from_utf8_lossy(&bytes).into_owned()
}

/// The global subscriber while tests capture logs: it takes nothing, and
/// answers every callsite with "sometimes". tracing caches each callsite's
/// interest, and while it counts one subscriber it asks the thread that
/// first hits the callsite, so a thread capturing nothing could otherwise
/// turn a log line off for another thread's capture.
struct Undecided;

impl tracing::Subscriber for Undecided {
  fn register_callsite(
    &self,
    _: &'static tracing::Metadata<'static>,
  ) -> tracing::subscriber::Interest {
    tracing::subscriber::Interest::sometimes()
  }

  fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
    Some(tracing::level_filters::LevelFilter::TRACE)
  }

  fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
    false
  }

  fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }

  fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

  fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

  fn event(&self, _: &tracing::Event<'_>) {}

  fn enter(&self, _: &tracing::span::Id) {}

  fn exit(&self, _: &tracing::span::Id) {}
}

/// A test's file or directory, removed when the test ends, a panic
/// included, so test runs leave nothing in the temp dir.
struct Removed(std::path::PathBuf);

impl std::ops::Deref for Removed {
  type Target = std::path::PathBuf;

  fn deref(&self) -> &std::path::PathBuf {
    &self.0
  }
}

impl Drop for Removed {
  fn drop(&mut self) {
    if std::fs::remove_dir_all(&self.0).is_err() {
      let _ = std::fs::remove_file(&self.0);
    }
  }
}
