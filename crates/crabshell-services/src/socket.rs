//! Unix sockets a config opens, as Quickshell's `Socket`: lines in, bytes
//! out.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Once;

use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;

use crate::Shared;

/// The longest line passed on. The rest of a longer one, up to its
/// newline, is dropped.
pub(crate) const MAX_LINE: usize = 1024 * 1024;

/// The most bytes left to write before the socket is closed, so a program
/// that reads nothing can't grow the shell without end.
pub(crate) const MAX_OUTBOX: usize = 4 * 1024 * 1024;

/// What `cx.socket` needs from the shell: the loop.
pub(crate) struct Sockets(pub(crate) Rc<Shared>);

/// A connection to a Unix socket, from `cx.socket(path)`. It connects at
/// once, passes each line it reads to [`on_line`](Self::on_line), and
/// writes what [`write`](Self::write) is given. A clone is cheap and
/// shares the connection.
///
/// It closes when the context that opened it goes. When the other end
/// closes it, [`connected`](Self::connected) turns `false` and it stays
/// closed until [`reconnect`](Self::reconnect): nothing retries on its
/// own, so a socket that isn't there never wakes the shell.
#[derive(Clone)]
pub struct Socket {
  inner: Rc<Inner>,
}

struct Inner {
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  path: PathBuf,
  /// Lives under the opening context's owner, so it says when the socket
  /// must close for good.
  connected: Signal<bool>,
  state: RefCell<State>,
}

/// What a config does with each line.
type OnLine = Rc<dyn Fn(&str)>;

#[derive(Default)]
struct State {
  connection: Option<Connection>,
  on_line: Option<OnLine>,
  /// Where `on_line` was set, for the log if it panics.
  at: Option<&'static Location<'static>>,
  /// Bytes after the last newline.
  inbox: Vec<u8>,
  /// Dropping the rest of a line longer than [`MAX_LINE`].
  skipping: bool,
}

struct Connection {
  stream: UnixStream,
  /// The loop source that waits for something to read.
  reader: RegistrationToken,
  /// The loop source that waits for room to write, while the socket
  /// holds back part of `outbox`.
  writer: Option<RegistrationToken>,
  outbox: Vec<u8>,
}

/// `cx.socket(path)`.
pub(crate) fn socket(cx: &Cx, path: PathBuf) -> Socket {
  let shared = cx.service::<Sockets>().map(|sockets| Rc::clone(&sockets.0));
  if shared.is_none() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.socket() needs the services the shell provides; it never connects");
    });
  }
  let inner = Rc::new(Inner {
    shared,
    path,
    connected: cx.signal(false),
    state: RefCell::default(),
  });
  // Dropped with the opening context's owner, which closes the socket.
  cx.owner().signal(CloseOnDrop(Rc::clone(&inner)));
  inner.connect();
  Socket { inner }
}

/// Closes the socket when the opening context's owner drops it.
struct CloseOnDrop(Rc<Inner>);

impl Drop for CloseOnDrop {
  fn drop(&mut self) {
    self.0.close();
  }
}

impl Socket {
  /// Whether it is connected: `true` from a connect that worked until the
  /// other end closes it, it breaks, or [`close`](Self::close).
  pub fn connected(&self) -> ReadSignal<bool> {
    self.inner.connected.read_only()
  }

  /// Calls `on_line` with each line read, without its `\n`, decoded as
  /// UTF-8 with bad bytes replaced. What is left when the other end closes
  /// counts as a last line. A line over 1 MiB is dropped and logged.
  ///
  /// It runs on the main thread. If it panics, that is logged with the
  /// socket's path and where `on_line` was called, and it isn't called
  /// again; the socket stays open.
  #[track_caller]
  pub fn on_line(self, on_line: impl Fn(&str) + 'static) -> Socket {
    let mut state = self.inner.state.borrow_mut();
    state.on_line = Some(Rc::new(on_line));
    state.at = Some(Location::caller());
    drop(state);
    self
  }

  /// Sends `data`, all of it, as the other end takes it. While not
  /// connected it is dropped, and that is logged. If more than 4 MiB wait
  /// because the other end reads nothing, the socket is closed.
  pub fn write(&self, data: impl AsRef<[u8]>) {
    self.inner.write(data.as_ref());
  }

  /// Closes it and connects again, as after the other end restarted.
  pub fn reconnect(&self) {
    self.inner.close();
    self.inner.connect();
  }

  /// Closes it. [`reconnect`](Self::reconnect) opens it again.
  pub fn close(&self) {
    self.inner.close();
  }
}

impl Inner {
  /// Whether the opening context is still there.
  fn alive(&self) -> bool {
    self.connected.try_with(|_| ()).is_some()
  }

