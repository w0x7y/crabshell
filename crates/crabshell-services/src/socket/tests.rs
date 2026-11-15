use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;

use super::{MAX_OUTBOX, Socket};
use crate::ServicesExt;
use crate::lines::MAX_LINE;
use crate::testing::{Harness, capture_logs, offline};

/// A listening socket in a short temporary directory, since socket paths
/// are capped at 108 bytes. The directory goes on drop.
struct Server {
  dir: PathBuf,
  listener: UnixListener,
}

impl Server {
  fn new() -> Server {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-socket-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let listener = UnixListener::bind(dir.join("s")).expect("a socket in the directory");
    Server { dir, listener }
  }

  fn path(&self) -> PathBuf {
    self.dir.join("s")
  }

  /// The client the shell connected, which it did at once.
  fn accept(&self) -> UnixStream {
    let (stream, _) = self.listener.accept().expect("a client");
    stream
      .set_read_timeout(Some(Duration::from_secs(2)))
      .expect("a read timeout");
    stream
  }
}

impl Drop for Server {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

/// A socket at `path` whose lines are kept.
fn open(shell: &Harness, path: PathBuf) -> (Socket, Rc<RefCell<Vec<String>>>) {
  let lines = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&lines);
  let socket = shell
    .cx
    .socket(path)
    .on_line(move |line| kept.borrow_mut().push(line.to_owned()));
  (socket, lines)
}

#[test]
fn it_passes_on_each_line_and_the_rest_when_the_other_end_closes() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, lines) = open(&shell, server.path());
  let mut peer = server.accept();
  assert!(socket.connected().get_untracked());

  peer.write_all(b"one\ntw").unwrap();
  run(&mut shell, 50);
  peer.write_all(b"o\n\nfour \xff\nlast").unwrap();
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["one", "two", "", "four \u{fffd}"]);

  drop(peer);
  run(&mut shell, 50);
  assert_eq!(lines.borrow().last().map(String::as_str), Some("last"));
  assert!(!socket.connected().get_untracked());
}

#[test]
fn on_split_splits_at_its_marker_and_an_empty_one_passes_on_each_read() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (pieces, chunks) = (
    Rc::new(RefCell::new(Vec::new())),
    Rc::new(RefCell::new(Vec::new())),
  );
  let kept = Rc::clone(&pieces);
  let _split = shell
    .cx
    .socket(server.path())
    .on_split("\r\n", move |piece| {
      kept.borrow_mut().push(piece.to_owned())
    });
  let kept = Rc::clone(&chunks);
  let _chunked = shell
    .cx
    .socket(server.path())
    .on_split("", move |chunk| kept.borrow_mut().push(chunk.to_owned()));
  let (mut split, mut chunked) = (server.accept(), server.accept());

  split.write_all(b"one\r").unwrap();
  chunked.write_all(b"ab").unwrap();
  run(&mut shell, 50);
  split.write_all(b"\ntwo\nstill\r\nlast").unwrap();
  chunked.write_all(b"c\nd").unwrap();
  run(&mut shell, 50);
  drop((split, chunked));
  run(&mut shell, 50);
  assert_eq!(*pieces.borrow(), ["one", "two\nstill", "last"]);
  assert_eq!(*chunks.borrow(), ["ab", "c\nd"]);
}

#[test]
fn writes_reach_the_other_end_even_past_its_buffer() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, _) = open(&shell, server.path());
  let mut peer = server.accept();
  let big = vec![b'x'; 1024 * 1024];
  socket.write(b"hello\n");
  socket.write(&big);
  let reader = std::thread::spawn(move || {
    let mut got = vec![0; 6 + 1024 * 1024];
    peer.read_exact(&mut got).expect("everything written");
    got
  });
  let give_up = Instant::now() + Duration::from_secs(5);
  while !reader.is_finished() {
    assert!(Instant::now() < give_up, "the write didn't finish");
    run(&mut shell, 10);
  }
  let got = reader.join().unwrap();
  assert_eq!(&got[..6], b"hello\n");
  assert!(got[6..].iter().all(|&byte| byte == b'x'));
}

#[test]
fn a_write_while_closed_is_dropped_and_logged() {
  let server = Server::new();
  let shell = Harness::new(offline());
  let (socket, _) = open(&shell, server.path());
  socket.close();
  assert!(!socket.connected().get_untracked());
  let logs = capture_logs(|| socket.write(b"lost\n"));
  assert!(logs.contains("a write was dropped"), "{logs}");
}

