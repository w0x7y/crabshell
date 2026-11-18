use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use serde_json::json;

use super::{
  MAX_ACTIONS, MAX_LINE, MAX_LIST, MAX_REQUEST, Niri, NiriKeyboardLayouts, NiriWindow,
  NiriWorkspace,
};
use crate::hyprland::MAX_TEXT;
use crate::testing::{Harness, capture_logs, offline};
use crate::{Config, ServicesExt};

/// A stand-in for niri: its socket in a short temporary directory, since
/// socket paths are capped at 108 bytes. The directory goes on drop.
struct FakeNiri {
  dir: PathBuf,
  listener: UnixListener,
}

impl FakeNiri {
  fn new() -> FakeNiri {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "crabshell-niri-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let listener = UnixListener::bind(dir.join("niri.sock")).expect("a socket");
    listener
      .set_nonblocking(true)
      .expect("a non-blocking socket");
    FakeNiri { dir, listener }
  }

  fn config(&self) -> Config {
    Config {
      niri: Some(self.dir.join("niri.sock")),
      ..offline()
    }
  }

  /// The next connection the shell made, with the request line it sent.
  /// The shell writes it as it connects.
  fn accept(&self) -> Option<(String, UnixStream)> {
    let stream = match self.listener.accept() {
      Ok((stream, _)) => stream,
      Err(error) if error.kind() == ErrorKind::WouldBlock => return None,
      Err(error) => panic!("accept failed: {error}"),
    };
    stream.set_nonblocking(false).expect("a blocking stream");
    let timeout = Some(Duration::from_secs(1));
    stream.set_read_timeout(timeout).expect("a read timeout");
    let mut request = String::new();
    BufReader::new(&stream)
      .read_line(&mut request)
      .expect("a request");
    Some((request, stream))
  }

  /// The shell's event stream, answered and sent `state`.
  fn stream(&self, state: &str) -> UnixStream {
    let (request, mut stream) = self.accept().expect("an event stream");
    assert_eq!(request, "\"EventStream\"\n");
    stream
      .write_all(format!("{{\"Ok\":\"Handled\"}}\n{state}").as_bytes())
      .expect("the state");
    stream
  }

  /// Every action the shell sent, each answered with `reply`.
  fn actions(&self, reply: &str) -> Vec<String> {
    std::iter::from_fn(|| self.accept())
      .map(|(request, mut stream)| {
        stream
          .write_all(format!("{reply}\n").as_bytes())
          .expect("a reply");
        request
      })
      .collect()
  }
}

impl Drop for FakeNiri {
  fn drop(&mut self) {
    let _ = std::fs::remove_dir_all(&self.dir);
  }
}

/// A window as niri writes it, with fields the service ignores.
fn window_json(id: u64, title: &str, workspace: u64, focused: bool) -> String {
  format!(
    r#"{{"id":{id},"title":"{title}","app_id":"foot","pid":10,"workspace_id":{workspace},"is_focused":{focused},"is_floating":false,"is_urgent":false,"layout":{{"pos_in_scrolling_layout":[1,1],"tile_size":[800.0,600.0],"window_size":[796,596],"tile_pos_in_workspace_view":null,"window_offset_in_tile":[2.0,2.0]}},"focus_timestamp":{{"secs":5,"nanos":0}}}}"#
  )
}

/// niri's first events, in its own order: DP-1 shows 1, which has the
/// focus, and 2 is hidden there; HDMI-A-1 shows 3.
fn state() -> String {
  [
    r#"{"WorkspacesChanged":{"workspaces":[
      {"id":3,"idx":1,"name":null,"output":"HDMI-A-1","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":null},
      {"id":2,"idx":2,"name":"web","output":"DP-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":12},
      {"id":1,"idx":1,"name":null,"output":"DP-1","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":11}
    ]}}"#
      .replace('\n', ""),
    format!(
      r#"{{"WindowsChanged":{{"windows":[{},{}]}}}}"#,
      window_json(12, "firefox", 2, false),
      window_json(11, "vim", 1, true)
    ),
    r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["English (US)","Hebrew"],"current_idx":0}}}"#.to_owned(),
    r#"{"OverviewOpenedOrClosed":{"is_open":false}}"#.to_owned(),
    r#"{"ConfigLoaded":{"failed":false}}"#.to_owned(),
    String::new(),
  ]
  .join("\n")
}

