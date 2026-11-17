//! The shell's own Bluetooth pairing agent: BlueZ's `org.bluez.Agent1`,
//! so a device that asks for a PIN, a passkey or a confirmation can pair
//! from the shell. zbus has no object server here, so BlueZ's calls are
//! answered by hand on the main thread, as the polkit agent's are.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::export::futures_core::Stream;
use zbus::message::{Flags, Type as Kind};
use zbus::{Connection, Message, MessageStream, ObjectPath};

use crate::Shared;
use crate::dbus::{CALL_TIMEOUT, bus_call, owner_changed, owner_rule, set_changed};

const BLUEZ: &str = "org.bluez";
const MANAGER_PATH: &str = "/org/bluez";
const MANAGER: &str = "org.bluez.AgentManager1";
const AGENT: &str = "org.bluez.Agent1";
/// Where the agent answers BlueZ: any path of the shell's own.
pub(crate) const AGENT_PATH: &str = "/org/crabshell/BluetoothAgent";
/// What a shell can do: show a code and take one typed.
const CAPABILITY: &str = "KeyboardDisplay";

const REJECTED: &str = "org.bluez.Error.Rejected";
const CANCELED: &str = "org.bluez.Error.Canceled";
const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

/// The longest PIN code, as Bluetooth allows.
const MAX_PIN: usize = 16;
/// The longest service UUID kept.
const MAX_UUID: usize = 64;
/// The largest passkey: six digits.
const MAX_PASSKEY: u32 = 999_999;

/// What BlueZ asks the shell during a pairing, from
/// [`BluetoothAgent::request`].
#[derive(Clone, Debug, PartialEq)]
pub struct PairingRequest {
  /// Which request this is, so two alike still differ.
  id: u64,
  /// The device's path, as in [`BluetoothDevice::path`], for its name.
  ///
  /// [`BluetoothDevice::path`]: crate::BluetoothDevice::path
  pub device: String,
  /// What BlueZ wants.
  pub kind: PairingKind,
}

/// What a [`PairingRequest`] wants, and the answer it takes.
#[derive(Clone, Debug, PartialEq)]
pub enum PairingKind {
  /// The device's PIN code, for [`BluetoothAgent::answer_pin_code`].
  PinCode,
  /// The passkey the device shows, for
  /// [`BluetoothAgent::answer_passkey`].
  Passkey,
  /// `pin_code` to show, for the user to type on the device. It shows
  /// until BlueZ is done with it; [`accept`](BluetoothAgent::accept) or
  /// [`reject`](BluetoothAgent::reject) only hides it.
  DisplayPinCode {
    /// At most 16 characters.
    pin_code: String,
  },
  /// `passkey` to show, as [`DisplayPinCode`](Self::DisplayPinCode)
  /// shows its code. `entered` is how many digits the device has typed,
  /// and goes up while it shows.
  DisplayPasskey {
    /// Six digits, with leading zeros.
    passkey: u32,
    /// How many digits were typed on the device.
    entered: u16,
  },
  /// Whether `passkey` is the one the device shows, for
  /// [`accept`](BluetoothAgent::accept) or
  /// [`reject`](BluetoothAgent::reject).
  Confirmation {
    /// Six digits, with leading zeros.
    passkey: u32,
  },
  /// Whether the device may pair, as one that asks for nothing does.
  Authorization,
  /// Whether the device may use the service `uuid`, such as audio.
  Service {
    /// The service's UUID, as BlueZ names it.
    uuid: String,
  },
}

/// The shell's Bluetooth pairing agent, from `cx.bluetooth_agent()`. A
/// clone is cheap and shares the one agent.
#[derive(Clone)]
pub struct BluetoothAgent {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  request: ReadSignal<Option<PairingRequest>>,
}

impl BluetoothAgent {
  /// What BlueZ asks now, or `None`. One shows at a time: a new request
  /// takes the place of one unanswered, which BlueZ is told was
  /// cancelled. It goes once answered, or when BlueZ cancels it.
  pub fn request(&self) -> ReadSignal<Option<PairingRequest>> {
    self.request
  }

