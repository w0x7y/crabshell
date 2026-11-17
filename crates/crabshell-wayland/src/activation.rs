//! Activation tokens through `xdg-activation-v1`, for the programs the
//! shell starts, so the compositor may focus their windows.

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
  /// window's surface, as the compositor wants for focusing it. `done`
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
    if let Some((seat, serial, window)) = &self.last_input
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
