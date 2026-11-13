//! Input methods through `zwp_text_input_v3`: each seat's text input
//! follows the keyboard into a window, is enabled while that window has a
//! focused text field, and tells the input method the field's text and
//! caret. What the input method composes and commits goes back into the
//! field. Nothing here wakes the loop by itself.

use crabshell_ui::{FieldState, Preedit, Window};
use smithay_client_toolkit::reexports::client::protocol::{wl_seat::WlSeat, wl_surface::WlSurface};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
  self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::platform::Platform;
use crate::windows::WindowId;

/// The most surrounding text the protocol takes, in bytes.
const MAX_SURROUNDING: usize = 4000;

/// A seat's text input.
pub(crate) struct SeatTextInput {
  pub(crate) seat: WlSeat,
  pub(crate) input: ZwpTextInputV3,
  /// The window the compositor said it entered.
  focus: Option<WindowId>,
  /// What the input method was last told, while enabled.
  sent: Option<FieldState>,
  /// What arrived since the last `done`.
  pending: Pending,
  /// `InputMethod` from a `done` until the next sync, which sends any
  /// change it made as the input method's.
  cause: ChangeCause,
}

#[derive(Default)]
struct Pending {
  preedit: Option<Preedit>,
  commit: Option<String>,
  delete: Option<(u32, u32)>,
}

impl SeatTextInput {
  pub(crate) fn new(seat: WlSeat, input: ZwpTextInputV3) -> SeatTextInput {
    SeatTextInput {
      seat,
      input,
      focus: None,
      sent: None,
      pending: Pending::default(),
      cause: ChangeCause::Other,
    }
  }

  /// Enables, updates or disables the text input for `window`, the one
  /// it is in, after a layout. Sends only what changed.
  fn sync(&mut self, window: Option<&Window>) {
    let state = window.and_then(Window::field);
    let after_done = std::mem::replace(&mut self.cause, ChangeCause::Other);
    if state == self.sent {
      return;
    }
    match &state {
      Some(state) => {
        // Again for each field the focus moves to, as the protocol asks.
        if self
          .sent
          .as_ref()
          .is_none_or(|sent| sent.field != state.field)
        {
          self.input.enable();
          self
            .input
            .set_content_type(ContentHint::None, ContentPurpose::Normal);
        }
        let (text, cursor, anchor) = surrounding(state);
        self
          .input
          .set_surrounding_text(text.to_owned(), cursor, anchor);
        self.input.set_text_change_cause(after_done);
        let caret = state.caret;
        let [x, y, width, height] =
          [caret.x, caret.y, caret.width, caret.height].map(|value| value.round() as i32);
        self.input.set_cursor_rectangle(x, y, width.max(1), height);
      }
      None => self.input.disable(),
    }
    self.input.commit();
    self.sent = state;
  }
}

/// At most [`MAX_SURROUNDING`] bytes of the field's text around its caret,
/// on character boundaries, with the caret inside it and the anchor
/// clamped to it.
pub(crate) fn surrounding(state: &FieldState) -> (&str, i32, i32) {
  let text = &state.text;
  let mut start = state.cursor.saturating_sub(MAX_SURROUNDING / 2);
  while !text.is_char_boundary(start) {
    start -= 1;
  }
  let mut end = (start + MAX_SURROUNDING).min(text.len());
  while !text.is_char_boundary(end) {
    end -= 1;
  }
  let at = |index: usize| i32::try_from(index.clamp(start, end) - start).unwrap_or(0);
  (&text[start..end], at(state.cursor), at(state.anchor))
}

impl Platform {
  /// Gives every seat a text input, once the compositor offers them.
  pub(crate) fn add_text_input(&mut self, qh: &QueueHandle<Platform>, seat: &WlSeat) {
    let Some(manager) = &self.text_inputs_manager else {
      return;
    };
    if self.text_inputs.iter().any(|ours| ours.seat == *seat) {
      return;
    }
    let input = manager.get_text_input(seat, qh, ());
    self
      .text_inputs
      .push(SeatTextInput::new(seat.clone(), input));
  }