fn workspace(id: u64, idx: u32, output: &str, active: bool, focused: bool) -> NiriWorkspace {
  NiriWorkspace {
    id,
    idx,
    name: (id == 2).then(|| "web".to_owned()),
    output: Some(output.to_owned()),
    is_active: active,
    is_focused: focused,
    is_urgent: false,
    active_window_id: match id {
      1 => Some(11),
      2 => Some(12),
      _ => None,
    },
  }
}

fn expected_workspaces() -> Vec<NiriWorkspace> {
  vec![
    workspace(1, 1, "DP-1", true, true),
    workspace(2, 2, "DP-1", false, false),
    workspace(3, 1, "HDMI-A-1", true, false),
  ]
}

fn window(id: u64, title: &str, workspace: u64, focused: bool) -> NiriWindow {
  NiriWindow {
    id,
    title: Some(title.to_owned()),
    app_id: Some("foot".to_owned()),
    workspace_id: Some(workspace),
    is_focused: focused,
    is_urgent: false,
  }
}

fn expected_windows() -> Vec<NiriWindow> {
  vec![window(11, "vim", 1, true), window(12, "firefox", 2, false)]
}

fn run(shell: &mut Harness) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(30))
}

/// Sends niri's event `line` and lets the shell read it.
fn send(shell: &mut Harness, stream: &mut UnixStream, line: &str) {
  stream
    .write_all(format!("{line}\n").as_bytes())
    .expect("an event");
  run(shell);
}

/// A shell connected to `fake`, its first events read, and the stream's
/// other end.
fn connect(fake: &FakeNiri) -> (Harness, Niri, UnixStream) {
  let mut shell = Harness::new(fake.config());
  let niri = shell.cx.niri();
  let stream = fake.stream(&state());
  // Until read, however busy the machine.
  let workspaces = niri.workspaces();
  let read = || workspaces.get_untracked() == expected_workspaces();
  shell.retry_until("the state", read, |_| {});
  (shell, niri, stream)
}

#[test]
fn the_whole_state_comes_first_and_is_sorted() {
  let fake = FakeNiri::new();
  let (_shell, niri, _stream) = connect(&fake);
  assert_eq!(niri.workspaces().get_untracked(), expected_workspaces());
  assert_eq!(niri.windows().get_untracked(), expected_windows());
  assert_eq!(
    niri.keyboard_layouts().get_untracked(),
    Some(NiriKeyboardLayouts {
      names: vec!["English (US)".to_owned(), "Hebrew".to_owned()],
      current_idx: 0,
    })
  );
  assert!(!niri.overview_open().get_untracked());
}

