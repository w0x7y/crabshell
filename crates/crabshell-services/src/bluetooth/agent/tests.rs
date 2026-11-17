use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use zbus::{Connection, Message, ObjectPath};

use super::{AGENT, AGENT_PATH, BLUEZ, BluetoothAgent, CANCELED, PairingKind, REJECTED};
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

const HEADSET: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// BlueZ's answer to a call, once it came: the reply, or the error's name.
type Answer = Rc<RefCell<Option<Result<Message, String>>>>;

/// Each `RegisterAgent`'s sender, path and capability.
type Registrations = Rc<RefCell<Vec<(String, String, String)>>>;

/// A shell and a fake BlueZ on a private system bus, which records the
/// agent manager's calls.
struct Bluez {
  shell: Harness,
  bus: PrivateBus,
  fake: FakeService,
  registrations: Registrations,
}

impl Bluez {
  fn start() -> Bluez {
    let bus = PrivateBus::start();
    let mut shell = Harness::new(Config {
      system_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
      ..offline()
    });
    let (fake, registrations) = fake_bluez(&mut shell, &bus);
    Bluez {
      shell,
      bus,
      fake,
      registrations,
    }
  }

  /// `cx.bluetooth_agent()`, once BlueZ took it, and its unique name.
  fn agent(&mut self) -> (BluetoothAgent, String) {
    let agent = self.shell.cx.bluetooth_agent();
    let service = Rc::clone(agent.service.as_ref().expect("the shell's agent"));
    self.wait("the registration", move || {
      let state = service.state.borrow();
      state.as_ref().is_some_and(|state| state.bluez.is_some())
    });
    let name = self.registrations.borrow()[0].0.clone();
    (agent, name)
  }

  fn wait(&mut self, what: &str, done: impl Fn() -> bool) {
    self.shell.retry_until(what, done, |_| {});
  }

  /// Calls `member` on `agent` from `from`, as BlueZ does.
  fn ask_from<B>(&mut self, from: &Connection, agent: &str, member: &'static str, body: B) -> Answer
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let (from, agent) = (from.clone(), agent.to_owned());
    let answer = Answer::default();
    let sink = Rc::clone(&answer);
    self.shell.spawn(async move {
      let call = from.call_method(Some(agent.as_str()), AGENT_PATH, Some(AGENT), member, &body);
      *sink.borrow_mut() = Some(call.await.map_err(error_name));
    });
    answer
  }

  fn ask<B>(&mut self, agent: &str, member: &'static str, body: B) -> Answer
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let from = self.fake.connection.clone();
    self.ask_from(&from, agent, member, body)
  }

  /// The answer, once the agent gave it.
  fn answered(&mut self, answer: &Answer) -> Result<Message, String> {
    let done = Rc::clone(answer);
    self.wait("the agent's answer", move || done.borrow().is_some());
    answer.borrow_mut().take().expect("an answer")
  }

  /// Waits for the request shown to be `kind`.
  fn wait_kind(&mut self, agent: &BluetoothAgent, kind: PairingKind) {
    let request = agent.request();
    self.wait("the request", move || {
      request
        .get_untracked()
        .is_some_and(|shown| shown.kind == kind)
    });
  }

  /// The agent manager's calls, as `member path`, but the registrations.
  fn manager_calls(&self) -> Vec<String> {
    let calls = self.fake.calls.borrow();
    let others = calls
      .iter()
      .filter(|call| !call.starts_with("RegisterAgent"));
    others.cloned().collect()
  }
}

/// A fake BlueZ on `bus` that takes every registration, recording it.
fn fake_bluez(shell: &mut Harness, bus: &PrivateBus) -> (FakeService, Registrations) {
  let fake = FakeService::start(shell, bus, BLUEZ);
  let registrations = Registrations::default();
  let sink = Rc::clone(&registrations);
  fake.on("RegisterAgent", move |call| {
    let body = call.body();
    let (path, capability) = body.deserialize::<(ObjectPath<'_>, String)>().ok()?;
    let sender = call.header().sender()?.to_string();
    sink
      .borrow_mut()
      .push((sender, path.to_string(), capability));
    reply(call, &())
  });
  for member in ["RequestDefaultAgent", "UnregisterAgent"] {
    fake.on(member, |call| reply(call, &()));
  }
  fake.own(shell);
  (fake, registrations)
}

fn error_name(error: zbus::Error) -> String {
  use zbus::DBusError;

  match error {
    zbus::Error::MethodError(name, _, _) => name.to_string(),
    zbus::Error::FDO(error) => error.name().to_string(),
    other => other.to_string(),
  }
}

fn device() -> ObjectPath<'static> {
  ObjectPath::from_static_str(HEADSET).expect("a path")
}

