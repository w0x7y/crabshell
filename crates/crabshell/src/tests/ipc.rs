//! `cx.ipc` on a plain calloop loop, driven by hand, with a client on a
//! `UnixStream`: the protocol, the limits, owners, the socket file, a
//! panicking handler and an idle socket.

use std::cell::Cell;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use super::capture_logs;
use crate::ipc::{self, Ipc, Request};

/// What `crabshell ipc launcher toggle a b` sends, and `crabshell ipc`.
const CALL: &str = "{\"args\":[\"a\",\"b\"],\"function\":\"toggle\",\"target\":\"launcher\"}\n";
const LIST: &str = "{\"list\":true}\n";

/// A request line for `target function`, with no arguments.
fn call(target: &str, function: &str) -> String {
  format!("{{\"target\":\"{target}\",\"function\":\"{function}\"}}\n")
}

type Reply = Result<String, String>;

/// A handler that always says `text`.
fn says(text: &'static str) -> impl Fn(&[String]) -> Reply {
  move |_| Ok(text.to_owned())
}

fn ok(text: &str) -> String {
  format!("{}\n", serde_json::json!({ "ok": text }))
}

fn error(text: &str) -> String {
  format!("{}\n", serde_json::json!({ "error": text }))
}

/// A directory removed when dropped.
struct TempDir(PathBuf);

impl Drop for TempDir {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.0);
  }
}

/// An `Ipc` on a loop of its own, serving at a socket in a fresh directory.
struct Server {
  event_loop: calloop::EventLoop<'static, ()>,
  ipc: Rc<Ipc<()>>,
  path: PathBuf,
  cx: Cx,
  _dir: TempDir,
}

