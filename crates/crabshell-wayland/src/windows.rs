//! The windows the platform holds, and the output each one's surface was
//! created on.
//!
//! It is generic over the surface and output types so the bookkeeping can
//! be tested with fakes: the real ones need a compositor.

use crabshell_ui::Window;

/// A window the platform holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowId(pub(crate) u64);

/// A window and, once shown, its surface and the output it is on.
pub(crate) struct Entry<S, O> {
  pub(crate) id: WindowId,
  pub(crate) window: Window,
  pub(crate) surface: Option<S>,
  /// The output the surface was created on; set with `surface`.
  pub(crate) output: Option<O>,
  /// The window's settings were refused, and that was logged.
  pub(crate) refused: bool,
  /// Waiting for an output of its name was logged.
  pub(crate) waiting: bool,
}

/// The windows, in the order they were added.
pub(crate) struct Windows<S, O> {
  pub(crate) entries: Vec<Entry<S, O>>,
  next_id: u64,
}

impl<S, O: PartialEq> Windows<S, O> {
  pub(crate) fn new() -> Windows<S, O> {
    Windows {
      entries: Vec::new(),
      next_id: 0,
    }
  }

  pub(crate) fn add(&mut self, window: Window) -> WindowId {
    let id = WindowId(self.next_id);
    self.next_id += 1;
    self.entries.push(Entry {
      id,
      window,
      surface: None,
      output: None,
      refused: false,
      waiting: false,
    });
    id
  }

  /// Drops a window, with its owner and surface. Returns whether it was
  /// there.
  pub(crate) fn remove(&mut self, id: WindowId) -> bool {
    let before = self.entries.len();
    self.entries.retain(|entry| entry.id != id);
    self.entries.len() < before
  }

  pub(crate) fn get(&self, id: WindowId) -> Option<&Entry<S, O>> {
    self.entries.iter().find(|entry| entry.id == id)
  }

  /// Drops the windows of an output that went away: those whose surface
  /// was created on `output`, and those without a surface that are for an
  /// output called `name`. A window on another output of the same name,
  /// such as one plugged back in, stays. Returns how many were dropped.
  pub(crate) fn drop_output(&mut self, output: &O, name: &str) -> usize {
    let before = self.entries.len();
    self.entries.retain(|entry| match &entry.output {
      Some(on) => on != output,
      None => entry.window.output().name() != name,
    });
    before - self.entries.len()
  }
}
