//! Handing the compositor's own shortcuts to a window while it has the
//! keyboard, through `keyboard-shortcuts-inhibit-unstable-v1`.

use crabshell_reactive::Signal;
use crabshell_ui::Window;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};
use smithay_client_toolkit::seat::SeatState;
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibit_manager_v1::{
  self, ZwpKeyboardShortcutsInhibitManagerV1,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1::{
  self, ZwpKeyboardShortcutsInhibitorV1,
};

use crate::platform::Platform;
use crate::surface::Surface;
use crate::windows::WindowId;

/// The compositor's shortcuts inhibitor manager.
pub(crate) struct ShortcutsInhibit {
  pub(crate) manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
  /// Whether a window asked without the protocol, which is logged once.
  warned: bool,
}

impl ShortcutsInhibit {
  pub(crate) fn new(manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>) -> ShortcutsInhibit {
    ShortcutsInhibit {
      manager,
      warned: false,
    }
  }
}

/// A window's shortcuts inhibitor, destroyed with its surface.
pub(crate) struct ShortcutsInhibitor {
  proxy: ZwpKeyboardShortcutsInhibitorV1,
  /// The window's `shortcuts_inhibited`.
  signal: Option<Signal<bool>>,
  /// Whether the compositor said `active` last.
  active: bool,
}

impl Drop for ShortcutsInhibitor {
  fn drop(&mut self) {
    self.proxy.destroy();
    if self.active
      && let Some(signal) = self.signal
    {
      signal.try_set(false);
    }
  }
}

impl Surface {
  /// Holds a shortcuts inhibitor for `seat` on this surface while
  /// `window`, held as `id`, asks for one, and none otherwise.
  pub(crate) fn inhibit_shortcuts(
    &mut self,
    inhibit: &mut ShortcutsInhibit,
    (id, window): (WindowId, &Window),
    seats: &SeatState,
    qh: &QueueHandle<Platform>,
  ) {
    if !window.inhibits_shortcuts() {
      self.shortcuts_inhibitor = None;
      return;
    }
    if self.shortcuts_inhibitor.is_some() {
      return;
    }
    let Some(manager) = &inhibit.manager else {
      if !std::mem::replace(&mut inhibit.warned, true) {
        tracing::warn!(
          "the compositor lacks keyboard-shortcuts-inhibit-unstable-v1, so it keeps its shortcuts"
        );
      }
      return;
    };
    // shortcut: the first seat only, as idle watches; a seat of its own
    // per window once Crabshell handles several.
    let Some(seat) = seats.seats().next() else {
      return;
    };
    let proxy = manager.inhibit_shortcuts(self.wl_surface(), &seat, qh, id);
    self.shortcuts_inhibitor = Some(ShortcutsInhibitor {
      proxy,
      signal: window.shortcuts_inhibited(),
      active: false,
    });
  }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, WindowId> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &ZwpKeyboardShortcutsInhibitorV1,
    event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
    id: &WindowId,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let active = match event {
      zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => true,
      zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => false,
      _ => return,
    };
    let Some(inhibitor) = platform
      .windows
      .entries
      .iter_mut()
      .find(|entry| entry.id == *id)
      .and_then(|entry| entry.surface.as_mut()?.shortcuts_inhibitor.as_mut())
      .filter(|inhibitor| inhibitor.proxy == *proxy)
    else {
      return;
    };
    inhibitor.active = active;
    if let Some(signal) = inhibitor.signal {
      signal.try_set(active);
    }
  }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitManagerV1, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ZwpKeyboardShortcutsInhibitManagerV1,
    _: zwp_keyboard_shortcuts_inhibit_manager_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}