  /// Destroys a seat's text input, as the seat goes.
  pub(crate) fn drop_text_input(&mut self, seat: &WlSeat) {
    self.text_inputs.retain(|ours| {
      if ours.seat == *seat {
        ours.input.destroy();
        false
      } else {
        true
      }
    });
  }

  /// Tells each input method about the field in the window it is in.
  pub(crate) fn sync_text_inputs(&mut self) {
    let Platform {
      text_inputs,
      windows,
      ..
    } = self;
    for ours in text_inputs {
      let window = ours
        .focus
        .and_then(|id| windows.get(id))
        .map(|entry| &entry.window);
      ours.sync(window);
    }
  }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for Platform {
  fn event(
    _: &mut Platform,
    _: &ZwpTextInputManagerV3,
    _: <ZwpTextInputManagerV3 as Proxy>::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
  }
}

impl Dispatch<ZwpTextInputV3, ()> for Platform {
  fn event(
    platform: &mut Platform,
    input: &ZwpTextInputV3,
    event: zwp_text_input_v3::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let Some(index) = platform
      .text_inputs
      .iter()
      .position(|ours| ours.input == *input)
    else {
      return;
    };
    match event {
      zwp_text_input_v3::Event::Enter { surface } => {
        let id = platform.window_of(&surface);
        let ours = &mut platform.text_inputs[index];
        ours.focus = id;
        ours.sent = None;
        ours.pending = Pending::default();
      }
      zwp_text_input_v3::Event::Leave { .. } => {
        let ours = &mut platform.text_inputs[index];
        if ours.sent.take().is_some() {
          ours.input.disable();
          ours.input.commit();
        }
        if let Some(entry) = ours.focus.take().and_then(|id| platform.windows.get(id)) {
          entry.window.set_preedit(None);
        }
      }
      zwp_text_input_v3::Event::PreeditString {
        text,
        cursor_begin,
        cursor_end,
      } => {
        let cursor = usize::try_from(cursor_begin)
          .ok()
          .zip(usize::try_from(cursor_end).ok());
        platform.text_inputs[index].pending.preedit = text.map(|text| {
          // A cursor that isn't on a character boundary isn't shown.
          let cursor = cursor.filter(|&(start, end)| {
            start <= end && text.is_char_boundary(start) && text.is_char_boundary(end)
          });
          Preedit { text, cursor }
        });
      }
      zwp_text_input_v3::Event::CommitString { text } => {
        platform.text_inputs[index].pending.commit = text;
      }
      zwp_text_input_v3::Event::DeleteSurroundingText {
        before_length,
        after_length,
      } => {
        platform.text_inputs[index].pending.delete = Some((before_length, after_length));
      }
      // Applied whatever the serial: an old one only means the input
      // method hadn't seen our latest state.
      zwp_text_input_v3::Event::Done { .. } => {
        let ours = &mut platform.text_inputs[index];
        let pending = std::mem::take(&mut ours.pending);
        // What this changes, the next sync sends as the input method's.
        ours.cause = ChangeCause::InputMethod;
        let Some(entry) = ours.focus.and_then(|id| platform.windows.get(id)) else {
          return;
        };
        let window = &entry.window;
        window.set_preedit(None);
        if let Some((before, after)) = pending.delete {
          window.delete_around(before as usize, after as usize);
        }
        if let Some(text) = pending.commit {
          window.commit_text(&text);
        }
        window.set_preedit(pending.preedit);
      }
      _ => {}
    }
  }
}

impl Platform {
  /// The window whose surface is `surface`.
  fn window_of(&self, surface: &WlSurface) -> Option<WindowId> {
    self
      .windows
      .entries
      .iter()
      .find(|entry| {
        entry
          .surface
          .as_ref()
          .is_some_and(|ours| ours.wl_surface() == surface)
      })
      .map(|entry| entry.id)
  }
}