  /// Answers [`PairingKind::PinCode`] with `pin_code`, 1 to 16 letters
  /// and digits. Any other, or at any other time, it is dropped and
  /// logged.
  pub fn answer_pin_code(&self, pin_code: &str) {
    let fits = (1..=MAX_PIN).contains(&pin_code.len())
      && pin_code.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if !fits {
      return tracing::warn!("a Bluetooth PIN code must be 1 to 16 letters and digits; dropped");
    }
    let pin = matches!(self.kind(), Some(PairingKind::PinCode));
    self.answer(pin, "a PIN code", Ok(&pin_code));
  }

  /// Answers [`PairingKind::Passkey`] with `passkey`, at most 999999. Any
  /// other, or at any other time, it is dropped and logged.
  pub fn answer_passkey(&self, passkey: u32) {
    if passkey > MAX_PASSKEY {
      return tracing::warn!(passkey, "a Bluetooth passkey has six digits; dropped");
    }
    let asked = matches!(self.kind(), Some(PairingKind::Passkey));
    self.answer(asked, "a passkey", Ok(&passkey));
  }

  /// Says yes to a confirmation, an authorization or a service, and
  /// hides a code shown. A PIN code or passkey needs its answer instead.
  pub fn accept(&self) {
    let answerable = self
      .kind()
      .is_some_and(|kind| !matches!(kind, PairingKind::PinCode | PairingKind::Passkey));
    self.answer(answerable, "a yes", Ok(&()));
  }

  /// Says no to a question, so the pairing fails. A code shown is only
  /// hidden: BlueZ took its answer already, and ends the pairing itself.
  pub fn reject(&self) {
    let shown = self.kind().is_some();
    self.answer::<()>(shown, "a no", Err((REJECTED, "the user said no")));
  }

  /// Asks BlueZ to make the shell its default agent, now and whenever
  /// BlueZ starts again, so the shell also answers pairings that other
  /// programs, such as `bluetoothctl`, or the devices themselves start.
  /// Without it, the shell answers only the pairings
  /// [`Bluetooth::pair`] starts, since BlueZ asks the agent of whoever
  /// called `Pair`.
  ///
  /// [`Bluetooth::pair`]: crate::Bluetooth::pair
  pub fn make_default(&self) {
    if let Some(service) = &self.service {
      service.make_default();
    }
  }

  fn kind(&self) -> Option<PairingKind> {
    let service = self.service.as_ref()?;
    let state = service.state.borrow();
    let current = state.as_ref()?.current.as_ref()?;
    Some(current.shown.kind.clone())
  }

  /// Answers the request with `answer` if `fits`, else logs that `what`
  /// was dropped.
  fn answer<B>(&self, fits: bool, what: &str, answer: Result<&B, (&str, &str)>)
  where
    B: serde::Serialize + zbus::DynamicType,
  {
    let Some(service) = &self.service else {
      return;
    };
    if !fits {
      return tracing::warn!(what, "no Bluetooth request takes it; dropped");
    }
    let taken = service
      .state
      .borrow_mut()
      .as_mut()
      .and_then(|state| state.current.take());
    service.publish();
    if let Some(call) = taken.and_then(|pending| pending.call) {
      service.reply(&call, answer);
    }
  }
}

/// `cx.bluetooth_agent()`.
pub(crate) fn bluetooth_agent(cx: &Cx) -> BluetoothAgent {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.bluetooth_agent() needs the services the shell provides; nothing arrives");
    });
    return BluetoothAgent {
      service: None,
      request: cx.signal(None).read_only(),
    };
  };
  let request = service.start();
  BluetoothAgent {
    service: Some(service),
    request,
  }
}

/// The pairing agent of one shell, registered on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
  /// The number the next request gets.
  next: Cell<u64>,
}

struct State {
  request: Signal<Option<PairingRequest>>,
  /// The system bus, once connected.
  connection: Option<Connection>,
  /// BlueZ's unique name, once it took the registration: the only caller
  /// the agent answers.
  bluez: Option<String>,
  /// Whether the config asked to be BlueZ's default agent.
  default: bool,
  current: Option<Pending>,
}

/// The request shown.
struct Pending {
  shown: PairingRequest,
  /// BlueZ's call, while it waits for the answer. A code to show is
  /// answered at once, so it has none.
  call: Option<Message>,
}

