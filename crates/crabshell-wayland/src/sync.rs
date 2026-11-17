//! `wl_display.sync` without blocking: a ticket the compositor answers
//! after every event it sent before the request, so a caller learns, on
//! the loop, when it has heard everything up to now.

use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, QueueHandle};

use crate::platform::Platform;

/// A sync's ticket, answered once the compositor's `done` arrives.
pub(crate) struct Ticket(u64);

impl Platform {
  /// Asks the compositor to answer after every event it has sent so far,
  /// such as the descriptions of the outputs it announced. Returns a
  /// ticket for [`synced`](Self::synced). The answer wakes the loop.
  pub fn sync(&mut self) -> u64 {
    self.syncs_sent += 1;
    let ticket = self.syncs_sent;
    self.connection.display().sync(&self.qh, Ticket(ticket));
    ticket
  }

  /// Whether the compositor answered sync `ticket`: every event it sent
  /// before that request has been handled.
  pub fn synced(&self, ticket: u64) -> bool {
    ticket <= self.syncs_done
  }
}

impl Dispatch<WlCallback, Ticket> for Platform {
  fn event(
    platform: &mut Platform,
    _: &WlCallback,
    event: wl_callback::Event,
    ticket: &Ticket,
    _: &Connection,
    _: &QueueHandle<Platform>,
  ) {
    // The compositor answers in the order asked.
    if let wl_callback::Event::Done { .. } = event {
      platform.syncs_done = platform.syncs_done.max(ticket.0);
    }
  }
}
