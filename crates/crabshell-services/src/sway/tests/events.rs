//! Raw events for a config's handlers, against the fake Sway.

use std::cell::RefCell;

use super::*;
use crate::sway::ipc::GET_OUTPUTS;
use crate::sway::{SwayEvent, SwayEventKind};

const MODE_EVENT: u32 = 0x8000_0002;
const WINDOW_EVENT: u32 = 0x8000_0003;

/// A handler that keeps each event it hears, and what it heard.
fn recorder() -> (impl Fn(&SwayEvent), Rc<RefCell<Vec<SwayEvent>>>) {
  let heard = Rc::new(RefCell::new(Vec::new()));
  let keep = Rc::clone(&heard);
  (
    move |event: &SwayEvent| keep.borrow_mut().push(event.clone()),
    heard,
  )
}

fn event(kind: SwayEventKind, data: &str) -> SwayEvent {
  SwayEvent {
    kind,
    data: data.to_owned(),
  }
}

#[test]
fn each_kind_has_its_own_code_and_name() {
  for kind in SwayEventKind::ALL {
    let (code, name) = kind.wire();
    assert_eq!(SwayEventKind::from_code(code), Some(kind), "{name}");
    let same_name = SwayEventKind::ALL
      .iter()
      .filter(|other| other.wire().1 == name);
    assert_eq!(same_name.count(), 1, "{name}");
  }
  assert_eq!(SwayEventKind::from_code(GET_WORKSPACES), None);
}

#[test]
fn a_listener_subscribes_to_its_kinds_alone_and_hears_their_events() {
  let fake = FakeSway::new();
  let (mut shell, mut peer) = Harness::connect(&fake);
  let sway = shell.cx.sway();
  let (first, heard) = recorder();
  sway.on_event(&[SwayEventKind::Mode, SwayEventKind::Window], first);
  assert_eq!(peer.read(), (SUBSCRIBE, r#"["mode","window"]"#.to_owned()));
  peer.send(SUBSCRIBE, r#"{"success":true}"#);
  // Kinds already asked for aren't asked for again.
  let (second, also_heard) = recorder();
  sway.on_event(&[SwayEventKind::Mode], second);
  let (third, workspace_heard) = recorder();
  sway.on_event(&[SwayEventKind::Workspace], third);
  peer.read_nothing();

  let mode = r#"{"change":"resize","pango_markup":false}"#;
  peer.send(MODE_EVENT, mode);
  peer.send(WINDOW_EVENT, r#"{"change":"focus"}"#);
  shell.turn();
  let mode = event(SwayEventKind::Mode, mode);
  let window = event(SwayEventKind::Window, r#"{"change":"focus"}"#);
  assert_eq!(*heard.borrow(), [mode.clone(), window]);
  assert_eq!(*also_heard.borrow(), [mode]);

  // A workspace listener hears the event, and the list is fetched too.
  peer.send_event(&mut shell);
  assert_eq!(
    *workspace_heard.borrow(),
    [event(SwayEventKind::Workspace, "{}")]
  );
  peer.read_nothing();
}

#[test]
fn a_bad_event_is_logged_and_skipped() {
  let logs = capture_logs(|| {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let (handler, heard) = recorder();
    shell.cx.sway().on_event(&[SwayEventKind::Mode], handler);
    peer.read();
    let mut bytes = Vec::new();
    ipc::encode(MODE_EVENT, b"{not json", &mut bytes);
    ipc::encode(MODE_EVENT, b"\"\xff\"", &mut bytes);
    ipc::encode(0x8000_0099, b"{}", &mut bytes);
    ipc::encode(MODE_EVENT, br#"{"change":"default"}"#, &mut bytes);
    peer.0.write_all(&bytes).expect("a write to the service");
    shell.turn();
    let default = event(SwayEventKind::Mode, r#"{"change":"default"}"#);
    assert_eq!(*heard.borrow(), [default]);
  });
  assert_eq!(logs.matches("a bad event from Sway").count(), 2, "{logs}");
}

#[test]
fn a_panicking_listener_is_logged_and_dropped_and_the_rest_go_on() {
  let mut at = String::new();
  let logs = capture_logs(|| {
    let fake = FakeSway::new();
    let (mut shell, mut peer) = Harness::connect(&fake);
    let sway = shell.cx.sway();
    let runs = Rc::new(Cell::new(0));
    let counted = Rc::clone(&runs);
    at = format!("{}:{}", file!(), line!() + 1);
    sway.on_event(&[SwayEventKind::Mode], move |_| {
      counted.set(counted.get() + 1);
      panic!("a bad binding");
    });
    peer.read();
    // A handler may call the service back: nothing is borrowed.
    let calls = sway.clone();
    let (record, heard) = recorder();
    sway.on_event(&[SwayEventKind::Mode], move |event| {
      calls.dispatch("mode default");
      calls.outputs();
      record(event);
    });

    peer.send(MODE_EVENT, r#"{"change":"resize"}"#);
    peer.send(MODE_EVENT, r#"{"change":"default"}"#);
    shell.turn();
    assert_eq!(runs.get(), 1, "not called after its panic");
    assert_eq!(heard.borrow().len(), 2);
    assert_eq!(peer.read(), (RUN_COMMAND, "mode default".to_owned()));
    assert_eq!(peer.read(), (SUBSCRIBE, r#"["output"]"#.to_owned()));
    peer.read();
    assert_eq!(peer.read(), (RUN_COMMAND, "mode default".to_owned()));

    // The workspaces go on too.
    peer.send_event(&mut shell);
    assert_eq!(peer.read(), (GET_OUTPUTS, String::new()));
    peer.send(GET_WORKSPACES, &two().0);
    shell.turn();
    assert_eq!(sway.workspaces().get_untracked(), two().1);
  });
  let logged = format!("Sway on_event (called at {at}");
  assert_eq!(logs.matches(&logged).count(), 1, "{logs}");
  assert!(logs.contains("panicked: a bad binding"), "{logs}");
}

#[test]
fn a_listener_goes_with_its_context_and_its_kind_with_the_next_event() {
  let runtime = Runtime::current();
  let fake = FakeSway::new();
  let (mut shell, mut peer) = Harness::connect(&fake);
  // An event never asked for, as from a compositor that sends them anyway,
  // brings no new connection: there is nothing to drop.
  peer.send(0x8000_0007, r#"{"first":true,"payload":""}"#);
  shell.turn();
  peer.read_nothing();
  assert!(fake.try_accept().is_none());

  let nodes = runtime.node_count();
  let output = shell.cx.child();
  let (handler, heard) = recorder();
  output.sway().on_event(&[SwayEventKind::Window], handler);
  assert_eq!(peer.read(), (SUBSCRIBE, r#"["window"]"#.to_owned()));
  output.owner().dispose();
  assert_eq!(runtime.node_count(), nodes);
  peer.read_nothing();

  // Sway can't unsubscribe, so the next window event, which no handler
  // wants, brings a new connection without it at once.
  peer.send(WINDOW_EVENT, r#"{"change":"focus"}"#);
  shell.turn();
  peer.read_end();
  let mut peer = fake.accept();
  peer.read_opening();
  assert!(heard.borrow().is_empty());
  assert_eq!(
    shell.cx.sway().workspaces().get_untracked(),
    one().1,
    "the list stays"
  );
  peer.send(SUBSCRIBE, r#"{"success":true}"#);
  peer.send(GET_WORKSPACES, &one().0);
  shell.turn();
  peer.read_nothing();
}