/// Rejects the request and unregisters the agent when the root goes.
struct StopOnDrop(Rc<Service>);

impl Drop for StopOnDrop {
  fn drop(&mut self) {
    self.0.stop();
  }
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
      next: Cell::new(1),
    }
  }

  /// The request, registering with BlueZ if this is the first ask.
  fn start(self: &Rc<Self>) -> ReadSignal<Option<PairingRequest>> {
    if let Some(state) = &*self.state.borrow() {
      return state.request.read_only();
    }
    let request = self.shared.root.signal(None);
    *self.state.borrow_mut() = Some(State {
      request,
      connection: None,
      bluez: None,
      default: false,
      current: None,
    });
    self.shared.root.signal(StopOnDrop(Rc::clone(self)));
    match self.shared.config.system_bus.clone() {
      Some(address) => self.shared.spawn(run(Rc::clone(self), address)),
      None => tracing::info!("no system bus, so no Bluetooth pairing requests arrive"),
    }
    request.read_only()
  }

  /// Whether the request is still there: its owner, the root, isn't
  /// disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state
      .as_ref()
      .and_then(|state| state.request.try_with(|_| ()));
    alive.is_some()
  }

  /// Shows the request as it is now.
  fn publish(&self) {
    let shown = self.state.borrow().as_ref().map(|state| {
      let current = state.current.as_ref();
      (state.request, current.map(|pending| pending.shown.clone()))
    });
    if let Some((signal, shown)) = shown {
      set_changed(signal, shown);
    }
  }

  /// The connection the agent registered on, while BlueZ holds it.
  pub(crate) fn connection(&self) -> Option<Connection> {
    let state = self.state.borrow();
    let state = state.as_ref()?;
    state.bluez.as_ref().and(state.connection.clone())
  }

  fn make_default(self: &Rc<Self>) {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.default = true;
    }
    let connection = self.connection();
    if let Some(connection) = connection {
      let service = Rc::clone(self);
      self
        .shared
        .spawn(async move { request_default(&service, &connection).await });
    }
  }

  /// The root went: rejects the request and unregisters, as far as the
  /// loop still runs. Touches no signal, since the root is going.
  fn stop(&self) {
    let call = self.state.borrow_mut().as_mut().and_then(|state| {
      let pending = state.current.take()?;
      pending.call
    });
    if let Some(call) = call {
      self.reply::<()>(&call, Err((REJECTED, "the shell's agent went")));
    }
    let connection = self.state.borrow_mut().as_mut().and_then(|state| {
      let registered = state.bluez.take().is_some();
      state.connection.take().filter(|_| registered)
    });
    if let Some(connection) = connection {
      self.shared.spawn(async move {
        let path = agent_path();
        let call = connection.call_method(
          Some(BLUEZ),
          MANAGER_PATH,
          Some(MANAGER),
          "UnregisterAgent",
          &path,
        );
        if let Err(error) = call.await {
          tracing::debug!(%error, "BlueZ didn't take the agent back");
        }
      });
    }
  }

  /// One method call from the bus.
  fn on_call(self: &Rc<Self>, call: Message) {
    let header = call.header();
    let sender = header.sender().map(|sender| sender.as_str());
    let bluez = self
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.bluez.clone());
    if sender.is_none() || sender != bluez.as_deref() {
      tracing::warn!(
        ?sender,
        "a program other than BlueZ called the Bluetooth agent; refused"
      );
      return self.reply::<()>(&call, Err((ACCESS_DENIED, "only BlueZ may call the agent")));
    }
    let path = header.path().map(|path| path.as_str());
    let interface = header.interface().map(|interface| interface.as_str());
    if path != Some(AGENT_PATH) || interface != Some(AGENT) {
      return self.reply::<()>(&call, Err((UNKNOWN_METHOD, "no such method")));
    }
    let member = header.member().map_or("", |member| member.as_str());
    let body = call.body();
    let asked = match member {
      "Release" => {
        tracing::info!("BlueZ released the Bluetooth agent");
        self.reply(&call, Ok(&()));
        if let Some(state) = self.state.borrow_mut().as_mut() {
          state.bluez = None;
        }
        return self.end(CANCELED, "BlueZ released the agent");
      }
      "Cancel" => {
        self.reply(&call, Ok(&()));
        return self.end(CANCELED, "BlueZ cancelled it");
      }
      "RequestPinCode" => body
        .deserialize::<ObjectPath<'_>>()
        .map(|device| (device, PairingKind::PinCode, true)),
      "RequestPasskey" => body
        .deserialize::<ObjectPath<'_>>()
        .map(|device| (device, PairingKind::Passkey, true)),
      "DisplayPinCode" => body
        .deserialize::<(ObjectPath<'_>, &str)>()
        .map(|(device, pin_code)| {
          let pin_code = cut(pin_code, MAX_PIN);
          (device, PairingKind::DisplayPinCode { pin_code }, false)
        }),
      "DisplayPasskey" => {
        body
          .deserialize::<(ObjectPath<'_>, u32, u16)>()
          .map(|(device, passkey, entered)| {
            let kind = PairingKind::DisplayPasskey { passkey, entered };
            (device, kind, false)
          })
      }
      "RequestConfirmation" => body
        .deserialize::<(ObjectPath<'_>, u32)>()
        .map(|(device, passkey)| (device, PairingKind::Confirmation { passkey }, true)),
      "RequestAuthorization" => body
        .deserialize::<ObjectPath<'_>>()
        .map(|device| (device, PairingKind::Authorization, true)),
      "AuthorizeService" => body
        .deserialize::<(ObjectPath<'_>, &str)>()
        .map(|(device, uuid)| {
          let uuid = cut(uuid, MAX_UUID);
          (device, PairingKind::Service { uuid }, true)
        }),
      _ => return self.reply::<()>(&call, Err((UNKNOWN_METHOD, "no such method"))),
    };
    let Ok((device, kind, waits)) = asked else {
      return self.reply::<()>(&call, Err((INVALID_ARGS, "not what BlueZ sends")));
    };
    tracing::info!(member, "BlueZ asks the Bluetooth agent");
    let device = device.to_string();
    if !waits {
      // BlueZ waits only for the call to arrive, and cancels the code later.
      self.reply(&call, Ok(&()));
    }
    self.show(device, kind, waits.then_some(call.clone()));
  }

  /// Shows a new request, in place of the one before, which BlueZ is told
  /// was cancelled if it still waited.
  fn show(&self, device: String, kind: PairingKind, call: Option<Message>) {
    let replaced = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      // A passkey whose typed count went up stays the same request.
      let same = state.current.as_ref().filter(|pending| {
        let typing = |kind: &PairingKind| matches!(kind, PairingKind::DisplayPasskey { .. });
        pending.shown.device == device && typing(&pending.shown.kind) && typing(&kind)
      });
      let id = same.map_or_else(
        || self.next.replace(self.next.get() + 1),
        |pending| pending.shown.id,
      );
      let shown = PairingRequest { id, device, kind };
      let old = state.current.replace(Pending { shown, call });
      old.and_then(|pending| pending.call)
    };
    if let Some(old) = replaced {
      self.reply::<()>(&old, Err((CANCELED, "another request came")));
    }
    self.publish();
  }

  /// Takes the request away, telling BlueZ `error` if it still waited.
  fn end(&self, error: &str, why: &str) {
    let taken = self
      .state
      .borrow_mut()
      .as_mut()
      .and_then(|state| state.current.take());
    let Some(pending) = taken else {
      return;
    };
    if let Some(call) = pending.call {
      self.reply::<()>(&call, Err((error, why)));
    }
    self.publish();
  }

  /// Answers `call` with `answer`'s body, or with the error and why.
  fn reply<B>(&self, call: &Message, answer: Result<&B, (&str, &str)>)
  where
    B: serde::Serialize + zbus::DynamicType,
  {
    let header = call.header();
    if header.primary().flags().contains(Flags::NO_REPLY_EXPECTED) {
      return;
    }
    let reply = match answer {
      Ok(body) => Message::method_return(&header).build(body),
      Err((name, why)) => Message::error(&header, name).build(&why),
    };
    let connection = self
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.connection.clone());
    let Some(connection) = connection else {
      return;
    };
    self.shared.spawn(async move {
      let sent = match reply {
        Ok(reply) => connection.send(&reply).await,
        Err(error) => Err(error),
      };
      if let Err(error) = sent {
        tracing::debug!(%error, "the Bluetooth agent's answer wasn't sent");
      }
    });
  }
}

