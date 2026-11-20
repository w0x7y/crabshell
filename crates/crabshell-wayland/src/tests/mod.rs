//! Unit tests: everything that can be checked without a compositor.

mod buffers;
mod events;
mod ext_toplevels;
mod focus_grab;
mod foreign_toplevels;
mod global_shortcuts;
mod hyprland_surface;
mod keyboard;
mod lock;
mod outputs;
mod pacing;
mod platform;
mod pointer;
mod popups;
mod scale;
mod surface;
mod text_input;
mod toplevels;
mod windows;
mod windowsets;

use std::io;
use std::sync::{Arc, Mutex};

use crabshell_ui::{Output, PanelWindow};

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
  #[derive(Clone, Default)]
  struct Logs(Arc<Mutex<Vec<u8>>>);

  impl io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
      self
        .0
        .lock()
        .expect("log buffer lock")
        .extend_from_slice(bytes);
      Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
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
  let bytes = logs.0.lock().expect("log buffer lock").clone();
  String::from_utf8(bytes).expect("log output is UTF-8")
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

/// A panel for output `name`.
fn panel(name: &str) -> PanelWindow {
  PanelWindow::new(&Output::new(name))
}
