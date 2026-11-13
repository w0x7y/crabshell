//! The windows the platform holds, panels and the popups they opened, and
//! the output each panel's surface was created on.
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
  /// For a popup: the window that opened it, which always comes earlier
  /// in the list.
  pub(crate) opener: Option<WindowId>,
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
    self.push(window, None)
  }

  /// Adds a popup `opener` opened. Only one chain of popups is open at a
  /// time, as xdg-shell's grabs need, so every popup that isn't the
  /// opener or one of its openers goes first. `None`, and the popup
  /// dropped, if the opener isn't held.
  pub(crate) fn add_popup(&mut self, opener: WindowId, popup: Window) -> Option<WindowId> {
    self.get(opener)?;
    let mut chain = vec![opener];
    while let Some(next) = chain.last().and_then(|&id| self.get(id)?.opener) {
      chain.push(next);
    }
    self.remove_where(|entry| entry.opener.is_some() && !chain.contains(&entry.id));
    Some(self.push(popup, Some(opener)))
  }

  fn push(&mut self, window: Window, opener: Option<WindowId>) -> WindowId {
    let id = WindowId(self.next_id);
    self.next_id += 1;
    self.entries.push(Entry {
      id,
      window,
      opener,
      surface: None,
      output: None,
      refused: false,
      waiting: false,
    });
    id
  }

  /// Drops a window, with its owner, its surface and the popups it
  /// opened. Returns whether it was there.
  pub(crate) fn remove(&mut self, id: WindowId) -> bool {
    self.remove_where(|entry| entry.id == id) > 0
  }

  /// Drops the windows `doomed` picks and every popup they opened, the
  /// last added first: xdg-shell allows destroying only the topmost popup.
  /// Returns how many went.
  pub(crate) fn remove_where(&mut self, doomed: impl Fn(&Entry<S, O>) -> bool) -> usize {
    let mut ids = Vec::new();
    // An opener comes before its popups, so one pass finds them all.
    for entry in &self.entries {
      if doomed(entry) || entry.opener.is_some_and(|opener| ids.contains(&opener)) {
        ids.push(entry.id);
      }
    }
    for id in ids.iter().rev() {
      self.entries.retain(|entry| entry.id != *id);
    }
    ids.len()
  }

  /// Drops the popups `id` opened, and theirs, keeping `id`.
  pub(crate) fn remove_popups_of(&mut self, id: WindowId) -> usize {
    self.remove_where(|entry| entry.opener == Some(id))
  }

  pub(crate) fn get(&self, id: WindowId) -> Option<&Entry<S, O>> {
    self.entries.iter().find(|entry| entry.id == id)
  }

  /// Drops the panels of an output that went away, with their popups:
  /// those whose surface was created on `output`, and those without a
  /// surface that are for an output called `name`. A panel on another
  /// output of the same name, such as one plugged back in, stays. Returns
  /// how many windows were dropped.
  pub(crate) fn drop_output(&mut self, output: &O, name: &str) -> usize {
    self.remove_where(|entry| {
      entry.opener.is_none()
        && match &entry.output {
          Some(on) => on == output,
          None => entry.window.output().name() == name,
        }
    })
  }
}