/// `text`'s first `max` bytes at most, cut at a char boundary.
fn cut(text: &str, max: usize) -> String {
  text[..text.floor_char_boundary(max)].to_owned()
}

fn agent_path() -> ObjectPath<'static> {
  ObjectPath::from_static_str(AGENT_PATH).expect("invariant: AGENT_PATH is an object path")
}

/// Joins the system bus at `address`, registers the agent with BlueZ, again
/// whenever BlueZ starts again, and answers BlueZ's calls, until the bus
/// closes or the request is disposed. It follows only BlueZ's owner, so
/// nothing but BlueZ's calls wakes the shell.
async fn run(service: Rc<Service>, address: zbus::Address) {
  let connection = match zbus::connection::Builder::address(address).build().await {
    Ok(connection) => connection,
    Err(error) => {
      return tracing::info!(%error, "can't reach the system bus, so no pairing requests arrive");
    }
  };
  // Made before BlueZ can call, so no call is missed.
  let mut messages = MessageStream::from(&connection);
  if let Err(error) = bus_call(&connection, "AddMatch", &owner_rule(BLUEZ)).await {
    return tracing::warn!(%error, "can't follow BlueZ, so no pairing requests arrive");
  }
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.connection = Some(connection.clone());
  }
  register(&service, &connection).await;
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !service.alive() {
      return;
    }
    match next {
      Some(Ok(message)) if message.header().message_type() == Kind::MethodCall => {
        service.on_call(message);
      }
      Some(Ok(message)) => {
        // The bus daemon sends this, so nobody else can fake it.
        if let Some((name, owner)) = owner_changed(&message)
          && name == BLUEZ
        {
          // Its calls went with it, so nothing is answered.
          if let Some(state) = service.state.borrow_mut().as_mut() {
            state.bluez = None;
            state.current = None;
          }
          service.publish();
          if !owner.is_empty() {
            tracing::info!("BlueZ started again; registering the Bluetooth agent with it");
            register(&service, &connection).await;
          }
        }
      }
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the system bus"),
      None => break,
    }
  }
  tracing::warn!("the system bus closed, so no more pairing requests arrive");
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.connection = None;
    state.bluez = None;
    state.current = None;
  }
  service.publish();
}

