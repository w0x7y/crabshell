//! Keyboards: which window each seat's keyboard is in, and turning sctk's
//! key events into [`crabshell_ui::KeyEvent`]s for it.
//!
//! sctk reads the compositor's keymap with libxkbcommon and repeats a held
//! key with a calloop timer that exists only while the key is down, so an
//! idle keyboard wakes nothing.

use crabshell_ui::{Key, KeyEvent, Modifiers};
use smithay_client_toolkit::reexports::client::protocol::{
  wl_keyboard::WlKeyboard, wl_seat::WlSeat, wl_surface::WlSurface,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::seat::keyboard::{self, KeyboardHandler, Keysym, RawModifiers};

use crate::platform::Platform;
use crate::windows::WindowId;

/// A seat's keyboard.
pub(crate) struct SeatKeyboard {
  pub(crate) seat: WlSeat,
  pub(crate) keyboard: WlKeyboard,
  /// The window it is in.
  pub(crate) focus: Option<WindowId>,
  pub(crate) modifiers: Modifiers,
}

/// The ui's event for sctk's `event`.
pub(crate) fn key_event(
  event: &keyboard::KeyEvent,
  modifiers: Modifiers,
  repeat: bool,
) -> KeyEvent {
  KeyEvent {
    key: key(event.keysym),
    // Enter, Backspace and Ctrl combinations "type" control characters.
    text: event
      .utf8
      .clone()
      .filter(|text| !text.is_empty() && !text.chars().any(char::is_control)),
    modifiers,
    repeat,
  }
}

/// The ui's name for `keysym`.
pub(crate) fn key(keysym: Keysym) -> Key {
  match keysym {
    Keysym::Return | Keysym::KP_Enter => Key::Enter,
    Keysym::Escape => Key::Escape,
    Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
    Keysym::BackSpace => Key::Backspace,
    Keysym::Delete | Keysym::KP_Delete => Key::Delete,
    Keysym::Left | Keysym::KP_Left => Key::Left,
    Keysym::Right | Keysym::KP_Right => Key::Right,
    Keysym::Up | Keysym::KP_Up => Key::Up,
    Keysym::Down | Keysym::KP_Down => Key::Down,
    Keysym::Home | Keysym::KP_Home => Key::Home,
    Keysym::End | Keysym::KP_End => Key::End,
    Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
    Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
    _ => {
      let raw = keysym.raw();
      if (Keysym::F1.raw()..=Keysym::F35.raw()).contains(&raw) {
        // At most 35, so it fits.
        Key::Function((raw - Keysym::F1.raw() + 1) as u8)
      } else {
        match keysym.key_char() {
          Some(c) if !c.is_control() => Key::Character(c),
          _ => Key::Other(raw),
        }
      }
    }
  }
}

impl Platform {
  /// Gives `event` to the window `keyboard` is in.
  pub(crate) fn key(&mut self, keyboard: &WlKeyboard, event: &keyboard::KeyEvent, repeat: bool) {
    let Some(ours) = self
      .keyboards
      .iter()
      .find(|ours| ours.keyboard == *keyboard)
    else {
      return;
    };
    let event = key_event(event, ours.modifiers, repeat);
    if let Some(entry) = ours.focus.and_then(|id| self.windows.get(id)) {
      entry.window.key(&event);
    }
  }

  /// Takes `keyboard` out of the window it was in.
  pub(crate) fn keyboard_leave(&mut self, index: usize) {
    let Some(id) = self.keyboards[index].focus.take() else {
      return;
    };
    if let Some(entry) = self.windows.get(id) {
      entry.window.keyboard_leave();
    }
  }

  /// Lets go of a seat's keyboard, leaving the window it was in.
  pub(crate) fn drop_keyboard(&mut self, index: usize) {
    self.keyboard_leave(index);
    let ours = self.keyboards.remove(index);
    // `release` came in version 3; older keyboards just stop being used.
    if ours.keyboard.version() >= 3 {
      ours.keyboard.release();
    }
  }
}

impl KeyboardHandler for Platform {
  fn enter(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    keyboard: &WlKeyboard,
    surface: &WlSurface,
    _: u32,
    _: &[u32],
    _: &[Keysym],
  ) {
    let Some(index) = self
      .keyboards
      .iter()
      .position(|ours| ours.keyboard == *keyboard)
    else {
      return;
    };
    self.keyboard_leave(index);
    let Some(entry) = self.windows.entries.iter().find(|entry| {
      entry
        .surface
        .as_ref()
        .is_some_and(|ours| ours.wl_surface() == surface)
    }) else {
      return;
    };
    entry.window.keyboard_enter();
    self.keyboards[index].focus = Some(entry.id);
  }

  fn leave(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    keyboard: &WlKeyboard,
    _: &WlSurface,
    _: u32,
  ) {
    if let Some(index) = self
      .keyboards
      .iter()
      .position(|ours| ours.keyboard == *keyboard)
    {
      self.keyboard_leave(index);
    }
  }

  fn press_key(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    keyboard: &WlKeyboard,
    _: u32,
    event: keyboard::KeyEvent,
  ) {
    self.key(keyboard, &event, false);
  }

  /// Only for compositors that repeat keys themselves (`wl_keyboard`
  /// version 10); others are repeated by sctk's timer.
  fn repeat_key(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    keyboard: &WlKeyboard,
    _: u32,
    event: keyboard::KeyEvent,
  ) {
    self.key(keyboard, &event, true);
  }

  fn release_key(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    _: &WlKeyboard,
    _: u32,
    _: keyboard::KeyEvent,
  ) {
  }

  fn update_modifiers(
    &mut self,
    _: &Connection,
    _: &QueueHandle<Self>,
    keyboard: &WlKeyboard,
    _: u32,
    modifiers: keyboard::Modifiers,
    _: RawModifiers,
    _: u32,
  ) {
    if let Some(ours) = self
      .keyboards
      .iter_mut()
      .find(|ours| ours.keyboard == *keyboard)
    {
      ours.modifiers = Modifiers {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        logo: modifiers.logo,
      };
    }
  }
}
