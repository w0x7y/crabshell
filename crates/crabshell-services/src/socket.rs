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
use std::time::{Duration, Instant};

use calloop::timer::TimeoutAction;
use calloop::{Interest, RegistrationToken};
use crabshell_reactive::{Owner, ReadSignal, Signal, untrack};
use crabshell_ui::Cx;

use crate::Shared;
use crate::lines::{Lines, panic_message};

/// The most bytes left to write before the socket is closed, so a program
/// that reads nothing can't grow the shell without end.
pub(crate) const MAX_OUTBOX: usize = 4 * 1024 * 1024;

/// How long a server's connection stays open for a reply once its client
/// stopped sending, as the shell's own IPC socket waits.
const HALF_OPEN: Duration = if cfg!(test) {
  Duration::from_millis(300)
} else {
  Duration::from_secs(5)
};

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
///
/// A [`SocketServer`](crate::SocketServer) hands each connection it
/// accepts over as a `Socket` too, which can't reconnect.
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
  /// For a connection a server accepted, its own context's owner, which
  /// goes once the connection ends.
  served: Option<Owner>,
}

/// What a config does with each line.
type OnLine = Rc<dyn Fn(&str)>;

#[derive(Default)]
struct State {
  connection: Option<Connection>,
  on_line: Option<OnLine>,
  /// Which method set `on_line` and where, for the log if it panics.
  at: Option<(&'static str, &'static Location<'static>)>,
  /// What was read and not passed on yet. Emptied by a close or a connect,
  /// so lines read before one aren't passed on after it.
  lines: Lines,
  /// A next-turn delivery is on its way, and passes on all that waits.
  later: bool,
}

struct Connection {
  stream: UnixStream,
  /// The loop source that waits for something to read.
  reader: RegistrationToken,
  /// The loop source that waits for room to write, while the socket
  /// holds back part of `outbox`.
  writer: Option<RegistrationToken>,
  outbox: Vec<u8>,
  /// Why writing failed, after which only reading goes on.
  write_failed: Option<String>,
  /// A server's client stopped sending but may read a reply: reading is
  /// over, and this timer closes the connection after [`HALF_OPEN`].
  half_open: Option<RegistrationToken>,
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
    served: None,
  });
  // Dropped with the opening context's owner, which closes the socket.
  cx.owner().signal(CloseOnDrop(Rc::clone(&inner)));
  inner.connect();
  Socket { inner }
}

/// A connection a server accepted on `path`, as a `Socket` under `cx`,
/// whose owner goes on the loop's turn after the connection ends. `None`,
/// logged, if the loop can't take it.
pub(crate) fn accepted(
  cx: &Cx,
  shared: &Rc<Shared>,
  path: PathBuf,
  stream: UnixStream,
) -> Option<Socket> {
  let inner = Rc::new(Inner {
    shared: Some(Rc::clone(shared)),
    path,
    connected: cx.signal(false),
    state: RefCell::default(),
    served: Some(cx.owner()),
  });
  cx.owner().signal(CloseOnDrop(Rc::clone(&inner)));
  if let Err(error) = inner.attach(stream) {
    let path = inner.path.display();
    tracing::warn!(%path, %error, "can't serve a connection");
    return None;
  }
  Some(Socket { inner })
}

/// Closes the socket when the opening context's owner drops it.
struct CloseOnDrop(Rc<Inner>);

impl Drop for CloseOnDrop {
  fn drop(&mut self) {
    // Dropped outside the borrow: `on_line` may hold this socket, which
    // would keep it alive, and its drop may use it.
    let on_line = self.0.state.borrow_mut().on_line.take();
    drop(on_line);
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
    self.handle("on_line", "\n", Rc::new(on_line))
  }

  /// Like [`on_line`](Self::on_line), with lines that end at `marker`
  /// instead of `\n`, such as `"\0"` or `"\r\n"`, as Quickshell's
  /// `SplitParser`. An empty `marker` passes on each read as it comes, up
  /// to 64 KiB, keeping a character the read cut off for the next.
  ///
  /// It replaces `on_line`, and everything not passed on yet is split at
  /// the new marker: set from a handler, from the line after the one it
  /// was given; set elsewhere, on the loop's next turn.
  #[track_caller]
  pub fn on_split(self, marker: &str, on_split: impl Fn(&str) + 'static) -> Socket {
    self.handle("on_split", marker, Rc::new(on_split))
  }

  #[track_caller]
  fn handle(self, name: &'static str, marker: &str, handler: OnLine) -> Socket {
    let mut state = self.inner.state.borrow_mut();
    state.on_line = Some(handler);
    state.at = Some((name, Location::caller()));
    state.lines.set_marker(marker);
    let waiting = state.lines.waiting();
    drop(state);
    if waiting {
      self.inner.deliver_later();
    }
    self
  }