#[test]
fn each_event_changes_its_signal() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let workspaces = || niri.workspaces().get_untracked();
  let windows = || niri.windows().get_untracked();

  send(
    &mut shell,
    &mut stream,
    r#"{"WorkspaceActivated":{"id":2,"focused":true}}"#,
  );
  let mut expected = expected_workspaces();
  (expected[0].is_active, expected[0].is_focused) = (false, false);
  (expected[1].is_active, expected[1].is_focused) = (true, true);
  assert_eq!(workspaces(), expected, "HDMI-A-1 keeps showing 3");

  send(
    &mut shell,
    &mut stream,
    r#"{"WorkspaceActivated":{"id":3,"focused":false}}"#,
  );
  assert_eq!(
    workspaces(),
    expected,
    "3 was active already and gets no focus"
  );

  let line = r#"{"WorkspaceActiveWindowChanged":{"workspace_id":2,"active_window_id":null}}"#;
  send(&mut shell, &mut stream, line);
  expected[1].active_window_id = None;
  assert_eq!(workspaces(), expected);

  let line = r#"{"WorkspaceUrgencyChanged":{"id":3,"urgent":true}}"#;
  send(&mut shell, &mut stream, line);
  expected[2].is_urgent = true;
  assert_eq!(workspaces(), expected);

  let line = r#"{"WorkspacesChanged":{"workspaces":[{"id":9,"idx":1,"name":null,"output":null,"is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null}]}}"#;
  send(&mut shell, &mut stream, line);
  let only = NiriWorkspace {
    output: None,
    ..workspace(9, 1, "", true, true)
  };
  assert_eq!(workspaces(), [only]);

  let line = format!(
    r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
    window_json(10, "new", 1, true)
  );
  send(&mut shell, &mut stream, &line);
  let mut expected = expected_windows();
  expected[0].is_focused = false;
  expected.insert(0, window(10, "new", 1, true));
  assert_eq!(windows(), expected, "a focused window takes the focus");

  let line = format!(
    r#"{{"WindowOpenedOrChanged":{{"window":{}}}}}"#,
    window_json(12, "renamed", 2, false)
  );
  send(&mut shell, &mut stream, &line);
  expected[2].title = Some("renamed".to_owned());
  assert_eq!(windows(), expected);

  send(
    &mut shell,
    &mut stream,
    r#"{"WindowFocusChanged":{"id":12}}"#,
  );
  expected[0].is_focused = false;
  expected[2].is_focused = true;
  assert_eq!(windows(), expected);

  send(
    &mut shell,
    &mut stream,
    r#"{"WindowUrgencyChanged":{"id":11,"urgent":true}}"#,
  );
  expected[1].is_urgent = true;
  assert_eq!(windows(), expected);

  send(&mut shell, &mut stream, r#"{"WindowClosed":{"id":10}}"#);
  expected.remove(0);
  assert_eq!(windows(), expected);

  send(
    &mut shell,
    &mut stream,
    r#"{"WindowFocusChanged":{"id":null}}"#,
  );
  expected[1].is_focused = false;
  assert_eq!(windows(), expected);

  let line = format!(
    r#"{{"WindowsChanged":{{"windows":[{}]}}}}"#,
    window_json(5, "only", 9, false)
  );
  send(&mut shell, &mut stream, &line);
  assert_eq!(windows(), [window(5, "only", 9, false)]);

  send(
    &mut shell,
    &mut stream,
    r#"{"KeyboardLayoutSwitched":{"idx":1}}"#,
  );
  let layouts = niri.keyboard_layouts().get_untracked().expect("layouts");
  assert_eq!(layouts.current_idx, 1);

  let line =
    r#"{"KeyboardLayoutsChanged":{"keyboard_layouts":{"names":["Greek"],"current_idx":0}}}"#;
  send(&mut shell, &mut stream, line);
  let layouts = niri.keyboard_layouts().get_untracked().expect("layouts");
  assert_eq!(layouts.names, ["Greek"]);

  send(
    &mut shell,
    &mut stream,
    r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
  );
  assert!(niri.overview_open().get_untracked());
}

