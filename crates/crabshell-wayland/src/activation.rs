//! Activation tokens through `xdg-activation-v1`, for the programs the
//! shell starts, so the compositor may focus their windows.

use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::client::{
  Connection, Dispatch, Proxy, QueueHandle, delegate_noop,
};
use wayland_protocols::xdg::activation::v1::client::{
  xdg_activation_token_v1::{self, XdgActivationTokenV1},
  xdg_activation_v1::XdgActivationV1,
};

use crate::platform::Platform;

/// What a requested token is handed to.
type Done = Box<dyn FnOnce(String)>;

/// The compositor's activation global and the requests waiting for their
/// token.
pub(crate) struct Activation {
  manager: Option<XdgActivationV1>,
  /// What each token object, by the number it holds, hands its token to.
  waiting: Vec<(u64, Done)>,
  next: u64,
}

impl Activation {
  pub(crate) fn new(manager: Option<XdgActivationV1>) -> Activation {
    Activation {
      manager,
      waiting: Vec::new(),
      next: 0,
    }
  }

  pub(crate) fn offers(&self) -> bool {
    self.manager.is_some()
  }
}

impl Platform {
  /// Whether the compositor hands out activation tokens.
  pub fn offers_activation(&self) -> bool {
    self.activation.offers()
  }

  /// Asks the compositor for an activation token for a program about to
  /// start, with `app_id` if given, and the seat and serial of the last
  /// button press, touch or key press on one of the windows and that
  /// window's surface, as the compositor wants for focusing it, if that
  /// input came no more than 10 s ago. `done`
  /// gets the token. `false`, and `done` is dropped, without
  /// `xdg-activation-v1`.
  pub fn request_activation_token(
    &mut self,
    app_id: Option<&str>,
    done: impl FnOnce(String) + 'static,
  ) -> bool {
    let Some(manager) = &self.activation.manager else {
      return false;
    };
    let number = self.activation.next;
    self.activation.next += 1;
    let token = manager.get_activation_token(&self.qh, number);
    if let Some((seat, serial, window)) = recent(self.last_input.as_ref(), Instant::now())
      && seat.is_alive()
    {
      token.set_serial(*serial, seat);
      let surface = self
        .windows
        .get(*window)
        .and_then(|entry| entry.surface.as_ref());
      if let Some(surface) = surface {
        token.set_surface(surface.wl_surface());
      }
    }
    if let Some(app_id) = app_id {
      token.set_app_id(app_id.to_owned());
    }
    token.commit();
    self.activation.waiting.push((number, Box::new(done)));
    true
  }
}

/// How long after the input a token is asked for its serial and surface
/// still go with it. A launch an input caused asks within a moment, or
/// a little later after a config's animation or confirmation; an older
/// one more likely came from a timer or IPC, and its serial may have left
/// the ring of recent serials wlroots checks, which would cost the token
/// its workspace.
pub(crate) const INPUT_AGE: Duration = Duration::from_secs(10);

/// What `input` holds, if it came no longer than [`INPUT_AGE`] before
/// `now`.
fn recent<T>(input: Option<&(T, Instant)>, now: Instant) -> Option<&T> {
  let (input, at) = input?;
  (now.saturating_duration_since(*at) <= INPUT_AGE).then_some(input)
}

delegate_noop!(Platform: XdgActivationV1);

impl Dispatch<XdgActivationTokenV1, u64> for Platform {
  fn event(
    platform: &mut Platform,
    proxy: &XdgActivationTokenV1,
    event: xdg_activation_token_v1::Event,
    number: &u64,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    let xdg_activation_token_v1::Event::Done { token } = event else {
      return;
    };
    proxy.destroy();
    let waiting = &mut platform.activation.waiting;
    if let Some(index) = waiting.iter().position(|(ours, _)| ours == number) {
      let (_, done) = waiting.swap_remove(index);
      done(token);
    }
  }
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use super::{INPUT_AGE, recent};

  #[test]
  fn an_input_older_than_the_age_names_no_token() {
    let at = Instant::now();
    let input = Some((7_u32, at));
    assert_eq!(recent(input.as_ref(), at), Some(&7));
    assert_eq!(recent(input.as_ref(), at + INPUT_AGE), Some(&7));
    let late = at + INPUT_AGE + Duration::from_millis(1);
    assert_eq!(recent(input.as_ref(), late), None);
    assert_eq!(recent::<u32>(None, at), None);
  }
}