  /// Sends `data`, all of it, as the other end takes it. While not
  /// connected it is dropped, and that is logged. If more than 4 MiB wait
  /// because the other end reads nothing, the socket is closed.
  pub fn write(&self, data: impl AsRef<[u8]>) {
    self.inner.write(data.as_ref());
  }

  /// Closes it and connects again, as after the other end restarted.
  /// Lines the old connection read and `on_line` hasn't had yet are
  /// dropped. A connection a server accepted can't: that is logged, and
  /// it stays as it is.
  pub fn reconnect(&self) {
    if self.inner.served.is_some() {
      let path = self.inner.path.display();
      return tracing::warn!(%path, "a connection a server accepted can't reconnect");
    }
    self.inner.close();
    self.inner.connect();
  }

  /// Closes it. [`reconnect`](Self::reconnect) opens it again. Lines read
  /// and not yet passed on are dropped.
  pub fn close(&self) {
    self.inner.close();
  }
}

impl Inner {
  /// Whether the opening context is still there. Untracked, so an effect
  /// that opens or reconnects a socket doesn't run again for it.
  fn alive(&self) -> bool {
    untrack(|| self.connected.try_with(|_| ()).is_some())
  }

  fn connect(self: &Rc<Self>) {
    let event_loop = self.shared.as_ref().and_then(|shared| shared.event_loop());
    if event_loop.is_none() || !self.alive() {
      return;
    }
    if let Err(error) = UnixStream::connect(&self.path).and_then(|stream| self.attach(stream)) {
      let path = self.path.display();
      tracing::warn!(%path, %error, "can't connect to the socket");
    }
  }

  /// Reads and writes `stream` from the loop from now on.
  fn attach(self: &Rc<Self>, stream: UnixStream) -> std::io::Result<()> {
    let event_loop = self
      .shared
      .as_ref()
      .and_then(|shared| shared.event_loop())
      .ok_or_else(|| std::io::Error::other("the loop is gone"))?;
    stream.set_nonblocking(true)?;
    let fd = stream.as_fd().try_clone_to_owned()?;
    let inner = Rc::clone(self);
    let read = Box::new(move || inner.on_readable());
    let reader = event_loop
      .insert_fd(fd, Interest::READ, read)
      .ok_or_else(|| std::io::Error::other("the loop refused it"))?;
    let mut state = self.state.borrow_mut();
    state.lines.reset();
    state.connection = Some(Connection {
      stream,
      reader,
      writer: None,
      outbox: Vec::new(),
      write_failed: None,
      half_open: None,
    });
    drop(state);
    self.connected.try_set(true);
    Ok(())
  }