#[test]
fn unknown_events_are_skipped_and_bad_lines_logged() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let logs = capture_logs(|| {
    send(&mut shell, &mut stream, r#"{"CastsChanged":{"casts":[]}}"#);
    send(
      &mut shell,
      &mut stream,
      r#"{"SomethingNiriAddsLater":{"x":1}}"#,
    );
  });
  assert!(logs.is_empty(), "{logs}");
  let logs = capture_logs(|| {
    stream
      .write_all(b"not json\n{\"WindowClosed\":{\"id\":\"eleven\"}}\n[1]\n")
      .expect("events");
    send(&mut shell, &mut stream, r#"{"WindowClosed":{"id":11}}"#);
  });
  assert!(logs.contains("isn't JSON"), "{logs}");
  assert!(logs.contains("a bad event from niri"), "{logs}");
  assert!(logs.contains("WindowClosed"), "{logs}");
  assert!(logs.contains("not an event"), "{logs}");
  assert_eq!(
    niri.windows().get_untracked(),
    [window(12, "firefox", 2, false)],
    "the stream reads on"
  );
  // About a workspace niri never listed.
  send(
    &mut shell,
    &mut stream,
    r#"{"WorkspaceActivated":{"id":77,"focused":true}}"#,
  );
  assert_eq!(niri.workspaces().get_untracked(), expected_workspaces());
}

#[test]
fn actions_go_as_json_lines_and_a_refusal_is_logged() {
  let fake = FakeNiri::new();
  let (mut shell, niri, _stream) = connect(&fake);
  let logs = capture_logs(|| {
    niri.focus_workspace(3);
    niri.focus_window(11);
    niri.action(json!({ "Spawn": { "command": ["foot"] } }));
    let requests = fake.actions(r#"{"Ok":"Handled"}"#);
    assert_eq!(
      requests,
      [
        "{\"Action\":{\"FocusWorkspace\":{\"reference\":{\"Id\":3}}}}\n",
        "{\"Action\":{\"FocusWindow\":{\"id\":11}}}\n",
        "{\"Action\":{\"Spawn\":{\"command\":[\"foot\"]}}}\n",
      ]
    );
    run(&mut shell);
  });
  assert!(logs.is_empty(), "{logs}");
  let logs = capture_logs(|| {
    niri.action(json!({ "Bogus": {} }));
    fake.actions(r#"{"Err":"error parsing request"}"#);
    run(&mut shell);
    niri.action(json!("x".repeat(MAX_REQUEST)));
  });
  assert!(logs.contains("error parsing request"), "{logs}");
  assert!(logs.contains("too long"), "{logs}");
  assert!(fake.accept().is_none(), "a long action isn't sent");

  let shell = Harness::new(offline());
  let logs = capture_logs(|| shell.cx.niri().focus_window(1));
  assert!(logs.contains("no niri to send an action to"), "{logs}");
}

#[test]
fn past_64_waiting_actions_one_is_refused() {
  let fake = FakeNiri::new();
  let (_shell, niri, _stream) = connect(&fake);
  let logs = capture_logs(|| {
    for id in 0..=MAX_ACTIONS as u64 {
      niri.focus_window(id);
    }
  });
  assert!(logs.contains("64 actions wait for niri"), "{logs}");
  assert_eq!(fake.actions(r#"{"Ok":"Handled"}"#).len(), MAX_ACTIONS);
}

/// Sends a line over the cap on `stream`, and checks the shell drops it
/// for good.
fn assert_over_the_cap_is_lost(
  fake: &FakeNiri,
  shell: &mut Harness,
  niri: &Niri,
  stream: UnixStream,
) {
  let mut stream = stream;
  let writer = std::thread::spawn(move || {
    // The shell hangs up partway, so the write fails, or once it has read
    // the last of it.
    let _ = stream.write_all(&vec![b' '; MAX_LINE + 1]);
    // Done at the hang-up.
    while matches!(stream.read(&mut [0; 1]), Err(error)
      if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut))
    {}
  });
  let logs = capture_logs(|| {
    shell.retry_until("the writer done", || writer.is_finished(), |_| {});
    run(shell);
  });
  writer.join().expect("the writer");
  assert!(logs.contains("over 1 MiB"), "{logs}");
  assert!(logs.contains("lost niri"), "{logs}");
  assert!(niri.workspaces().get_untracked().is_empty());
  assert!(fake.accept().is_none(), "nothing reconnects");
  assert_eq!(run(shell), 1, "nothing retries");
}

#[test]
fn a_line_over_the_cap_loses_the_stream() {
  let fake = FakeNiri::new();
  let (mut shell, niri, stream) = connect(&fake);
  assert_over_the_cap_is_lost(&fake, &mut shell, &niri, stream);
}

#[test]
fn a_state_line_over_the_cap_loses_the_stream() {
  let fake = FakeNiri::new();
  let mut shell = Harness::new(fake.config());
  let niri = shell.cx.niri();
  let (_, mut stream) = fake.accept().expect("an event stream");
  stream
    .write_all(b"{\"Ok\":\"Handled\"}\n")
    .expect("the reply");
  run(&mut shell);
  assert_over_the_cap_is_lost(&fake, &mut shell, &niri, stream);
}

#[test]
fn long_texts_are_cut_at_a_character_and_long_lists_capped() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let long = "é".repeat(MAX_TEXT);
  let windows: Vec<_> = (0..=MAX_LIST as u64)
    .map(|id| {
      let title = if id == 0 { &long } else { "t" };
      format!(r#"{{"id":{id},"title":"{title}","is_focused":false}}"#)
    })
    .collect();
  let line = format!(
    r#"{{"WindowsChanged":{{"windows":[{}]}}}}"#,
    windows.join(",")
  );
  assert!(line.len() < MAX_LINE);
  let writer = std::thread::spawn(move || {
    stream
      .write_all(format!("{line}\n").as_bytes())
      .expect("events");
    stream
  });
  let logs = capture_logs(|| {
    shell.retry_until(
      "the long list",
      || niri.windows().get_untracked().len() == MAX_LIST,
      |_| {},
    );
  });
  let _stream = writer.join().expect("the writer");
  assert!(logs.contains("more windows than the 4096 kept"), "{logs}");
  let windows = niri.windows().get_untracked();
  assert_eq!(
    windows[0].title.as_deref(),
    Some("é".repeat(MAX_TEXT / 2).as_str())
  );
}

/// niri writes its reply and each state event apart, so they may come in
/// reads of their own.
#[test]
fn a_stream_that_closes_after_its_state_in_many_reads_is_opened_again_once() {
  let fake = FakeNiri::new();
  let mut shell = Harness::new(fake.config());
  let niri = shell.cx.niri();
  for connection in 0..2 {
    let (_, mut stream) = fake.accept().expect("an event stream");
    stream
      .write_all(b"{\"Ok\":\"Handled\"}\n")
      .expect("the reply");
    run(&mut shell);
    for line in state().lines() {
      send(&mut shell, &mut stream, line);
    }
    assert_eq!(niri.workspaces().get_untracked(), expected_workspaces());
    drop(stream);
    let logs = capture_logs(|| {
      run(&mut shell);
    });
    let expected = ["connecting again", "lost niri"][connection];
    assert!(logs.contains(expected), "{logs}");
  }
  assert!(niri.workspaces().get_untracked().is_empty());
  assert!(fake.accept().is_none(), "no third connection");
}

/// niri closes an event client that falls behind, and runs on, so its
/// socket is still there.
#[test]
fn a_closed_stream_is_opened_again_once() {
  let fake = FakeNiri::new();
  let (mut shell, niri, stream) = connect(&fake);
  drop(stream);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("connecting again"), "{logs}");
  assert_eq!(
    niri.workspaces().get_untracked(),
    expected_workspaces(),
    "kept meanwhile"
  );
  let mut stream = fake.stream(&state());
  run(&mut shell);

  // After an event past niri's state, a close is followed again.
  send(
    &mut shell,
    &mut stream,
    r#"{"WindowFocusChanged":{"id":null}}"#,
  );
  drop(stream);
  run(&mut shell);
  let stream = fake.stream(&state());
  run(&mut shell);
  assert_eq!(
    niri.windows().get_untracked(),
    expected_windows(),
    "the new state"
  );

  // A stream that ends with nothing past its state isn't.
  drop(stream);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("lost niri"), "{logs}");
  assert!(niri.workspaces().get_untracked().is_empty());
  assert!(niri.windows().get_untracked().is_empty());
  assert!(fake.accept().is_none(), "no third connection");
  assert_eq!(run(&mut shell), 1, "nothing retries");
}

#[test]
fn a_stream_whose_socket_went_is_lost() {
  let fake = FakeNiri::new();
  let (mut shell, niri, stream) = connect(&fake);
  // niri is gone, and its socket with it.
  std::fs::remove_file(fake.dir.join("niri.sock")).expect("the socket file");
  drop(stream);
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("niri closed the event stream"), "{logs}");
  assert!(!logs.contains("connecting again"), "{logs}");
  assert!(niri.workspaces().get_untracked().is_empty());
  assert_eq!(niri.keyboard_layouts().get_untracked(), None);
  assert_eq!(run(&mut shell), 1, "nothing reconnects");
}

#[test]
fn a_refused_stream_is_lost() {
  let fake = FakeNiri::new();
  let mut shell = Harness::new(fake.config());
  let niri = shell.cx.niri();
  let (_, mut stream) = fake.accept().expect("an event stream");
  let answer = format!("{{\"Err\":\"no\"}}\n{}", state());
  stream.write_all(answer.as_bytes()).expect("a reply");
  let logs = capture_logs(|| {
    run(&mut shell);
  });
  assert!(logs.contains("niri refused the event stream"), "{logs}");
  assert!(
    niri.workspaces().get_untracked().is_empty(),
    "nothing after it is read"
  );
  assert!(fake.accept().is_none(), "nothing reconnects");
}

#[test]
fn without_niri_the_lists_stay_empty_and_that_is_logged_once() {
  let shell = Harness::new(offline());
  let logs = capture_logs(|| {
    shell.cx.niri();
  });
  assert!(logs.contains("NIRI_SOCKET is unset"), "{logs}");

  let fake = FakeNiri::new();
  let config = fake.config();
  drop(fake);
  let mut shell = Harness::new(config);
  let logs = capture_logs(|| {
    let niri = shell.cx.niri();
    assert!(niri.workspaces().get_untracked().is_empty());
  });
  assert!(logs.contains("can't connect to niri"), "{logs}");
  let logs = capture_logs(|| {
    shell.cx.niri();
  });
  assert!(logs.is_empty(), "{logs}");
  assert_eq!(run(&mut shell), 1, "nothing retries");
}

#[test]
fn a_quiet_stream_wakes_nothing() {
  let fake = FakeNiri::new();
  let (mut shell, _niri, _stream) = connect(&fake);
  let wakeups = shell.run_until(Instant::now() + Duration::from_millis(500));
  assert_eq!(wakeups, 1, "only the run's own deadline");
}

#[test]
fn every_event_reaches_on_event_after_the_lists() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let heard = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&heard);
  let overview = niri.overview_open();
  niri.on_event(move |event| {
    kept
      .borrow_mut()
      .push((event.clone(), overview.get_untracked()))
  });
  send(
    &mut shell,
    &mut stream,
    r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#,
  );
  send(&mut shell, &mut stream, r#"{"SomethingNew":{}}"#);
  assert_eq!(
    *heard.borrow(),
    [
      (
        json!({ "OverviewOpenedOrClosed": { "is_open": true } }),
        true
      ),
      (json!({ "SomethingNew": {} }), true),
    ]
  );
}