#[test]
fn a_missing_socket_is_logged_and_reconnect_tries_again() {
  let server = Server::new();
  let path = server.dir.join("later");
  let mut shell = Harness::new(offline());
  let mut socket = None;
  let logs = capture_logs(|| socket = Some(open(&shell, path.clone()).0));
  let socket = socket.unwrap();
  assert!(logs.contains("can't connect to the socket"), "{logs}");
  assert!(!socket.connected().get_untracked());

  let listener = UnixListener::bind(&path).unwrap();
  socket.reconnect();
  assert!(socket.connected().get_untracked());
  let (mut peer, _) = listener.accept().unwrap();
  socket.write(b"back\n");
  run(&mut shell, 20);
  let mut got = [0; 5];
  peer.read_exact(&mut got).unwrap();
  assert_eq!(&got, b"back\n");
}

#[test]
fn a_line_over_the_cap_is_dropped_and_the_next_one_arrives() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (_socket, lines) = open(&shell, server.path());
  let mut peer = server.accept();
  let writer = std::thread::spawn(move || {
    peer.write_all(&vec![b'y'; MAX_LINE + 10]).unwrap();
    peer.write_all(b"\nnext\n").unwrap();
    peer
  });
  let logs = capture_logs(|| {
    let give_up = Instant::now() + Duration::from_secs(5);
    while lines.borrow().is_empty() {
      assert!(Instant::now() < give_up, "no line after the long one");
      run(&mut shell, 10);
    }
  });
  let _peer = writer.join().unwrap();
  assert_eq!(*lines.borrow(), ["next"]);
  assert!(logs.contains("a line over 1 MiB"), "{logs}");
}

#[test]
fn a_panicking_on_line_is_logged_and_disabled_and_the_socket_stays_open() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let calls = Rc::new(RefCell::new(0));
  let counted = Rc::clone(&calls);
  let socket = shell.cx.socket(server.path()).on_line(move |line| {
    *counted.borrow_mut() += 1;
    panic!("bad line {line}");
  });
  let (other, other_lines) = open(&shell, server.path());
  let (mut peer, mut other_peer) = (server.accept(), server.accept());
  peer.write_all(b"a\nb\n").unwrap();
  other_peer.write_all(b"fine\n").unwrap();
  let logs = capture_logs(|| {
    run(&mut shell, 50);
  });
  assert!(
    logs.contains("socket ")
      && logs.contains("on_line (called at crates/crabshell-services/src/socket/tests.rs:"),
    "{logs}"
  );
  assert!(logs.contains("panicked: bad line a"), "{logs}");
  peer.write_all(b"c\n").unwrap();
  run(&mut shell, 50);
  assert_eq!(*calls.borrow(), 1);
  assert!(socket.connected().get_untracked());
  assert!(other.connected().get_untracked());
  assert_eq!(*other_lines.borrow(), ["fine"]);
}

#[test]
fn the_socket_closes_when_its_context_goes() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let child = shell.cx.child();
  let lines = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&lines);
  let socket = child
    .socket(server.path())
    .on_line(move |line| kept.borrow_mut().push(line.to_owned()));
  let mut peer = server.accept();
  child.owner().dispose();
  // The shell hung up, so this may already fail.
  let _ = peer.write_all(b"too late\n");
  run(&mut shell, 50);
  assert!(lines.borrow().is_empty());
  let mut rest = Vec::new();
  let hung_up = match peer.read_to_end(&mut rest) {
    Ok(_) => rest.is_empty(),
    // The write above, unread when the shell hung up, resets it instead.
    Err(error) => error.kind() == ErrorKind::ConnectionReset,
  };
  assert!(hung_up, "the shell hung up");
  socket.reconnect();
  assert!(
    server.listener.set_nonblocking(true).is_ok()
      && matches!(server.listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
    "a socket whose context went never connects again"
  );
}

#[test]
fn a_quiet_open_socket_wakes_nothing() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, _) = open(&shell, server.path());
  let _peer = server.accept();
  run(&mut shell, 50);
  assert!(socket.connected().get_untracked());
  assert_eq!(run(&mut shell, 1000), 1, "only the run's own deadline");
}

#[test]
fn an_other_end_that_reads_nothing_is_dropped() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, _) = open(&shell, server.path());
  let _peer = server.accept();
  let logs = capture_logs(|| {
    // Past what the kernel's buffer takes, too.
    socket.write(vec![0; 2 * MAX_OUTBOX]);
    run(&mut shell, 20);
  });
  assert!(!socket.connected().get_untracked());
  assert!(logs.contains("reads nothing"), "{logs}");
  Runtime::current().flush();
}

