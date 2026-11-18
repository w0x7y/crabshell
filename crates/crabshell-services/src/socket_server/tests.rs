use std::cell::{Cell, RefCell};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{MAX_CONNECTIONS, SocketServer};
use crate::testing::{Harness, capture_logs, offline};
use crate::{ServicesExt, Socket};

/// A short temporary directory, since socket paths are capped at 108
/// bytes. It goes on drop.
struct Dir(PathBuf);

impl Dir {
  fn new() -> Dir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-server-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    Dir(dir)
  }

  fn join(&self, name: &str) -> PathBuf {
    self.0.join(name)
  }
}

impl Drop for Dir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

fn client(path: &Path) -> UnixStream {
  let stream = UnixStream::connect(path).expect("the server listens");
  stream
    .set_read_timeout(Some(Duration::from_millis(500)))
    .expect("a read timeout");
  stream
}

/// Whether the server hung up on `client`, rather than keeping it open.
fn hung_up(client: &mut UnixStream) -> bool {
  let mut rest = Vec::new();
  !matches!(client.read_to_end(&mut rest), Err(error)
    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
}

#[test]
fn each_connection_is_a_socket_whose_context_goes_when_it_ends() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let pieces = Rc::new(RefCell::new(Vec::new()));
  let held = Rc::new(RefCell::new(Vec::new()));
  let (kept, sockets) = (Rc::clone(&pieces), Rc::clone(&held));
  let _server = shell
    .cx
    .socket_server(dir.join("s"))
    .on_connection(move |cx, socket| {
      let kept = Rc::clone(&kept);
      let socket = socket.on_split("\0", move |piece| kept.borrow_mut().push(piece.to_owned()));
      socket.write("hello\n");
      sockets.borrow_mut().push((socket, cx.signal(0)));
    });
  let nodes = Runtime::current().node_count();
  let (mut first, mut second) = (client(&dir.join("s")), client(&dir.join("s")));
  run(&mut shell, 50);
  let mut hello = [0; 6];
  first.read_exact(&mut hello).unwrap();
  assert_eq!(&hello, b"hello\n");
  first.write_all(b"a\0b\0la").unwrap();
  run(&mut shell, 50);
  first.write_all(b"st").unwrap();
  drop(first);
  run(&mut shell, 50);
  assert_eq!(*pieces.borrow(), ["a", "b", "last"]);
  let (ended, made) = held.borrow()[0].clone();
  assert_eq!(
    ended.connected().try_get(),
    None,
    "its signals went with it"
  );
  assert_eq!(made.try_get(), None, "its context went");

  let (open, _) = held.borrow()[1].clone();
  let logs = capture_logs(|| open.reconnect());
  assert!(logs.contains("can't reconnect"), "{logs}");
  assert!(open.connected().get_untracked());
  open.close();
  run(&mut shell, 50);
  assert!(hung_up(&mut second));
  assert_eq!(Runtime::current().node_count(), nodes, "both contexts went");
}

#[test]
fn the_server_goes_with_its_context_or_close_and_takes_only_its_own_file() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let child = shell.cx.child();
  let _server = child.socket_server(dir.join("s")).on_connection(|_, _| {});
  let mut peer = client(&dir.join("s"));
  run(&mut shell, 50);
  child.owner().dispose();
  assert!(hung_up(&mut peer), "its connections closed");
  assert!(!dir.join("s").exists(), "its socket file went");

  let child = shell.cx.child();
  let _server = child.socket_server(dir.join("s"));
  std::fs::remove_file(dir.join("s")).unwrap();
  let _theirs = UnixListener::bind(dir.join("s")).unwrap();
  child.owner().dispose();
  assert!(dir.join("s").exists(), "a file that isn't its own stays");

  let server = shell
    .cx
    .socket_server(dir.join("t"))
    .on_connection(|_, _| {});
  let mut peer = client(&dir.join("t"));
  run(&mut shell, 50);
  let mut waiting = client(&dir.join("t"));
  server.close();
  assert!(hung_up(&mut peer));
  assert!(hung_up(&mut waiting), "one not taken yet is turned away");
  assert!(
    UnixStream::connect(dir.join("t")).is_err(),
    "no longer listening"
  );
}

#[test]
fn a_stale_socket_is_replaced_and_a_live_one_or_another_file_left_alone() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  drop(UnixListener::bind(dir.join("stale")).unwrap());
  let mut server = None;
  let logs = capture_logs(|| server = Some(shell.cx.socket_server(dir.join("stale"))));
  assert!(
    logs.contains("replaced a socket nobody listened on"),
    "{logs}"
  );
  let _peer = client(&dir.join("stale"));

  let _theirs = UnixListener::bind(dir.join("live")).unwrap();
  let inode = std::fs::metadata(dir.join("live")).unwrap().ino();
  std::fs::write(dir.join("file"), "keep").unwrap();
  let child = shell.cx.child();
  let logs = capture_logs(|| {
    child.socket_server(dir.join("live"));
    child.socket_server(dir.join("file"));
  });
  assert!(logs.contains("another program listens there"), "{logs}");
  assert!(logs.contains("isn't a socket"), "{logs}");
  child.owner().dispose();
  run(&mut shell, 20);
  assert_eq!(std::fs::metadata(dir.join("live")).unwrap().ino(), inode);
  assert_eq!(std::fs::read_to_string(dir.join("file")).unwrap(), "keep");
}

