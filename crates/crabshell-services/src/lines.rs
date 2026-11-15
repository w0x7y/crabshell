//! Splitting what a socket or a program sends into lines, or into pieces
//! at another marker, as Quickshell's `SplitParser`.

#[cfg(test)]
mod tests;

use std::fmt::Display;

/// The longest line passed on. The rest of a longer one, up to its
/// marker, is dropped.
pub(crate) const MAX_LINE: usize = 1024 * 1024;

/// Bytes after the last marker, waiting for the rest of their line.
pub(crate) struct Lines {
  /// What ends a line. Empty passes on each read as it comes.
  marker: Vec<u8>,
  inbox: Vec<u8>,
  /// How much of `inbox` is known to start no marker.
  scanned: usize,
  /// Dropping the rest of a line longer than [`MAX_LINE`].
  skipping: bool,
}

impl Default for Lines {
  fn default() -> Lines {
    Lines::new("\n")
  }
}

impl Lines {
  /// Lines that end at `marker`, or for an empty one, each read.
  pub(crate) fn new(marker: &str) -> Lines {
    Lines {
      marker: marker.as_bytes().to_vec(),
      inbox: Vec::new(),
      scanned: 0,
      skipping: false,
    }
  }

  /// Splits at `marker` from now on, what waits included.
  pub(crate) fn set_marker(&mut self, marker: &str) {
    self.marker = marker.as_bytes().to_vec();
    self.scanned = 0;
  }

  /// Drops what waits, for a new stream, and keeps the marker.
  pub(crate) fn reset(&mut self) {
    self.inbox = Vec::new();
    self.scanned = 0;
    self.skipping = false;
  }

  /// The whole lines `bytes` completes, without their marker, decoded as
  /// UTF-8 with bad bytes replaced. The rest waits. A line over
  /// [`MAX_LINE`] is dropped and logged as coming from `from`. With an
  /// empty marker, the text `bytes` holds, keeping a character it cuts
  /// off for the next read.
  pub(crate) fn push(&mut self, bytes: &[u8], from: &dyn Display) -> Vec<String> {
    self.inbox.extend_from_slice(bytes);
    if self.marker.is_empty() {
      let text = decode(&mut self.inbox);
      return if text.is_empty() { vec![] } else { vec![text] };
    }
    let width = self.marker.len();
    let mut lines = Vec::new();
    let (mut start, mut at) = (0, self.scanned);
    while let Some(found) = find(&self.inbox[at..], &self.marker) {
      let end = at + found;
      if std::mem::take(&mut self.skipping) {
        // The end of a line dropped and logged already.
      } else if end - start > MAX_LINE {
        tracing::warn!(%from, "a line over 1 MiB was dropped");
      } else {
        lines.push(String::from_utf8_lossy(&self.inbox[start..end]).into_owned());
      }
      start = end + width;
      at = start;
    }
    self.inbox.drain(..start);
    if start > 0 {
      // A long line's room isn't kept.
      self.inbox.shrink_to(64 * 1024);
    }
    // A marker the read cut off may start in its last bytes.
    self.scanned = self.inbox.len().saturating_sub(width - 1);
    if self.inbox.len() > MAX_LINE && !self.skipping {
      tracing::warn!(%from, "a line over 1 MiB was dropped");
      self.skipping = true;
    }
    if self.skipping {
      // Only what may start a marker is kept.
      self.inbox.drain(..self.scanned);
      self.inbox.shrink_to(64 * 1024);
      self.scanned = 0;
    }
    lines
  }

  /// What is left once the sender is done, as a last line, if anything.
  pub(crate) fn finish(&mut self) -> Option<String> {
    let rest = std::mem::take(&mut self.inbox);
    let skipped = std::mem::take(&mut self.skipping);
    self.scanned = 0;
    (!rest.is_empty() && !skipped).then(|| String::from_utf8_lossy(&rest).into_owned())
  }
}

/// Where `marker` first starts in `bytes`.
fn find(bytes: &[u8], marker: &[u8]) -> Option<usize> {
  bytes
    .windows(marker.len())
    .position(|window| window == marker)
}

/// The text `bytes` holds, with bad bytes replaced, leaving in `bytes`
/// the start of a character cut off at its end.
pub(crate) fn decode(bytes: &mut Vec<u8>) -> String {
  let mut text = String::new();
  let mut rest: &[u8] = bytes;
  loop {
    match std::str::from_utf8(rest) {
      Ok(valid) => {
        text.push_str(valid);
        rest = &[];
        break;
      }
      Err(error) => {
        let (valid, after) = rest.split_at(error.valid_up_to());
        text.push_str(&String::from_utf8_lossy(valid));
        match error.error_len() {
          Some(bad) => {
            text.push('\u{fffd}');
            rest = &after[bad..];
          }
          None => {
            rest = after;
            break;
          }
        }
      }
    }
  }
  *bytes = rest.to_vec();
  text
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