#[test]
fn a_panicking_on_event_is_logged_with_where_it_was_called_and_the_rest_runs() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let panics = Rc::new(Cell::new(0));
  let counted = Rc::clone(&panics);
  niri.on_event(move |_| {
    counted.set(counted.get() + 1);
    panic!("bad event");
  });
  let heard = Rc::new(Cell::new(0));
  let kept = Rc::clone(&heard);
  let inner = niri.clone();
  niri.on_event(move |_| {
    // Nothing of the service is borrowed while a handler runs.
    inner.focus_window(11);
    inner.on_event(|_| {});
    kept.set(kept.get() + 1);
  });
  let logs = capture_logs(|| {
    send(&mut shell, &mut stream, r#"{"WindowClosed":{"id":12}}"#);
  });
  let at = "niri on_event (called at crates/crabshell-services/src/niri/tests.rs:";
  assert!(logs.contains(at), "{logs}");
  assert!(
    logs.contains("panicked on WindowClosed: bad event"),
    "{logs}"
  );
  assert_eq!(heard.get(), 1);
  assert_eq!(fake.actions(r#"{"Ok":"Handled"}"#).len(), 1);
  send(&mut shell, &mut stream, r#"{"WindowClosed":{"id":11}}"#);
  assert_eq!(
    panics.get(),
    1,
    "a handler that panicked isn't called again"
  );
  assert_eq!(heard.get(), 2);
  assert!(
    niri.windows().get_untracked().is_empty(),
    "the stream runs on"
  );
}

#[test]
fn an_on_event_goes_with_its_context() {
  let fake = FakeNiri::new();
  let (mut shell, _niri, mut stream) = connect(&fake);
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let heard = Rc::new(Cell::new(0));
  let kept = Rc::clone(&heard);
  child.niri().on_event(move |_| kept.set(kept.get() + 1));
  send(&mut shell, &mut stream, r#"{"SomethingNew":{}}"#);
  assert_eq!(heard.get(), 1);
  child.owner().dispose();
  assert_eq!(Runtime::current().node_count(), nodes);
  assert_eq!(Rc::strong_count(&heard), 1, "the handler was dropped");

  let child = shell.cx.child();
  let stale = child.niri();
  child.owner().dispose();
  let kept = Rc::clone(&heard);
  let logs = capture_logs(|| stale.on_event(move |_| kept.set(kept.get() + 1)));
  assert!(logs.contains("context that is gone"), "{logs}");
  assert_eq!(Rc::strong_count(&heard), 1, "the handler wasn't kept");
}

#[test]
fn a_layout_index_past_the_names_is_refused() {
  let fake = FakeNiri::new();
  let (mut shell, niri, mut stream) = connect(&fake);
  let names: Vec<_> = (0..=MAX_LIST).map(|n| format!("\"l{n}\"")).collect();
  let capped = format!(
    r#"{{"KeyboardLayoutsChanged":{{"keyboard_layouts":{{"names":[{}],"current_idx":{MAX_LIST}}}}}}}"#,
    names.join(",")
  );
  let logs = capture_logs(|| {
    send(&mut shell, &mut stream, &capped);
    send(
      &mut shell,
      &mut stream,
      r#"{"KeyboardLayoutSwitched":{"idx":2}}"#,
    );
  });
  assert_eq!(
    logs.matches("a layout index past the names").count(),
    2,
    "{logs}"
  );
  let layouts = niri.keyboard_layouts().get_untracked().expect("layouts");
  assert_eq!((layouts.names.len(), layouts.current_idx), (2, 0), "kept");
}