#[test]
fn connections_past_the_cap_are_closed_and_logged() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let _server = shell
    .cx
    .socket_server(dir.join("s"))
    .on_connection(|_, _| {});
  let mut open: Vec<_> = (0..MAX_CONNECTIONS)
    .map(|_| client(&dir.join("s")))
    .collect();
  run(&mut shell, 50);
  let mut extra = client(&dir.join("s"));
  let logs = capture_logs(|| {
    run(&mut shell, 50);
  });
  assert!(logs.contains("refused a connection"), "{logs}");
  assert!(hung_up(&mut extra));

  drop(open.pop());
  run(&mut shell, 50);
  let mut again = client(&dir.join("s"));
  run(&mut shell, 50);
  assert!(!hung_up(&mut again), "one that ended made room");
}

#[test]
fn a_panicking_on_connection_is_logged_and_disabled_and_the_rest_goes_on() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let pieces = Rc::new(RefCell::new(Vec::new()));
  let calls = Rc::new(Cell::new(0));
  let (kept, counted) = (Rc::clone(&pieces), Rc::clone(&calls));
  let _server = shell
    .cx
    .socket_server(dir.join("s"))
    .on_connection(move |_, socket| {
      counted.set(counted.get() + 1);
      assert!(counted.get() != 2, "bad client");
      let kept = Rc::clone(&kept);
      let _ = socket.on_split(";", move |piece| {
        assert!(piece != "boom", "bad piece");
        kept.borrow_mut().push(piece.to_owned());
      });
    });
  let mut good = client(&dir.join("s"));
  run(&mut shell, 50);
  let mut bad = client(&dir.join("s"));
  let logs = capture_logs(|| {
    run(&mut shell, 50);
  });
  assert!(
    logs.contains("socket server ")
      && logs
        .contains("on_connection (called at crates/crabshell-services/src/socket_server/tests.rs:"),
    "{logs}"
  );
  assert!(logs.contains("panicked: bad client"), "{logs}");
  assert!(hung_up(&mut bad), "its connection closed");
  let mut late = client(&dir.join("s"));
  run(&mut shell, 50);
  assert!(hung_up(&mut late), "nothing handles it now");
  assert_eq!(calls.get(), 2);

  good.write_all(b"fine;boom;after;").unwrap();
  let logs = capture_logs(|| {
    run(&mut shell, 50);
  });
  assert!(
    logs.contains("on_split (called at crates/crabshell-services/src/socket_server/tests.rs:"),
    "{logs}"
  );
  assert_eq!(*pieces.borrow(), ["fine"]);
}

#[test]
fn an_open_server_with_a_quiet_connection_wakes_nothing() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let _server = shell
    .cx
    .socket_server(dir.join("s"))
    .on_connection(|_, _| {});
  let _peer = client(&dir.join("s"));
  run(&mut shell, 50);
  assert_eq!(run(&mut shell, 1000), 1, "only the run's own deadline");
}

/// A server at `path` that keeps each connection's socket in the list.
fn keeping(shell: &Harness, path: &Path) -> (SocketServer, Rc<RefCell<Vec<Socket>>>) {
  let held = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&held);
  let server = shell
    .cx
    .socket_server(path)
    .on_connection(move |_, socket| kept.borrow_mut().push(socket));
  (server, held)
}

#[test]
fn a_client_that_only_stops_sending_still_gets_its_reply() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let _server = shell
    .cx
    .socket_server(dir.join("s"))
    .on_connection(|_, socket| {
      let replier = socket.clone();
      let _ = socket.on_line(move |line| {
        replier.write(format!("got {line}\n"));
        replier.close();
      });
    });
  // As `printf cmd | socat - UNIX-CONNECT:s` does.
  let mut peer = client(&dir.join("s"));
  peer.write_all(b"cmd").unwrap();
  peer.shutdown(std::net::Shutdown::Write).unwrap();
  run(&mut shell, 50);
  let mut reply = String::new();
  peer.read_to_string(&mut reply).unwrap();
  assert_eq!(reply, "got cmd\n");
}

#[test]
fn a_client_that_stops_sending_is_closed_after_a_while_without_a_reply() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let (_server, held) = keeping(&shell, &dir.join("s"));
  let mut peer = client(&dir.join("s"));
  peer.shutdown(std::net::Shutdown::Write).unwrap();
  run(&mut shell, 50);
  let socket = held.borrow()[0].clone();
  assert_eq!(socket.connected().try_get(), Some(true), "open for a reply");
  assert!(run(&mut shell, 200) <= 1, "nothing spins meanwhile");
  run(&mut shell, 200);
  assert_eq!(
    socket.connected().try_get(),
    None,
    "closed, its context gone"
  );
  assert!(hung_up(&mut peer));
}