fn body<T: serde::de::DeserializeOwned + zbus::zvariant::Type>(
  answer: Result<Message, String>,
) -> T {
  let reply = answer.expect("a reply");
  reply.body().deserialize::<T>().expect("its body")
}

#[test]
fn the_agent_registers_as_keyboard_display_on_the_first_call_only() {
  let mut bluez = Bluez::start();
  let (agent, _) = bluez.agent();
  let again = bluez.shell.cx.child().bluetooth_agent();
  bluez
    .shell
    .run_until(Instant::now() + Duration::from_millis(200));
  let registrations = bluez.registrations.borrow().clone();
  assert_eq!(registrations.len(), 1, "{registrations:?}");
  assert_eq!(registrations[0].1, AGENT_PATH);
  assert_eq!(registrations[0].2, "KeyboardDisplay");
  assert_eq!(agent.request().get_untracked(), None);
  assert_eq!(again.request().get_untracked(), None);
  assert!(bluez.manager_calls().is_empty(), "not the default unasked");

  agent.make_default();
  let calls = Rc::clone(&bluez.fake.calls);
  bluez.wait("the default", move || {
    calls
      .borrow()
      .iter()
      .any(|call| call == "RequestDefaultAgent /org/bluez")
  });
}

#[test]
fn each_question_shows_and_its_answer_reaches_bluez() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();

  let answer = bluez.ask(&name, "RequestPinCode", device());
  bluez.wait_kind(&agent, PairingKind::PinCode);
  assert_eq!(
    agent.request().get_untracked().expect("shown").device,
    HEADSET
  );
  agent.answer_pin_code("0000");
  assert_eq!(agent.request().get_untracked(), None, "gone once answered");
  assert_eq!(body::<String>(bluez.answered(&answer)), "0000");

  let answer = bluez.ask(&name, "RequestPasskey", device());
  bluez.wait_kind(&agent, PairingKind::Passkey);
  agent.answer_passkey(12_345);
  assert_eq!(body::<u32>(bluez.answered(&answer)), 12_345);

  let answer = bluez.ask(&name, "RequestConfirmation", (device(), 654_321_u32));
  bluez.wait_kind(&agent, PairingKind::Confirmation { passkey: 654_321 });
  agent.accept();
  assert!(bluez.answered(&answer).is_ok());

  let answer = bluez.ask(&name, "RequestAuthorization", device());
  bluez.wait_kind(&agent, PairingKind::Authorization);
  agent.reject();
  assert_eq!(bluez.answered(&answer).map(drop), Err(REJECTED.to_owned()));

  let uuid = "0000110b-0000-1000-8000-00805f9b34fb";
  let answer = bluez.ask(&name, "AuthorizeService", (device(), uuid));
  let uuid = uuid.to_owned();
  bluez.wait_kind(&agent, PairingKind::Service { uuid });
  agent.accept();
  assert!(bluez.answered(&answer).is_ok());
}

#[test]
fn a_code_to_show_is_answered_at_once_and_shows_until_bluez_cancels() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let answer = bluez.ask(&name, "DisplayPinCode", (device(), "123456"));
  assert!(bluez.answered(&answer).is_ok(), "answered at once");
  let pin_code = String::from("123456");
  bluez.wait_kind(&agent, PairingKind::DisplayPinCode { pin_code });
  let answer = bluez.ask(&name, "Cancel", ());
  assert!(bluez.answered(&answer).is_ok());
  assert_eq!(agent.request().get_untracked(), None);

  // A passkey's typed count goes up in the same request.
  let answer = bluez.ask(&name, "DisplayPasskey", (device(), 42_u32, 0_u16));
  assert!(bluez.answered(&answer).is_ok());
  let first = PairingKind::DisplayPasskey {
    passkey: 42,
    entered: 0,
  };
  bluez.wait_kind(&agent, first);
  let id = agent.request().get_untracked().expect("shown").id;
  let answer = bluez.ask(&name, "DisplayPasskey", (device(), 42_u32, 3_u16));
  assert!(bluez.answered(&answer).is_ok());
  let typed = PairingKind::DisplayPasskey {
    passkey: 42,
    entered: 3,
  };
  bluez.wait_kind(&agent, typed);
  assert_eq!(agent.request().get_untracked().expect("shown").id, id);
  agent.reject();
  assert_eq!(agent.request().get_untracked(), None, "hidden");
}