  /// Reads what came, and passes each whole line on.
  fn on_readable(self: &Rc<Self>) {
    let mut chunk = [0; 64 * 1024];
    let mut state = self.state.borrow_mut();
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    let closed = match connection.stream.read(&mut chunk) {
      // As `printf cmd | socat - UNIX-CONNECT:path` does: the client
      // stopped sending and waits for the reply.
      Ok(0) if self.served.is_some() && !hung_up(&connection.stream) => {
        drop(state);
        self.half_close();
        return self.deliver(true);
      }
      Ok(0) => Some(String::from("the other end closed it")),
      Ok(read) => {
        state.lines.append(&chunk[..read]);
        None
      }
      Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
        return;
      }
      // As a reset from a peer that closed with input unread: the end.
      Err(error) => Some(error.to_string()),
    };
    drop(state);
    // Closed first, so a handler may reconnect.
    if let Some(why) = &closed {
      self.drop_connection(why);
    }
    self.deliver(closed.is_some());
  }

  /// Stops reading a server's connection whose client stopped sending,
  /// and keeps it open for the reply until the config closes it, a write
  /// fails or [`HALF_OPEN`] passes.
  fn half_close(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) else {
      return;
    };
    let mut state = self.state.borrow_mut();
    let Some(connection) = state.connection.as_mut() else {
      return;
    };
    // At its end it would be readable for good.
    event_loop.remove(connection.reader);
    let inner = Rc::clone(self);
    let expire = move || {
      // Gone with this call, so the close doesn't remove it again.
      if let Some(connection) = inner.state.borrow_mut().connection.as_mut() {
        connection.half_open = None;
      }
      inner.drop_connection("the client got no reply in time");
      TimeoutAction::Drop
    };
    connection.half_open = event_loop.insert_timer(Instant::now() + HALF_OPEN, Box::new(expire));
    let unbounded = connection.half_open.is_none();
    drop(state);
    if unbounded {
      self.drop_connection("the client stopped sending");
    }
  }

  /// Passes on what waits at the loop's next turn, as a marker set outside
  /// a handler asks.
  fn deliver_later(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) else {
      return;
    };
    if std::mem::replace(&mut self.state.borrow_mut().later, true) {
      return;
    }
    let inner = Rc::clone(self);
    let later = move || {
      inner.state.borrow_mut().later = false;
      inner.deliver(false);
      TimeoutAction::Drop
    };
    event_loop.insert_timer(Instant::now(), Box::new(later));
  }

  /// Passes each whole line waiting to the handler set at the time, one at
  /// a time with nothing borrowed, so one that sets another marker has the
  /// rest split at it. When the other end is `done`, what is left goes
  /// last. A close or a reconnect empties what waits, which ends it.
  fn deliver(&self, done: bool) {
    loop {
      let (line, on_line, at) = {
        let mut state = self.state.borrow_mut();
        let line = match state.lines.next(&self.path.display()) {
          Some(line) => line,
          None if done => match state.lines.rest(&self.path.display()) {
            Some(line) => line,
            None => return,
          },
          None => return,
        };
        (line, state.on_line.clone(), state.at)
      };
      let Some(on_line) = on_line else {
        continue;
      };
      if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| on_line(&line))) {
        let path = self.path.display();
        let (name, at) = at.map_or(("on_line", String::new()), |(name, at)| {
          (name, format!(" (called at {at})"))
        });
        let message = panic_message(payload.as_ref());
        tracing::error!("socket {path} {name}{at} panicked: {message}");
        let mut state = self.state.borrow_mut();
        // Unless it set a new one meanwhile.
        if state
          .on_line
          .as_ref()
          .is_some_and(|now| Rc::ptr_eq(now, &on_line))
        {
          state.on_line = None;
        }
      }
    }
  }

  fn write(self: &Rc<Self>, data: &[u8]) {
    let mut state = self.state.borrow_mut();
    let Some(connection) = state
      .connection
      .as_mut()
      .filter(|connection| connection.write_failed.is_none())
    else {
      let path = self.path.display();
      return tracing::warn!(%path, "the socket takes no writes now, so a write was dropped");
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
          // The other end may still have sent something: stop writing
          // and read on to its end.
          let path = self.path.display();
          tracing::info!(%path, %error, "the socket takes no more writes");
          connection.write_failed = Some(error.to_string());
          connection.outbox = Vec::new();
          // Unless reading is over too, as for a half-open connection.
          if connection.half_open.is_some() {
            broken = Some(error.to_string());
          }
          break;
        }
      }
    }
    if connection.outbox.len() > MAX_OUTBOX {
      broken = Some(String::from("the other end reads nothing it is sent"));
    }
    if broken.is_none() && connection.outbox.is_empty() {
      // A big write's room isn't kept.
      connection.outbox.shrink_to(64 * 1024);
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
  /// Lines it read before still reach `on_line`.
  fn drop_connection(&self, why: &str) {
    if self.state.borrow().connection.is_some() {
      let path = self.path.display();
      tracing::info!(%path, why, "the socket closed");
    }
    self.end(false);
  }

  /// Closes it as the config asked, so lines read before are dropped.
  fn close(&self) {
    self.end(true);
  }

  fn end(&self, asked: bool) {
    let connection = {
      let mut state = self.state.borrow_mut();
      if asked {
        state.lines.reset();
      }
      state.connection.take()
    };
    if let (Some(connection), Some(event_loop)) = (
      connection,
      self.shared.as_ref().and_then(|shared| shared.event_loop()),
    ) {
      event_loop.remove(connection.reader);
      for token in [connection.writer, connection.half_open]
        .into_iter()
        .flatten()
      {
        event_loop.remove(token);
      }
      // A server's connection never connects again, so its context goes,
      // on the next turn: a handler that closed it may still use it.
      if let Some(owner) = self.served.filter(|_| self.alive()) {
        let gone = move || {
          owner.dispose();
          TimeoutAction::Drop
        };
        event_loop.insert_timer(Instant::now(), Box::new(gone));
      }
    }
    if untrack(|| self.connected.try_with(|connected| *connected)) == Some(true) {
      self.connected.try_set(false);
    }
  }
}

/// Whether the other end of `stream` is gone both ways, rather than only
/// done sending. Asked at once, without waiting.
fn hung_up(stream: &UnixStream) -> bool {
  use rustix::event::{PollFd, PollFlags, Timespec};

  let mut fds = [PollFd::new(stream, PollFlags::empty())];
  let now = Timespec::default();
  rustix::event::poll(&mut fds, Some(&now)).is_err() || fds[0].revents().contains(PollFlags::HUP)
}
