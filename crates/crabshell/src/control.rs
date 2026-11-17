//! The supervisor's control socket, named by `CRABSHELL_CONTROL`. The
//! shell says `ready` once each of its windows has drawn a frame. Told
//! `save`, it saves its persisted values and exits, so the supervisor can
//! start the new build.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};

/// The variable that names the supervisor's socket.
pub(crate) const CONTROL: &str = "CRABSHELL_CONTROL";

/// The longest command line read; a longer one is dropped.
const MAX_LINE: usize = 256;

/// The supervisor's socket, connected, if `CRABSHELL_CONTROL` names one.
/// One that can't be reached is logged, and the shell runs on without.
pub(crate) fn connect() -> Option<UnixStream> {
  let path = std::env::var_os(CONTROL).filter(|path| !path.is_empty())?;
  match UnixStream::connect(&path) {
    Ok(stream) => Some(stream),
    Err(error) => {
      tracing::warn!(path = %path.to_string_lossy(), %error, "can't reach the supervisor");
      None
    }
  }
}

/// Whether the shell is ready: every panel it shows has drawn a frame,
/// and it shows at least one, or it shows none and `started` says the
/// startup judgement passed, so there is nothing to wait for. `panels`
/// gives each panel's `(shown, commits)`. One waiting for its output, or
/// whose settings layer-shell would refuse, isn't shown and doesn't count.
pub(crate) fn ready(panels: impl IntoIterator<Item = (bool, u64)>, started: bool) -> bool {
  let mut drawn = false;
  for (shown, commits) in panels {
    if shown {
      if commits == 0 {
        return false;
      }
      drawn = true;
    }
  }
  drawn || started
}

/// Tells the supervisor the shell is ready. A failure is logged.
pub(crate) fn send_ready(stream: &mut UnixStream) {
  if let Err(error) = stream.write_all(b"ready\n") {
    tracing::warn!(%error, "can't tell the supervisor the shell is ready");
  }
}

/// Reads commands from `stream` on the loop: `save` calls `on_save`.
/// Anything else is logged and skipped. When the supervisor hangs up, the
/// shell runs on alone.
pub(crate) fn listen<D: 'static>(
  handle: &LoopHandle<'static, D>,
  stream: UnixStream,
  mut on_save: impl FnMut() + 'static,
) {
  if let Err(error) = stream.set_nonblocking(true) {
    return tracing::warn!(%error, "can't listen to the supervisor");
  }
  let mut line = Vec::new();
  let source = Generic::new(stream, Interest::READ, Mode::Level);
  let inserted = handle.insert_source(source, move |_, stream, _| {
    let mut chunk = [0_u8; 64];
    loop {
      // Through a shared reference: calloop lends the source read-only.
      match (&**stream).read(&mut chunk) {
        Ok(0) => {
          tracing::info!("the supervisor hung up; the shell runs on alone");
          return Ok(PostAction::Remove);
        }
        Ok(read) => {
          for &byte in &chunk[..read] {
            if byte != b'\n' {
              if line.len() < MAX_LINE {
                line.push(byte);
              }
              continue;
            }
            match line.as_slice() {
              b"save" => on_save(),
              other => tracing::warn!(
                command = %String::from_utf8_lossy(other),
                "the supervisor sent something unknown"
              ),
            }
            line.clear();
          }
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => return Ok(PostAction::Continue),
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) => {
          tracing::warn!(%error, "can't read from the supervisor; the shell runs on alone");
          return Ok(PostAction::Remove);
        }
      }
    }
  });
  if let Err(error) = inserted {
    tracing::warn!(error = %error.error, "can't listen to the supervisor");
  }
}
