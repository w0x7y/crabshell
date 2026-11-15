//! Unix socket servers a config runs, as Quickshell's `SocketServer`: each
//! client that connects is a [`Socket`].

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::io::ErrorKind;
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::timer::TimeoutAction;
use calloop::{Interest, RegistrationToken};
use crabshell_ui::Cx;
use rustix::io::Errno;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use crate::Shared;
use crate::lines::panic_message;
use crate::socket::{self, Socket, Sockets};

/// The most connections open at once. A client past it is closed at once.
pub(crate) const MAX_CONNECTIONS: usize = 64;

/// How long the server rests after an accept that failed and left the
/// client waiting, as when the shell is out of file descriptors.
const BACK_OFF: Duration = Duration::from_secs(1);

/// A server on a Unix socket, from `cx.socket_server(path)`, as
/// Quickshell's `SocketServer`. It listens from the start and hands each
/// client that connects to [`on_connection`](Self::on_connection) as a
/// [`Socket`]. A clone is cheap and shares the server.
///
/// It stops when the context that made it goes, or at
/// [`close`](Self::close): every connection closes, and the socket file
/// goes if it is still the one the server made. A socket file already at
/// the path is replaced if nothing listens on it. One that something
/// listens on, or anything that isn't a socket, is left alone and logged,
/// and the server never listens. The file gets the permissions the
/// shell's umask leaves, so keep it in a directory only the user can
/// reach, such as `$XDG_RUNTIME_DIR`.
#[derive(Clone)]
pub struct SocketServer {
  inner: Rc<Inner>,
}

/// What a config does with each connection.
type OnConnection = Rc<dyn Fn(&Cx, Socket)>;

struct Inner {
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  path: PathBuf,
  /// The server's own context. Each connection's is made inside it, so
  /// disposing it ends them all.
  cx: Cx,
  /// Taken when the server stops, so a client still waiting is turned
  /// away.
  listener: RefCell<Option<UnixListener>>,
  /// The socket file's device and inode, so only ours is ever removed.
  file: Option<(u64, u64)>,
  /// The loop source that waits for clients, or the timer that brings it
  /// back after a back-off.
  source: Cell<Option<RegistrationToken>>,
  /// How many connections' contexts are alive.
  open: Rc<Cell<usize>>,
  on_connection: RefCell<Option<(OnConnection, &'static Location<'static>)>>,
}

/// `cx.socket_server(path)`.
pub(crate) fn socket_server(cx: &Cx, path: PathBuf) -> SocketServer {
  let shared = cx.service::<Sockets>().map(|sockets| Rc::clone(&sockets.0));
  if shared.is_none() {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.socket_server() needs the services the shell provides; it never listens");
    });
  }
  let bound = shared.as_ref().and_then(|_| {
    bind(&path)
      .inspect_err(|error| {
        let path = path.display();
        tracing::warn!(%path, %error, "can't listen on the socket");
      })
      .ok()
  });
  let (listener, file) = bound.unzip();
  let cx = cx.child();
  let inner = Rc::new(Inner {
    shared,
    path,
    cx: cx.clone(),
    listener: RefCell::new(listener),
    file,
    source: Cell::new(None),
    open: Rc::default(),
    on_connection: RefCell::new(None),
  });
  // Dropped with the server's context, which stops it.
  cx.owner().signal(StopOnDrop(Rc::clone(&inner)));
  inner.listen();
  SocketServer { inner }
}

/// Stops the server when its context's owner drops it.
struct StopOnDrop(Rc<Inner>);

impl Drop for StopOnDrop {
  fn drop(&mut self) {
    // Dropped outside the borrow: it may hold this server, and its drop
    // may use it.
    let on_connection = self.0.on_connection.borrow_mut().take();
    drop(on_connection);
    self.0.stop();
  }
}

/// Counts a connection while its context lives.
struct Open(Rc<Cell<usize>>);

impl Open {
  fn count(open: &Rc<Cell<usize>>) -> Open {
    open.set(open.get() + 1);
    Open(Rc::clone(open))
  }
}

impl Drop for Open {
  fn drop(&mut self) {
    self.0.set(self.0.get() - 1);
  }
}

impl SocketServer {
  /// Calls `on_connection` with each client that connects: a context of
  /// the connection's own, which goes on the loop's turn after the
  /// connection ends, and the connection as a [`Socket`], connected, for
  /// `on_line` or `on_split`, `write` and `close`. Lines are capped as a
  /// `Socket`'s are. The socket's `connected` signal goes with that
  /// context too, so code outside it reads it with `try_get`. A client
  /// that connects while nothing handles it is closed at once.
  ///
  /// A client that only stops sending, as `printf cmd | socat -
  /// UNIX-CONNECT:path` does, has its last line passed on, and the
  /// connection stays open for the reply until the config closes it, a
  /// write fails or 5 s pass, as the shell's own IPC socket waits. One
  /// that hangs up while still sending ends the connection at once; one
  /// that hangs up after it stopped sending, as socat does once its wait
  /// for a reply ends, keeps its place until a write fails or the 5 s
  /// pass, so a config that answers should close the connection.
  ///
  /// It runs on the main thread. If it panics, that is logged with the
  /// server's path and where `on_connection` was called, that connection
  /// is closed, and it isn't called again; the server and its other
  /// connections go on.
  #[track_caller]
  pub fn on_connection(self, on_connection: impl Fn(&Cx, Socket) + 'static) -> SocketServer {
    // Kept only while the server runs, so a handler that holds it can't
    // keep it alive.
    if !self.inner.cx.owner().is_disposed() {
      let handler: OnConnection = Rc::new(on_connection);
      let older = (self.inner.on_connection)
        .borrow_mut()
        .replace((handler, Location::caller()));
      drop(older);
    }
    self
  }

