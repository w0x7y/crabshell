//! Splitting what a socket or a program sends into lines, or into pieces
//! at another marker, as Quickshell's `SplitParser`.

#[cfg(test)]
mod tests;

use std::fmt::Display;

/// The longest line passed on. The rest of a longer one, up to its
/// marker, is dropped.
pub(crate) const MAX_LINE: usize = 1024 * 1024;

/// What was read and not passed on yet. Lines are taken one at a time,
/// so a marker set between two splits everything after the first.
pub(crate) struct Lines {
  /// What ends a line. Empty passes on each read as it comes.
  marker: Vec<u8>,
  inbox: Vec<u8>,
  /// Where the line being read starts in `inbox`: what is before it was
  /// passed on, and goes at the next append.
  start: usize,
  /// How far into `inbox` no marker starts, from `start` on.
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
      start: 0,
      scanned: 0,
      skipping: false,
    }
  }

  /// Splits at `marker` from now on, what waits included.
  pub(crate) fn set_marker(&mut self, marker: &str) {
    self.marker = marker.as_bytes().to_vec();
    self.scanned = self.start;
  }

  /// Drops what waits, for a new stream, and keeps the marker.
  pub(crate) fn reset(&mut self) {
    self.inbox = Vec::new();
    self.start = 0;
    self.scanned = 0;
    self.skipping = false;
  }

  /// Whether anything waits to be passed on.
  pub(crate) fn waiting(&self) -> bool {
    self.start < self.inbox.len()
  }

  /// Adds what was read, for [`next`](Self::next).
  pub(crate) fn append(&mut self, bytes: &[u8]) {
    self.compact();
    self.inbox.extend_from_slice(bytes);
  }

  /// Drops what was passed on, and a long line's room with it, so a
  /// stream that goes quiet keeps nothing it won't use.
  fn compact(&mut self) {
    if self.start > 0 {
      self.inbox.drain(..self.start);
      self.scanned -= self.start;
      self.start = 0;
      self.inbox.shrink_to(64 * 1024);
    }
  }

  /// The next whole line, without its marker, decoded as UTF-8 with bad
  /// bytes replaced. A line over [`MAX_LINE`] is dropped and logged as
  /// coming from `from`. With an empty marker, all the text waiting,
  /// keeping a character a read cut off.
  pub(crate) fn next(&mut self, from: &dyn Display) -> Option<String> {
    if self.marker.is_empty() {
      self.inbox.drain(..self.start);
      (self.start, self.scanned) = (0, 0);
      let text = decode(&mut self.inbox);
      return (!text.is_empty()).then_some(text);
    }
    let width = self.marker.len();
    while let Some(found) = find(&self.inbox[self.scanned..], &self.marker) {
      let end = self.scanned + found;
      let line = &self.inbox[self.start..end];
      let text = if std::mem::take(&mut self.skipping) {
        // The end of a line dropped and logged already.
        None
      } else if line.len() > MAX_LINE {
        tracing::warn!(%from, "a line over 1 MiB was dropped");
        None
      } else {
        Some(String::from_utf8_lossy(line).into_owned())
      };
      self.start = end + width;
      self.scanned = self.start;
      if text.is_some() {
        return text;
      }
    }
    // A marker a read cut off may start in the last bytes, so only what
    // is before them surely belongs to the line.
    let known = self.inbox.len().saturating_sub(width - 1).max(self.start);
    self.scanned = known;
    if known - self.start > MAX_LINE && !self.skipping {
      tracing::warn!(%from, "a line over 1 MiB was dropped");
      self.skipping = true;
    }
    if self.skipping {
      self.start = known;
    }
    self.compact();
    None
  }

  /// What is left once the sender is done and every whole line was taken,
  /// as a last line, if anything; over [`MAX_LINE`] it is dropped and
  /// logged as coming from `from`. It empties what waits.
  pub(crate) fn rest(&mut self, from: &dyn Display) -> Option<String> {
    let rest = &self.inbox[self.start..];
    let text = if rest.is_empty() || self.skipping {
      None
    } else if rest.len() > MAX_LINE {
      tracing::warn!(%from, "a line over 1 MiB was dropped");
      None
    } else {
      Some(String::from_utf8_lossy(rest).into_owned())
    };
    self.reset();
    text
  }

  /// Adds `bytes` and takes every whole line they complete.
  pub(crate) fn push(&mut self, bytes: &[u8], from: &dyn Display) -> Vec<String> {
    self.append(bytes);
    std::iter::from_fn(|| self.next(from)).collect()
  }

  /// Every whole line still waiting and then the rest, once the sender is
  /// done.
  pub(crate) fn finish(&mut self, from: &dyn Display) -> Vec<String> {
    let mut lines: Vec<String> = std::iter::from_fn(|| self.next(from)).collect();
    lines.extend(self.rest(from));
    lines
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