/// Registers the agent with the BlueZ on `connection`, which takes its
/// calls only from the unique name that answered, and makes it the
/// default if the config asked. A refusal is logged.
async fn register(service: &Rc<Service>, connection: &Connection) {
  let body = (agent_path(), CAPABILITY);
  let call = connection.call_method(
    Some(BLUEZ),
    MANAGER_PATH,
    Some(MANAGER),
    "RegisterAgent",
    &body,
  );
  let bluez = match service.shared.timeout(CALL_TIMEOUT, call).await {
    Ok(reply) => reply.header().sender().map(|sender| sender.to_string()),
    Err(error) => {
      return tracing::info!(%error, "BlueZ didn't take the agent; it registers when BlueZ starts");
    }
  };
  tracing::info!("registered as a Bluetooth agent");
  let default = service.state.borrow_mut().as_mut().is_some_and(|state| {
    state.bluez = bluez;
    state.default
  });
  if default {
    request_default(service, connection).await;
  }
}

/// Asks BlueZ to make the agent its default. A refusal is logged.
async fn request_default(service: &Rc<Service>, connection: &Connection) {
  let path = agent_path();
  let call = connection.call_method(
    Some(BLUEZ),
    MANAGER_PATH,
    Some(MANAGER),
    "RequestDefaultAgent",
    &path,
  );
  match service.shared.timeout(CALL_TIMEOUT, call).await {
    Ok(_) => tracing::info!("the Bluetooth agent is BlueZ's default"),
    Err(error) => tracing::warn!(%error, "BlueZ refused to make the agent its default"),
  }
}
