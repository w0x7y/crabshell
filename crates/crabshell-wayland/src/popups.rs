//! Popups: taking the ones widgets opened.

use crate::platform::Platform;

impl Platform {
  /// Adds the popups each window's widgets opened since the last call.
  pub(crate) fn take_popups(&mut self) {
    let opened: Vec<_> = self
      .windows
      .entries
      .iter()
      .flat_map(|entry| {
        let id = entry.id;
        entry
          .window
          .take_popups()
          .into_iter()
          .map(move |popup| (id, popup))
      })
      .collect();
    for (opener, popup) in opened {
      self.windows.add_popup(opener, popup);
    }
  }
}