#[test]
fn bluez_cancelling_ends_the_question_unanswered() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let answer = bluez.ask(&name, "RequestPasskey", device());
  bluez.wait_kind(&agent, PairingKind::Passkey);
  let cancel = bluez.ask(&name, "Cancel", ());
  assert!(bluez.answered(&cancel).is_ok());
  assert_eq!(bluez.answered(&answer).map(drop), Err(CANCELED.to_owned()));
  assert_eq!(agent.request().get_untracked(), None);
  let logs = capture_logs(|| agent.answer_passkey(1));
  assert!(logs.contains("no Bluetooth request takes it"), "{logs}");
}

#[test]
fn a_new_question_cancels_the_one_unanswered() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let first = bluez.ask(&name, "RequestPinCode", device());
  bluez.wait_kind(&agent, PairingKind::PinCode);
  let second = bluez.ask(&name, "RequestAuthorization", device());
  assert_eq!(bluez.answered(&first).map(drop), Err(CANCELED.to_owned()));
  bluez.wait_kind(&agent, PairingKind::Authorization);
  agent.accept();
  assert!(bluez.answered(&second).is_ok());
}

#[test]
fn answers_that_dont_fit_are_dropped_and_logged() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let answer = bluez.ask(&name, "RequestPinCode", device());
  bluez.wait_kind(&agent, PairingKind::PinCode);
  for pin in ["", "12345678901234567", "12 34", "ü"] {
    let logs = capture_logs(|| agent.answer_pin_code(pin));
    assert!(
      logs.contains("1 to 16 letters and digits"),
      "{pin:?}: {logs}"
    );
  }
  let logs = capture_logs(|| {
    agent.answer_passkey(7);
    agent.accept();
  });
  assert_eq!(
    logs.matches("no Bluetooth request takes it").count(),
    2,
    "{logs}"
  );
  assert!(answer.borrow().is_none(), "BlueZ still waits");
  assert!(agent.request().get_untracked().is_some(), "still shown");

  let logs = capture_logs(|| agent.answer_passkey(1_000_000));
  assert!(logs.contains("six digits"), "{logs}");
  agent.answer_pin_code("abc123");
  assert_eq!(body::<String>(bluez.answered(&answer)), "abc123");
}

#[test]
fn only_bluez_may_call_the_agent() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let other = bluez.shell.block_on(bluez.bus.connect(&[]));
  let answer = bluez.ask_from(&other, &name, "RequestAuthorization", device());
  assert_eq!(
    bluez.answered(&answer).map(drop),
    Err(ACCESS_DENIED.to_owned())
  );
  assert_eq!(agent.request().get_untracked(), None);
  // Bad arguments and unknown methods from BlueZ itself.
  let answer = bluez.ask(&name, "RequestPinCode", "not a path");
  assert!(bluez.answered(&answer).is_err());
  let answer = bluez.ask(&name, "Frobnicate", ());
  assert!(bluez.answered(&answer).is_err());
  assert_eq!(agent.request().get_untracked(), None);
}

#[test]
fn disposing_the_root_rejects_the_question_and_unregisters() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let answer = bluez.ask(&name, "RequestConfirmation", (device(), 1_u32));
  bluez.wait_kind(&agent, PairingKind::Confirmation { passkey: 1 });
  let service = Rc::downgrade(agent.service.as_ref().expect("the shell's agent"));
  drop(agent);
  bluez.shell.cx.owner().dispose();
  assert_eq!(bluez.answered(&answer).map(drop), Err(REJECTED.to_owned()));
  let calls = Rc::clone(&bluez.fake.calls);
  bluez.wait("the agent unregistered", move || {
    calls
      .borrow()
      .iter()
      .any(|call| call == "UnregisterAgent /org/bluez")
  });
  let state = service.upgrade().expect("provided");
  assert!(
    state
      .state
      .borrow()
      .as_ref()
      .expect("started")
      .current
      .is_none()
  );
}