#[test]
fn reconnecting_from_an_effect_connects_once_per_run() {
  let server = Server::new();
  server.listener.set_nonblocking(true).unwrap();
  let mut shell = Harness::new(offline());
  let socket = open(&shell, server.path()).0;
  let poke = shell.cx.signal(0);
  shell.cx.effect(move || {
    poke.get();
    socket.reconnect();
  });
  Runtime::current().flush();
  poke.set(1);
  Runtime::current().flush();
  run(&mut shell, 20);
  let accepted = std::iter::from_fn(|| server.listener.accept().ok()).count();
  // The first open, the effect's first run, and the poke.
  assert_eq!(accepted, 3);
}

#[test]
fn opening_a_socket_in_an_effect_opens_one() {
  let server = Server::new();
  server.listener.set_nonblocking(true).unwrap();
  let mut shell = Harness::new(offline());
  let cx = shell.cx.clone();
  let path = server.path();
  shell.cx.effect(move || {
    let _socket = cx.socket(path.clone());
  });
  Runtime::current().flush();
  run(&mut shell, 20);
  let accepted = std::iter::from_fn(|| server.listener.accept().ok()).count();
  assert_eq!(accepted, 1);
}

#[test]
fn an_on_line_that_holds_its_socket_doesnt_keep_it_once_its_context_goes() {
  let server = Server::new();
  let shell = Harness::new(offline());
  let child = shell.cx.child();
  let socket = child.socket(server.path());
  let held = socket.clone();
  let socket = socket.on_line(move |line| held.write(line));
  let weak = Rc::downgrade(&socket.inner);
  drop(socket);
  child.owner().dispose();
  assert!(weak.upgrade().is_none(), "the socket was freed");
}

#[test]
fn a_write_the_other_end_refuses_still_lets_its_last_lines_in() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, lines) = open(&shell, server.path());
  let mut peer = server.accept();
  peer.write_all(b"result\n").unwrap();
  drop(peer);
  socket.write(b"ack\n");
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["result"]);
}

#[test]
fn a_reset_still_passes_on_the_last_line() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, lines) = open(&shell, server.path());
  let mut peer = server.accept();
  // Left unread, so the peer's close resets the connection.
  socket.write(b"unread\n");
  run(&mut shell, 20);
  peer.write_all(b"one\nlast").unwrap();
  drop(peer);
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["one", "last"]);
}

#[test]
fn lines_after_on_line_closes_the_socket_are_dropped() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let lines = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&lines);
  let socket = shell.cx.socket(server.path());
  let closer = socket.clone();
  let _socket = socket.on_line(move |line| {
    kept.borrow_mut().push(line.to_owned());
    if line == "bye" {
      closer.close();
    }
  });
  let mut peer = server.accept();
  peer.write_all(b"bye\nafter\n").unwrap();
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["bye"]);
}

#[test]
fn a_handler_that_sets_another_marker_has_the_rest_split_at_it() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let pieces = Rc::new(RefCell::new(Vec::new()));
  let socket = Rc::new(RefCell::new(None::<Socket>));
  let (kept, held) = (Rc::clone(&pieces), Rc::clone(&socket));
  let opened = shell.cx.socket(server.path()).on_line(move |line| {
    kept.borrow_mut().push(format!("line {line}"));
    let kept = Rc::clone(&kept);
    let socket = held.borrow().clone().unwrap();
    let _ = socket.on_split(";", move |piece| {
      kept.borrow_mut().push(format!("piece {piece}"))
    });
  });
  *socket.borrow_mut() = Some(opened);
  let mut peer = server.accept();
  peer.write_all(b"HEADER\nr1;r2\nr3;").unwrap();
  run(&mut shell, 100);
  assert_eq!(
    *pieces.borrow(),
    ["line HEADER", "piece r1", "piece r2\nr3"]
  );
  socket.borrow_mut().take();
}

#[test]
fn a_marker_set_outside_a_handler_splits_what_waits_on_the_next_turn() {
  let server = Server::new();
  let mut shell = Harness::new(offline());
  let (socket, lines) = open(&shell, server.path());
  let peer = server.accept();
  (&peer).write_all(b"a;b").unwrap();
  run(&mut shell, 50);
  assert!(lines.borrow().is_empty(), "no newline yet");
  let kept = Rc::clone(&lines);
  let _socket = socket.on_split(";", move |piece| kept.borrow_mut().push(piece.to_owned()));
  assert!(lines.borrow().is_empty(), "not inside the call");
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["a"]);
  drop(peer);
  run(&mut shell, 50);
  assert_eq!(*lines.borrow(), ["a", "b"]);
}