fn server(name: &str) -> Server {
  let dir = std::env::temp_dir().join(format!("crabshell-ipc-{}-{name}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  let path = dir.join("crabshell").join("ipc-wayland-0.sock");
  let event_loop = calloop::EventLoop::try_new().expect("a loop");
  let ipc = Ipc::new(&event_loop.handle(), Some(path.clone()));
  let cx = Cx::new(Runtime::current().new_owner());
  let _dir = TempDir(dir);
  Server {
    event_loop,
    ipc,
    path,
    cx,
    _dir,
  }
}

#[track_caller]
fn register(
  ipc: &Rc<Ipc<()>>,
  cx: &Cx,
  target: &str,
  function: &str,
  f: impl Fn(&[String]) -> Reply + 'static,
) {
  ipc.register(cx, target, function, Box::new(f), Location::caller());
}

impl Server {
  #[track_caller]
  fn register(&self, target: &str, function: &str, f: impl Fn(&[String]) -> Reply + 'static) {
    register(&self.ipc, &self.cx, target, function, f);
  }

  fn turn(&mut self) {
    let turn = Some(Duration::from_millis(5));
    self
      .event_loop
      .dispatch(turn, &mut ())
      .expect("the loop runs");
  }

  fn connect(&mut self) -> UnixStream {
    let client = UnixStream::connect(&self.path).expect("connected");
    client.set_nonblocking(true).expect("nonblocking");
    client
  }

  /// Sends `line` on a new connection, ending the input after it if `end`,
  /// and returns all the shell said.
  fn ask_with(&mut self, line: &str, end: bool) -> String {
    let mut client = self.connect();
    client.write_all(line.as_bytes()).expect("sent");
    if end {
      client.shutdown(std::net::Shutdown::Write).expect("shut");
    }
    self.read_to_close(&mut client)
  }

  fn ask(&mut self, line: &str) -> String {
    self.ask_with(line, false)
  }

  /// Turns the loop until the shell closes `client`, and returns what it
  /// sent before.
  fn read_to_close(&mut self, client: &mut UnixStream) -> String {
    let started = Instant::now();
    let (mut said, mut chunk) = (Vec::new(), vec![0_u8; 1 << 16]);
    loop {
      self.turn();
      loop {
        match client.read(&mut chunk) {
          Ok(0) => return String::from_utf8(said).expect("UTF-8"),
          Ok(read) => said.extend_from_slice(&chunk[..read]),
          Err(error) if error.kind() == ErrorKind::WouldBlock => break,
          Err(error) => panic!("can't read the reply: {error}"),
        }
      }
      assert!(started.elapsed() < Duration::from_secs(10), "never closed");
    }
  }

  /// How long the loop slept, given up to `time`.
  fn sleep(&mut self, time: Duration) -> Duration {
    let started = Instant::now();
    self
      .event_loop
      .dispatch(Some(time), &mut ())
      .expect("the loop runs");
    started.elapsed()
  }
}

#[test]
fn a_request_is_a_call_or_the_list_and_a_reply_is_one_line() {
  let args = vec![String::from("a"), String::from("b")];
  let (target, function) = (String::from("launcher"), String::from("toggle"));
  assert_eq!(
    ipc::parse(CALL.as_bytes()),
    Ok(Request::Call {
      target,
      function,
      args
    })
  );
  assert_eq!(ipc::parse(LIST.as_bytes()), Ok(Request::List));
  let (target, function, args) = (String::from("bar"), String::from("hide"), Vec::new());
  assert_eq!(
    ipc::parse(call("bar", "hide").as_bytes()),
    Ok(Request::Call {
      target,
      function,
      args
    })
  );
  let neither = Err(String::from(
    "a request names a target and a function, or asks for the list",
  ));
  for line in [
    r#"{"target":"bar"}"#,
    r#"{"list":true,"target":"bar","function":"x"}"#,
    "{}",
  ] {
    assert_eq!(ipc::parse(line.as_bytes()), neither, "{line}");
  }
  let junk = ipc::parse(b"toggle").expect_err("not JSON");
  assert!(junk.starts_with("the request doesn't parse: "), "{junk}");
  assert!(ipc::parse(br#"{"target":"a","function":"b","args":[1]}"#).is_err());

  assert_eq!(
    ipc::reply(Ok(String::from("on\n"))),
    b"{\"ok\":\"on\\n\"}\n"
  );
  assert_eq!(ipc::reply(Err(String::from("no"))), b"{\"error\":\"no\"}\n");
}

#[test]
fn names_are_short_words_of_ascii() {
  for good in ["launcher", "a", "bar-2.top_left", &"x".repeat(64)] {
    assert!(ipc::valid_name(good), "{good}");
  }
  for bad in [
    "",
    "two words",
    "slash/y",
    "naïve",
    "new\nline",
    &"x".repeat(65),
  ] {
    assert!(!ipc::valid_name(bad), "{bad:?}");
  }
}

#[test]
fn the_socket_is_named_for_the_display() {
  let path = |dir: Option<&str>, display: Option<&str>| {
    ipc::socket_path(dir.map(AsRef::as_ref), display.map(AsRef::as_ref))
  };
  let at = |name: &str| Some(PathBuf::from(format!("/run/user/7/crabshell/{name}")));
  assert_eq!(
    path(Some("/run/user/7"), Some("wayland-1")),
    at("ipc-wayland-1.sock")
  );
  assert_eq!(path(Some("/run/user/7"), None), at("ipc-wayland-0.sock"));
  assert_eq!(
    path(Some("/run/user/7"), Some("")),
    at("ipc-wayland-0.sock")
  );
  let last = path(Some("/run/user/7"), Some("/tmp/sway/wayland-5"));
  assert_eq!(
    last,
    at("ipc-wayland-5.sock"),
    "only the last part of a path"
  );
  assert_eq!(path(None, Some("wayland-1")), None);
  assert_eq!(path(Some(""), Some("wayland-1")), None);
  assert_eq!(path(Some("run/7"), Some("wayland-1")), None, "relative");
}

#[test]
fn nothing_is_opened_until_a_handler_registers_then_calls_and_the_list_are_answered() {
  let mut server = server("call");
  server.turn();
  assert!(
    !server.path.parent().expect("a directory").exists(),
    "no handler, no socket"
  );

  // A handler that sets a signal: its effect runs on the flush after.
  let count = server.cx.signal(0_usize);
  let seen = Rc::new(Cell::new(0));
  let saw = Rc::clone(&seen);
  server.cx.effect(move || saw.set(count.get()));
  server.register("launcher", "toggle", move |args: &[String]| {
    count.update(|count| *count += args.len());
    Ok(format!("{} {}", args.join(","), count.get_untracked()))
  });
  server.register("bar", "hide", says(""));
  assert!(server.path.exists(), "bound by the first handler");
  assert_eq!(server.ask(CALL), ok("a,b 2"));
  Runtime::current().flush();
  assert_eq!(seen.get(), 2);
  assert_eq!(server.ask(LIST), ok("bar hide\nlauncher toggle"));
  let line = call("bar", "hide");
  assert_eq!(
    server.ask_with(line.trim_end(), true),
    ok(""),
    "the end of input ends a request too"
  );
}

#[test]
fn unknown_names_bad_requests_and_oversize_ones_get_errors() {
  let mut server = server("errors");
  server.register("bar", "hide", says("hidden"));
  assert_eq!(server.ask(CALL), error("no IPC handler launcher.toggle"));
  assert_eq!(
    server.ask(&call("bar", "show")),
    error("no IPC handler bar.show")
  );
  let junk = server.ask("hide\n");
  assert!(
    junk.starts_with(r#"{"error":"the request doesn't parse: "#),
    "{junk}"
  );
  let huge = format!(
    "{{\"target\":\"bar\",\"function\":\"hide\",\"args\":[\"{}\"]}}",
    "x".repeat(64 * 1024)
  );
  let over = error("the request is over 64 KiB");
  assert_eq!(server.ask_with(&huge, true), over);
  assert_eq!(server.ask(&format!("{huge}\n")), over, "with its newline");
  assert_eq!(
    server.ask(&call("bar", "hide")),
    ok("hidden"),
    "still answering"
  );
}

#[test]
fn a_reply_bigger_than_the_socket_takes_is_written_as_it_drains_and_cut_at_1_mib() {
  let mut server = server("big");
  // One byte, then two-byte characters: 1 MiB falls inside one.
  server.register("big", "text", |_: &[String]| {
    Ok(format!("a{}", "é".repeat(1 << 20)))
  });
  let mut reply = String::new();
  let logs = capture_logs(|| reply = server.ask(&call("big", "text")));
  let reply: serde_json::Value = serde_json::from_str(&reply).expect("JSON");
  let text = reply["ok"].as_str().expect("an ok reply");
  assert_eq!(text.len(), (1 << 20) - 1, "cut to the last whole character");
  assert!(
    logs.contains("ipc big.text's reply is over 1 MiB; it is cut"),
    "{logs}"
  );
}

#[test]
fn a_seventeenth_connection_is_refused_until_one_closes() {
  let mut server = server("many");
  server.register("bar", "hide", says("hidden"));
  let mut open: Vec<UnixStream> = (0..16).map(|_| server.connect()).collect();
  server.turn();
  let mut reply = String::new();
  let logs = capture_logs(|| reply = server.ask(&call("bar", "hide")));
  assert_eq!(
    reply,
    error("the shell has 16 IPC connections open already")
  );
  assert!(logs.contains("refused an IPC connection"), "{logs}");
  // One hangs up without asking anything: the next one is answered.
  drop(open.pop());
  server.turn();
  assert_eq!(server.ask(&call("bar", "hide")), ok("hidden"));
}

#[test]
fn a_connection_without_a_whole_request_is_closed_after_5_s() {
  let mut server = server("slow");
  server.register("bar", "hide", says(""));
  let mut client = server.connect();
  client.write_all(br#"{"target":"bar","#).expect("sent");
  let started = Instant::now();
  assert_eq!(server.read_to_close(&mut client), "");
  let waited = started.elapsed();
  assert!(
    waited >= ipc::TIMEOUT && waited < ipc::TIMEOUT + Duration::from_secs(1),
    "{waited:?}"
  );
}

#[test]
fn an_idle_socket_wakes_nothing() {
  let mut server = server("idle");
  server.register("bar", "hide", says(""));
  // One call opens a connection and its timer, and closes both.
  assert_eq!(server.ask(&call("bar", "hide")), ok(""));
  let slept = server.sleep(Duration::from_secs(6));
  assert!(slept >= Duration::from_millis(5500), "woke after {slept:?}");
  // A connection waiting for its request doesn't spin either.
  let _waiting = server.connect();
  server.turn();
  server.turn();
  let slept = server.sleep(Duration::from_secs(1));
  assert!(slept >= Duration::from_millis(900), "woke after {slept:?}");
}

#[test]
fn a_newer_registration_outlives_the_older_ones_owner() {
  let runtime = Runtime::current();
  let mut server = server("replace");
  let nodes = runtime.node_count();
  let (older, newer) = (server.cx.child(), server.cx.child());
  register(&server.ipc, &older, "bar", "hide", says("older"));
  let logs = capture_logs(|| register(&server.ipc, &newer, "bar", "hide", says("newer")));
  assert!(
    logs.contains("WARN") && logs.contains("ipc bar.hide (registered at "),
    "{logs}"
  );
  assert!(logs.contains("replaces the one registered at "), "{logs}");
  older.owner().dispose();
  assert_eq!(server.ask(&call("bar", "hide")), ok("newer"));
  newer.owner().dispose();
  assert_eq!(
    server.ask(&call("bar", "hide")),
    error("no IPC handler bar.hide"),
    "it goes with its owner"
  );
  assert_eq!(server.ask(LIST), ok(""));
  assert_eq!(runtime.node_count(), nodes);
}

#[test]
fn bad_names_are_refused_with_the_call_site() {
  let mut server = server("names");
  let line = line!() + 2;
  let logs = capture_logs(|| {
    server.register("two words", "x", says(""));
    server.register("bar", "", says(""));
  });
  assert!(
    logs.contains(&format!("cx.ipc at {}:{line}:", file!())),
    "{logs}"
  );
  assert!(logs.contains("\"two words\" isn't a target name"), "{logs}");
  assert!(logs.contains("\"\" isn't a function name"), "{logs}");
  server.turn();
  assert!(
    !server.path.exists(),
    "nothing registered, so nothing bound"
  );
}

#[test]
fn the_socket_replaces_a_file_and_is_removed_at_drop_only_if_ours() {
  let older = server("ours");
  std::fs::create_dir_all(older.path.parent().expect("a directory")).expect("made");
  std::fs::write(&older.path, "stale").expect("written");
  let logs = capture_logs(|| older.register("bar", "hide", says("")));
  assert!(logs.contains("INFO") && logs.contains("replaced"), "{logs}");
  let inode = |path: &Path| {
    let meta = std::fs::symlink_metadata(path).expect("the socket file");
    (meta.dev(), meta.ino())
  };
  let first = inode(&older.path);

  // A newer shell at the same path takes it over.
  let newer_loop = calloop::EventLoop::<()>::try_new().expect("a loop");
  let newer = Ipc::new(&newer_loop.handle(), Some(older.path.clone()));
  register(&newer, &older.cx, "bar", "show", says(""));
  let second = inode(&older.path);
  assert_ne!(first, second);

  let Server {
    event_loop,
    ipc,
    path,
    cx,
    _dir,
  } = older;
  cx.owner().dispose();
  drop((event_loop, ipc));
  assert_eq!(
    inode(&path),
    second,
    "the older shell leaves the newer one's socket"
  );
  drop((newer_loop, newer));
  assert!(!path.exists(), "the newer shell removes its own");
}

#[test]
fn without_a_runtime_dir_it_logs_once_and_serves_nothing() {
  let event_loop = calloop::EventLoop::<()>::try_new().expect("a loop");
  let ipc = Ipc::new(&event_loop.handle(), None);
  let cx = Cx::new(Runtime::current().new_owner());
  let logs = capture_logs(|| {
    register(&ipc, &cx, "bar", "hide", says(""));
    register(&ipc, &cx, "bar", "show", says(""));
  });
  assert_eq!(logs.matches("no IPC socket").count(), 1, "{logs}");
  cx.owner().dispose();
}

#[test]
fn a_panicking_handler_is_logged_and_disabled_and_the_rest_still_answer() {
  let mut server = server("panic");
  let line = line!() + 1;
  server.register("launcher", "toggle", |_: &[String]| panic!("no luck"));
  // A handler that registers another: no borrow is held across a call.
  let (weak, cx) = (Rc::downgrade(&server.ipc), server.cx.clone());
  server.register("bar", "grow", move |_: &[String]| {
    register(
      &weak.upgrade().expect("the server"),
      &cx,
      "bar",
      "grown",
      says("yes"),
    );
    Ok(String::from("grew"))
  });
  let mut reply = String::new();
  let logs = capture_logs(|| reply = server.ask(CALL));
  assert_eq!(reply, error("launcher.toggle panicked: no luck"));
  let logged = format!("ipc launcher.toggle (registered at {}:{line}:", file!());
  assert!(
    logs.contains(&logged) && logs.contains(") panicked: no luck"),
    "{logs}"
  );
  assert_eq!(
    server.ask(CALL),
    error("launcher.toggle panicked before and is disabled")
  );
  assert_eq!(server.ask(&call("bar", "grow")), ok("grew"));
  assert_eq!(server.ask(&call("bar", "grown")), ok("yes"));
}