#[test]
fn a_client_that_hangs_up_ends_its_connection_at_once() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let (_server, held) = keeping(&shell, &dir.join("s"));
  drop(client(&dir.join("s")));
  run(&mut shell, 50);
  assert_eq!(held.borrow()[0].connected().try_get(), None);

  let peer = client(&dir.join("s"));
  peer.shutdown(std::net::Shutdown::Write).unwrap();
  run(&mut shell, 50);
  drop(peer);
  let socket = held.borrow()[1].clone();
  socket.write("too late\n");
  run(&mut shell, 50);
  assert_eq!(socket.connected().try_get(), None, "the write failed");
}

#[test]
fn a_client_that_hangs_up_after_it_stopped_sending_ends_its_connection_at_once() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let (_server, held) = keeping(&shell, &dir.join("s"));
  let peer = client(&dir.join("s"));
  peer.shutdown(std::net::Shutdown::Write).unwrap();
  run(&mut shell, 50);
  let socket = held.borrow()[0].clone();
  assert_eq!(socket.connected().try_get(), Some(true), "open for a reply");
  assert!(run(&mut shell, 100) <= 1, "nothing spins meanwhile");
  // As socat does once its wait for a reply ends.
  drop(peer);
  run(&mut shell, 50);
  assert_eq!(
    socket.connected().try_get(),
    None,
    "closed with no write, well before the 300 ms cap"
  );
}

#[test]
fn a_server_and_its_connections_leave_no_nodes_behind() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let _server = child
    .socket_server(dir.join("s"))
    .on_connection(|cx, socket| {
      let count = cx.signal(0);
      let _ = socket.on_line(move |_| count.set(count.get_untracked() + 1));
    });
  let peers = [client(&dir.join("s")), client(&dir.join("s"))];
  run(&mut shell, 50);
  assert!(Runtime::current().node_count() > nodes);
  child.owner().dispose();
  run(&mut shell, 50);
  assert_eq!(Runtime::current().node_count(), nodes);
  drop(peers);
}

#[test]
fn a_back_off_stops_accepting_for_a_second() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let (server, held) = keeping(&shell, &dir.join("s"));
  // An accept that fails and leaves the client waiting, as with no file
  // descriptors left, can't be made here without starving the tests
  // beside this one, so this starts the back-off it leads to.
  server.inner.back_off();
  let _peer = client(&dir.join("s"));
  assert!(run(&mut shell, 500) <= 1, "nothing spins meanwhile");
  assert!(held.borrow().is_empty(), "not taken during the back-off");
  run(&mut shell, 700);
  assert_eq!(held.borrow().len(), 1, "taken once it is over");
}

#[test]
fn a_handler_may_close_its_server_and_open_another_at_the_same_path() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  let calls = Rc::new(Cell::new(0));
  let server = Rc::new(RefCell::new(None::<SocketServer>));
  let newer = Rc::new(RefCell::new(None::<SocketServer>));
  let (counted, held, opened) = (Rc::clone(&calls), Rc::clone(&server), Rc::clone(&newer));
  let (path, root) = (dir.join("s"), shell.cx.clone());
  let again = path.clone();
  let first = shell
    .cx
    .socket_server(&path)
    .on_connection(move |_, socket| {
      counted.set(counted.get() + 1);
      socket.write("hi\n");
      held.borrow().clone().unwrap().close();
      let next = root
        .socket_server(&again)
        .on_connection(|_, socket| socket.write("new\n"));
      *opened.borrow_mut() = Some(next);
    });
  *server.borrow_mut() = Some(first);
  let (mut a, mut b) = (client(&path), client(&path));
  run(&mut shell, 100);
  assert_eq!(calls.get(), 1);
  let mut hi = String::new();
  let _ = a.read_to_string(&mut hi);
  assert_eq!(hi, "hi\n");
  assert!(hung_up(&mut b), "b waited on the old listener");
  let mut c = client(&path);
  run(&mut shell, 100);
  let mut new = [0; 4];
  c.read_exact(&mut new).unwrap();
  assert_eq!(&new, b"new\n");
  server.borrow_mut().take();
  newer.borrow_mut().take();
}

/// The old shell of a reload listens on the path until it exits: the new
/// one's server binds once it runs alone, and one closed before that
/// never does.
#[test]
fn beside_the_old_shell_of_a_reload_a_server_listens_once_alone() {
  let dir = Dir::new();
  let mut shell = Harness::new(offline());
  crate::set_alone(&shell.cx, false);
  let old = UnixListener::bind(dir.join("shared")).unwrap();
  let child = shell.cx.child();
  let mut servers = Vec::new();
  let logs = capture_logs(|| {
    servers.push(shell.cx.socket_server(dir.join("shared")));
    child.socket_server(dir.join("gone"));
    run(&mut shell, 20);
  });
  assert!(!logs.contains("can't listen"), "{logs}");
  child.owner().dispose();
  // The old shell exits, leaving its file.
  drop(old);
  crate::set_alone(&shell.cx, true);
  let _peer = client(&dir.join("shared"));
  assert!(!dir.join("gone").exists(), "closed before it could bind");
}