  /// Stops it, as its context going does: every connection closes, and
  /// the socket file goes. Nothing opens it again.
  pub fn close(&self) {
    self.inner.cx.owner().dispose();
  }
}

impl Inner {
  fn event_loop(&self) -> Option<Box<dyn crate::EventLoop>> {
    self.shared.as_ref()?.event_loop()
  }

  /// Waits for clients, unless the server stopped.
  fn listen(self: &Rc<Self>) {
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    let fd = match self
      .listener
      .borrow()
      .as_ref()
      .map(|listener| listener.as_fd().try_clone_to_owned())
    {
      Some(Ok(fd)) => fd,
      Some(Err(error)) => {
        let path = self.path.display();
        return tracing::warn!(%path, %error, "can't listen on the socket");
      }
      None => return,
    };
    let inner = Rc::clone(self);
    let ready = Box::new(move || inner.accept());
    self
      .source
      .set(event_loop.insert_fd(fd, Interest::READ, ready));
  }

  /// Takes every client waiting.
  fn accept(self: &Rc<Self>) {
    loop {
      // Not borrowed while a handler runs: it may close the server.
      let accepted = self.listener.borrow().as_ref().map(UnixListener::accept);
      match accepted {
        Some(Ok((stream, _))) => self.serve(stream),
        Some(Err(error)) if error.kind() == ErrorKind::WouldBlock => return,
        Some(Err(error))
          if matches!(
            error.kind(),
            ErrorKind::Interrupted | ErrorKind::ConnectionAborted
          ) => {}
        Some(Err(error)) => {
          let path = self.path.display();
          tracing::warn!(%path, %error, "can't accept a connection; trying again in 1 s");
          return self.back_off();
        }
        None => return,
      }
    }
  }

  /// Stops waiting for clients for [`BACK_OFF`], since the one waiting
  /// would wake the loop again at once.
  fn back_off(self: &Rc<Self>) {
    let Some(event_loop) = self.event_loop() else {
      return;
    };
    if let Some(token) = self.source.take() {
      event_loop.remove(token);
    }
    let inner = Rc::clone(self);
    let again = Box::new(move || {
      inner.source.set(None);
      inner.listen();
      TimeoutAction::Drop
    });
    self
      .source
      .set(event_loop.insert_timer(Instant::now() + BACK_OFF, again));
  }

  /// Hands a client to the config's handler, under a context of its own.
  fn serve(self: &Rc<Self>, stream: UnixStream) {
    let path = self.path.display();
    if self.open.get() >= MAX_CONNECTIONS {
      return tracing::warn!(%path, "refused a connection: {MAX_CONNECTIONS} are open already");
    }
    let handler = self.on_connection.borrow().clone();
    let (Some((handler, at)), Some(shared)) = (handler, &self.shared) else {
      return tracing::info!(%path, "closed a connection, as nothing handles it");
    };
    let cx = self.cx.child();
    cx.owner().signal(Open::count(&self.open));
    let Some(socket) = socket::accepted(&cx, shared, self.path.clone(), stream) else {
      return cx.owner().dispose();
    };
    let handed = socket.clone();
    if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| handler(&cx, handed))) {
      let message = panic_message(payload.as_ref());
      tracing::error!("socket server {path} on_connection (called at {at}) panicked: {message}");
      let mut current = self.on_connection.borrow_mut();
      // Unless it set a new one meanwhile.
      let disabled = current
        .as_ref()
        .is_some_and(|(now, _)| Rc::ptr_eq(now, &handler))
        .then(|| current.take());
      drop(current);
      drop(disabled);
      socket.close();
    }
  }

  /// Stops listening and removes the socket file if it is still ours.
  fn stop(&self) {
    if let (Some(token), Some(event_loop)) = (self.source.take(), self.event_loop()) {
      event_loop.remove(token);
    }
    let listener = self.listener.borrow_mut().take();
    if let (Some(_), Some((dev, ino))) = (listener, self.file) {
      let ours = std::fs::symlink_metadata(&self.path)
        .is_ok_and(|meta| (meta.dev(), meta.ino()) == (dev, ino));
      if ours {
        let _ = std::fs::remove_file(&self.path);
      }
    }
  }
}

/// Listens at `path`, in place of a socket file nobody listens on.
fn bind(path: &Path) -> std::io::Result<(UnixListener, (u64, u64))> {
  match std::fs::symlink_metadata(path) {
    Ok(meta) if !meta.file_type().is_socket() => {
      let why = "something that isn't a socket is there";
      return Err(std::io::Error::new(ErrorKind::AlreadyExists, why));
    }
    Ok(_) if listening(path)? => {
      let why = "another program listens there";
      return Err(std::io::Error::new(ErrorKind::AddrInUse, why));
    }
    Ok(_) => {
      std::fs::remove_file(path)?;
      let path = path.display();
      tracing::info!(%path, "replaced a socket nobody listened on");
    }
    Err(error) if error.kind() == ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }
  let listener = UnixListener::bind(path)?;
  listener.set_nonblocking(true)?;
  let meta = std::fs::symlink_metadata(path)?;
  Ok((listener, (meta.dev(), meta.ino())))
}

/// Whether something takes connections at `path`, found without waiting:
/// a full backlog counts as listening.
fn listening(path: &Path) -> std::io::Result<bool> {
  let flags = SocketFlags::NONBLOCK | SocketFlags::CLOEXEC;
  let probe = rustix::net::socket_with(AddressFamily::UNIX, SocketType::STREAM, flags, None)?;
  let refused = rustix::net::connect(&probe, &SocketAddrUnix::new(path)?);
  Ok(!matches!(refused, Err(Errno::CONNREFUSED | Errno::NOENT)))
}