#[test]
fn a_bluez_that_starts_again_gets_the_agent_again() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  agent.make_default();
  let _old = bluez.ask(&name, "RequestPasskey", device());
  bluez.wait_kind(&agent, PairingKind::Passkey);
  let old = bluez.fake.connection.clone();
  bluez.shell.block_on(async move {
    let bus = zbus::fdo::DBusProxy::new(&old).await.expect("the bus");
    let name = BLUEZ.try_into().expect("a bus name");
    bus.release_name(name).await.expect("released");
  });
  let (new, registrations) = fake_bluez(&mut bluez.shell, &bluez.bus);
  let again = Rc::clone(&registrations);
  bluez.wait("the registration with the new BlueZ", move || {
    again.borrow().len() == 1
  });
  assert_eq!(agent.request().get_untracked(), None, "the old one went");
  let calls = Rc::clone(&new.calls);
  bluez.wait("the default again", move || {
    calls
      .borrow()
      .iter()
      .any(|call| call == "RequestDefaultAgent /org/bluez")
  });
  bluez.fake = new;
  let answer = bluez.ask(&name, "RequestAuthorization", device());
  bluez.wait_kind(&agent, PairingKind::Authorization);
  agent.accept();
  assert!(bluez.answered(&answer).is_ok());
}

#[test]
fn an_idle_agent_wakes_nothing() {
  let mut bluez = Bluez::start();
  let (agent, name) = bluez.agent();
  let quiet = |bluez: &mut Bluez, what: &str| {
    bluez
      .shell
      .run_until(Instant::now() + Duration::from_millis(200));
    let woke = bluez
      .shell
      .run_until(Instant::now() + Duration::from_secs(1));
    assert_eq!(woke, 1, "{what}");
  };
  quiet(&mut bluez, "registered, nothing asked");
  let answer = bluez.ask(&name, "RequestPasskey", device());
  bluez.wait_kind(&agent, PairingKind::Passkey);
  quiet(&mut bluez, "a question waits for the user");
  agent.answer_passkey(1);
  assert!(bluez.answered(&answer).is_ok());
  quiet(&mut bluez, "after a request");
}

#[test]
fn without_a_bus_nothing_arrives() {
  let shell = Harness::new(offline());
  let agent = shell.cx.bluetooth_agent();
  assert_eq!(agent.request().get_untracked(), None);
  let logs = capture_logs(|| agent.accept());
  assert!(logs.contains("no Bluetooth request takes it"), "{logs}");
}

/// BlueZ asks the agent of whoever called `Pair`, so a pairing the shell
/// starts goes out on the agent's connection once there is one.
#[test]
fn a_pairing_the_shell_starts_reaches_its_agent() {
  let mut bluez = Bluez::start();
  let pairs = Rc::<RefCell<Vec<String>>>::default();
  let sink = Rc::clone(&pairs);
  bluez.fake.on("Pair", move |call| {
    sink.borrow_mut().push(call.header().sender()?.to_string());
    reply(call, &())
  });
  let bluetooth = bluez.shell.cx.bluetooth();
  let service = Rc::clone(bluetooth.service.as_ref().expect("the shell's Bluetooth"));
  bluez.wait("BlueZ read", move || {
    let state = service.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.connection.is_some())
  });
  bluetooth.pair(HEADSET);
  let sent = Rc::clone(&pairs);
  bluez.wait("the first pairing", move || sent.borrow().len() == 1);

  let (agent, name) = bluez.agent();
  assert_ne!(
    pairs.borrow()[0],
    name,
    "no agent yet: from the service's own"
  );
  bluetooth.pair(HEADSET);
  let sent = Rc::clone(&pairs);
  bluez.wait("the second pairing", move || sent.borrow().len() == 2);
  let sender = pairs.borrow()[1].clone();
  assert_eq!(sender, name, "from the connection the agent registered on");
  // BlueZ, routing by sender, asks that agent.
  let answer = bluez.ask(&sender, "RequestConfirmation", (device(), 7_u32));
  bluez.wait_kind(&agent, PairingKind::Confirmation { passkey: 7 });
  agent.accept();
  assert!(bluez.answered(&answer).is_ok());
}
