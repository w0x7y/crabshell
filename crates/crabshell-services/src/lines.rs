//! Splitting what a socket or a program sends into lines.

use std::fmt::Display;

/// The longest line passed on. The rest of a longer one, up to its
/// newline, is dropped.
pub(crate) const MAX_LINE: usize = 1024 * 1024;

/// Bytes after the last newline, waiting for the rest of their line.
#[derive(Default)]
pub(crate) struct Lines {
  inbox: Vec<u8>,
  /// Dropping the rest of a line longer than [`MAX_LINE`].
  skipping: bool,
}

impl Lines {
  /// The whole lines `bytes` completes, without their `\n`, decoded as
  /// UTF-8 with bad bytes replaced. The rest waits. A line over
  /// [`MAX_LINE`] is dropped and logged as coming from `from`.
  pub(crate) fn push(&mut self, bytes: &[u8], from: &dyn Display) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pieces = bytes.split(|&byte| byte == b'\n').peekable();
    while let Some(piece) = pieces.next() {
      let ended = pieces.peek().is_some();
      if !self.skipping {
        self.inbox.extend_from_slice(piece);
        if self.inbox.len() > MAX_LINE {
          tracing::warn!(%from, "a line over 1 MiB was dropped");
          self.inbox = Vec::new();
          self.skipping = true;
        }
      }
      if ended {
        if !std::mem::take(&mut self.skipping) {
          lines.push(String::from_utf8_lossy(&self.inbox).into_owned());
        }
        self.inbox.clear();
        // A long line's room isn't kept.
        self.inbox.shrink_to(64 * 1024);
      }
    }
    lines
  }

  /// What is left once the sender is done, as a last line, if anything.
  pub(crate) fn finish(&mut self) -> Option<String> {
    let rest = std::mem::take(&mut self.inbox);
    let skipped = std::mem::take(&mut self.skipping);
    (!rest.is_empty() && !skipped).then(|| String::from_utf8_lossy(&rest).into_owned())
  }
}

/// A panic's message, for a log line.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    (*message).to_owned()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    String::from("(panic payload is not a string)")
  }
}