  fn connect(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) else {
      return;
    };
    if !self.alive() {
      return;
    }
    let opened = UnixStream::connect(&self.path).and_then(|stream| {
      stream.set_nonblocking(true)?;
      let fd = stream.as_fd().try_clone_to_owned()?;
      Ok((stream, fd))
    });
    let (stream, fd) = match opened {
      Ok(opened) => opened,
      Err(error) => {
        let path = self.path.display();
        return tracing::warn!(%path, %error, "can't connect to the socket");
      }
    };
    let inner = Rc::clone(self);
    let read = Box::new(move || inner.on_readable());
    let Some(reader) = event_loop.insert_fd(fd, Interest::READ, read) else {
      return;
    };
    let mut state = self.state.borrow_mut();
    state.inbox.clear();
    state.skipping = false;
    state.connection = Some(Connection {
      stream,
      reader,
      writer: None,
      outbox: Vec::new(),
    });
    drop(state);
    self.connected.try_set(true);
  }

  /// Reads what came, and passes each whole line on.
  fn on_readable(self: &Rc<Self>) {
    let mut chunk = [0; 64 * 1024];
    let mut state = self.state.borrow_mut();
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    let (lines, closed) = match connection.stream.read(&mut chunk) {
      Ok(0) => {
        let rest = std::mem::take(&mut state.inbox);
        let last = (!rest.is_empty() && !state.skipping)
          .then(|| String::from_utf8_lossy(&rest).into_owned());
        (last.into_iter().collect(), Some("the other end closed it"))
      }
      Ok(read) => (state.take_lines(&chunk[..read], &self.path), None),
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      Err(error) => {
        drop(state);
        return self.drop_connection(&error.to_string());
      }
    };
    drop(state);
    // Closed first, so `on_line` may reconnect.
    if let Some(why) = closed {
      self.drop_connection(why);
    }
    self.deliver(lines);
  }

  /// Calls `on_line` with each line, with nothing borrowed.
  fn deliver(&self, lines: Vec<String>) {
    let (on_line, at) = {
      let state = self.state.borrow();
      (state.on_line.clone(), state.at)
    };
    let Some(on_line) = on_line else {
      return;
    };
    for line in lines {
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_line(&line))) {
        let path = self.path.display();
        let at = at.map_or_else(String::new, |at| format!(" (called at {at})"));
        let message = panic_message(payload.as_ref());
        tracing::error!("socket {path} on_line{at} panicked: {message}");
        let mut state = self.state.borrow_mut();
        // Unless it set a new one meanwhile.
        if state
          .on_line
          .as_ref()
          .is_some_and(|now| Rc::ptr_eq(now, &on_line))
        {
          state.on_line = None;
        }
        return;
      }
    }
  }

  fn write(self: &Rc<Self>, data: &[u8]) {
    let mut state = self.state.borrow_mut();
    let Some(connection) = state.connection.as_mut() else {
      let path = self.path.display();
      return tracing::warn!(%path, "the socket isn't connected, so a write was dropped");
    };
    connection.outbox.extend_from_slice(data);
    drop(state);
    self.flush();
  }

  /// Writes what the socket takes, and waits for room for the rest.
  fn flush(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) else {
      return;
    };
    let mut state = self.state.borrow_mut();
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    let mut broken = None;
    while !connection.outbox.is_empty() {
      match connection.stream.write(&connection.outbox) {
        Ok(written) => {
          connection.outbox.drain(..written);
        }
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(error) if error.kind() == ErrorKind::WouldBlock => break,
        Err(error) => {
          broken = Some(error.to_string());
          break;
        }
      }
    }
    if broken.is_none() && connection.outbox.len() > MAX_OUTBOX {
      broken = Some(String::from("the other end reads nothing it is sent"));
    }
    if broken.is_none() && connection.outbox.is_empty() {
      if let Some(writer) = connection.writer.take() {
        event_loop.remove(writer);
      }
    } else if broken.is_none() && connection.writer.is_none() {
      match connection.stream.as_fd().try_clone_to_owned() {
        Ok(fd) => {
          let inner = Rc::clone(self);
          let write = Box::new(move || inner.flush());
          connection.writer = event_loop.insert_fd(fd, Interest::WRITE, write);
        }
        Err(error) => broken = Some(error.to_string()),
      }
    }
    drop(state);
    if let Some(why) = broken {
      self.drop_connection(&why);
    }
  }

  /// Ends the connection the other end ended or broke, and logs why.
  fn drop_connection(&self, why: &str) {
    if self.state.borrow().connection.is_some() {
      let path = self.path.display();
      tracing::info!(%path, why, "the socket closed");
    }
    self.close();
  }

  fn close(&self) {
    let connection = {
      let mut state = self.state.borrow_mut();
      state.inbox.clear();
      state.connection.take()
    };
    if let (Some(connection), Some(event_loop)) = (
      connection,
      self.shared.as_ref().and_then(|shared| shared.event_loop()),
    ) {
      event_loop.remove(connection.reader);
      if let Some(writer) = connection.writer {
        event_loop.remove(writer);
      }
    }
    if self.connected.try_with(|connected| *connected) == Some(true) {
      self.connected.try_set(false);
    }
  }
}

impl State {
  /// The whole lines `bytes` completes, keeping the rest for later.
  fn take_lines(&mut self, bytes: &[u8], path: &std::path::Path) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pieces = bytes.split(|&byte| byte == b'\n').peekable();
    while let Some(piece) = pieces.next() {
      let ended = pieces.peek().is_some();
      if !self.skipping {
        self.inbox.extend_from_slice(piece);
        if self.inbox.len() > MAX_LINE {
          let path = path.display();
          tracing::warn!(%path, "a line over 1 MiB from the socket was dropped");
          self.inbox.clear();
          self.skipping = true;
        }
      }
      if ended {
        if !std::mem::take(&mut self.skipping) {
          lines.push(String::from_utf8_lossy(&self.inbox).into_owned());
        }
        self.inbox.clear();
      }
    }
    lines
  }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
  if let Some(message) = payload.downcast_ref::<&str>() {
    (*message).to_owned()
  } else if let Some(message) = payload.downcast_ref::<String>() {
    message.clone()
  } else {
    String::from("(panic payload is not a string)")
  }
}
