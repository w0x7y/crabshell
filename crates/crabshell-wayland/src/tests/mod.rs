//! Unit tests: everything that can be checked without a compositor.

mod buffers;
mod events;
mod ext_toplevels;
mod foreign_toplevels;
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

  let logs = Logs::default();
  let writer = logs.clone();
  let subscriber = tracing_subscriber::fmt()
    .with_writer(move || writer.clone())
    .finish();
  tracing::subscriber::with_default(subscriber, f);
  let bytes = logs.0.lock().expect("log buffer lock").clone();
  String::from_utf8(bytes).expect("log output is UTF-8")
}

/// A panel for output `name`.
fn panel(name: &str) -> PanelWindow {
  PanelWindow::new(&Output::new(name))
}
