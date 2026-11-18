//! A screen reader's view of the shell, over AT-SPI 2.
//!
//! The shell follows `org.a11y.Status` on the session bus. While it says
//! a screen reader may listen, the shell asks `org.a11y.Bus` for the
//! accessibility bus, joins it, embeds its application root with the
//! registry, and answers calls on its windows and widgets by hand, in
//! [`server`], reading `crabshell_ui::Accessibility`. It emits the focus,
//! name and window events that registry kept. While the status says no,
//! it holds no accessibility connection, and the trees are never walked.

mod server;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Poll, Waker};

use crabshell_reactive::Signal;
use crabshell_ui::{AccessEvent, AccessId, Accessibility, Cx};
use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{Connection, Message, MessageStream, Variant};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, Props};

/// The session bus's name for the accessibility bus launcher.
const A11Y_BUS: &str = "org.a11y.Bus";
const A11Y_PATH: &str = "/org/a11y/bus";
const STATUS: &str = "org.a11y.Status";
const REGISTRY: &str = "org.a11y.atspi.Registry";
const SOCKET: &str = "org.a11y.atspi.Socket";
const EVENT_OBJECT: &str = "org.a11y.atspi.Event.Object";
const EVENT_WINDOW: &str = "org.a11y.atspi.Event.Window";
/// The application's own object, and the prefix of every other one.
pub(crate) const ROOT: &str = "/org/a11y/atspi/accessible/root";
pub(crate) const PREFIX: &str = "/org/a11y/atspi/accessible/";
/// The longest accessibility bus address taken from the launcher.
const MAX_ADDRESS: usize = 4096;

/// The screen reader support of one shell, started by the shell itself.
pub(crate) struct Service {
  shared: Rc<Shared>,
  status: RefCell<Option<Rc<Status>>>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      status: RefCell::new(None),
    }
  }
}

/// Follows `org.a11y.Status` from the session bus, and serves AT-SPI
/// while it says yes. Once per shell; a later call does nothing.
#[doc(hidden)]
pub fn follow_accessibility(cx: &Cx) {
  let (Some(service), Some(access)) = (cx.service::<Service>(), cx.service::<Accessibility>())
  else {
    return tracing::warn!("no screen reader support on a context the shell didn't set up");
  };
  if service.status.borrow().is_some() {
    return;
  }
  let shared = &service.shared;
  if shared.config.no_at_bridge {
    return tracing::info!("NO_AT_BRIDGE=1, so no screen reader reads the shell");
  }
  let Some(address) = shared.config.session_bus.clone() else {
    return tracing::info!("no session bus, so no screen reader can read the shell");
  };
  let status = Rc::new(Status {
    shared: Rc::clone(shared),
    access,
    alive: shared.root.signal(()),
    props: RefCell::new(Props::new()),
    reads: Cell::new(0),
    run: Cell::new(0),
    serving: Cell::new(false),
    waker: RefCell::new(None),
    app: App::default(),
  });
  service.status.replace(Some(Rc::clone(&status)));
  shared.spawn(dbus::follow(Rc::clone(shared), status, address));
}

/// What the registry and the application object keep while serving.
#[derive(Default)]
pub(crate) struct App {
  /// The registry's root, the application's parent.
  pub(crate) desktop: RefCell<Option<(String, zbus::OwnedObjectPath)>>,
  /// The id the registry gave the application.
  pub(crate) id: Cell<i32>,
}

/// The status, and the run of the provider it started.
struct Status {
  shared: Rc<Shared>,
  access: Rc<Accessibility>,
  /// Disposed with the root, which ends the following.
  alive: Signal<()>,
  props: RefCell<Props>,
  /// Counts reads of the status, so an old one is thrown away.
  reads: Cell<u64>,
  /// Counts the provider's runs; a run that isn't the current one stops.
  run: Cell<u64>,
  serving: Cell<bool>,
  /// Wakes the current run's loop when it should stop.
  waker: RefCell<Option<Waker>>,
  app: App,
}

impl Status {
  /// Starts or stops serving as the status now says.
  fn update(self: &Rc<Self>, connection: &Connection) {
    let props = self.props.borrow();
    let on = dbus::get::<bool>(&props, "IsEnabled") == Some(true)
      || dbus::get::<bool>(&props, "ScreenReaderEnabled") == Some(true);
    drop(props);
    if on == self.serving.get() {
      return;
    }
    self.stop();
    if on {
      self.serving.set(true);
      let (status, connection) = (Rc::clone(self), connection.clone());
      self.shared.spawn(serve(status, connection, self.run.get()));
    }
  }

  /// Ends the current run, if any, and stops keeping changes.
  fn stop(&self) {
    self.run.set(self.run.get() + 1);
    self.serving.set(false);
    self.access.listen(false);
    self.app.desktop.replace(None);
    if let Some(waker) = self.waker.take() {
      waker.wake();
    }
  }

  fn current(&self, run: u64) -> bool {
    self.run.get() == run && self.alive()
  }
}

impl Daemon for Status {
  const NAME: &'static str = A11Y_BUS;

  fn alive(&self) -> bool {
    self.alive.try_with(|_| ()).is_some()
  }

  fn clear(&self) -> u64 {
    self.props.borrow_mut().clear();
    if self.serving.get() {
      self.stop();
    }
    self.reads.set(self.reads.get() + 1);
    self.reads.get()
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let read = dbus::get_all(&self.shared, &connection, A11Y_BUS, A11Y_PATH, STATUS).await;
    let props = match read {
      Ok(props) => props,
      Err(error) => return tracing::warn!(%error, "couldn't read the accessibility status"),
    };
    if self.reads.get() == generation {
      self.props.replace(props);
      self.update(&connection);
    }
  }

  fn signal(self: &Rc<Self>, connection: &Connection, message: &Message) {
    if let Some((path, interface, changed, dropped)) = dbus::properties_changed(message)
      && path == A11Y_PATH
      && interface == STATUS
    {
      dbus::apply(&mut self.props.borrow_mut(), changed, dropped);
      self.update(connection);
    }
  }

  fn rules() -> Vec<String> {
    vec![format!(
      "type='signal',sender='{A11Y_BUS}',path='{A11Y_PATH}',interface='{}',member='PropertiesChanged'",
      dbus::PROPERTIES
    )]
  }
}

/// Joins the accessibility bus the launcher names, embeds the application
/// with the registry, and answers calls until run `run` ends or the bus
/// closes. A failure to join ends the run, so the next word from the
/// status tries again.
async fn serve(status: Rc<Status>, session: Connection, run: u64) {
  let shared = &status.shared;
  let Some((connection, mut messages, name, desktop)) = join(&status, &session).await else {
    if status.current(run) {
      status.stop();
    }
    return;
  };
  if !status.current(run) {
    return;
  }
  status.app.desktop.replace(Some(desktop));
  status.access.listen(true);
  tracing::info!("a screen reader may listen; serving the shell's widgets over AT-SPI");
  shared.spawn(emit(
    Rc::clone(&status),
    connection.clone(),
    name.clone(),
    run,
  ));
  loop {
    let next = poll_fn(|cx| {
      if !status.current(run) {
        return Poll::Ready(None);
      }
      status.waker.replace(Some(cx.waker().clone()));
      Pin::new(&mut messages).poll_next(cx)
    })
    .await;
    match next {
      Some(Ok(message)) if message.header().message_type() == Kind::MethodCall => {
        server::answer(&status.access, &status.app, &connection, &name, &message).await;
      }
      Some(Ok(_)) => {}
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the accessibility bus"),
      None => break,
    }
  }
  if status.current(run) {
    tracing::warn!("the accessibility bus closed");
    status.stop();
  } else {
    tracing::info!("no screen reader listens now; stopped serving AT-SPI");
  }
}

/// The accessibility bus joined, read from before anything is asked, the
/// shell's name there and the registry's root; `None`, logged, on a
/// failure.
async fn join(
  status: &Status,
  session: &Connection,
) -> Option<(
  Connection,
  MessageStream,
  String,
  (String, zbus::OwnedObjectPath),
)> {
  let shared = &status.shared;
  let address = match &shared.config.at_spi_bus {
    Some(address) => Ok(address.clone()),
    None => {
      let call = session.call_method(Some(A11Y_BUS), A11Y_PATH, Some(A11Y_BUS), "GetAddress", &());
      match shared.timeout(CALL_TIMEOUT, call).await {
        Ok(reply) => reply.body().deserialize::<String>(),
        Err(error) => Err(error),
      }
    }
  };
  let address = match address {
    Ok(address) if address.len() <= MAX_ADDRESS => address,
    Ok(_) => {
      tracing::warn!("the accessibility bus address is too long; skipped");
      return None;
    }
    Err(error) => {
      tracing::warn!(%error, "couldn't ask for the accessibility bus");
      return None;
    }
  };
  let built = zbus::connection::Builder::address(address.as_str()).build();
  let connection = built
    .await
    .inspect_err(|error| tracing::warn!(%error, "couldn't join the accessibility bus"))
    .ok()?;
  // Made before anything is asked, so no call is missed.
  let messages = MessageStream::from(&connection);
  let Some(name) = connection.unique_name().map(|name| name.to_string()) else {
    tracing::warn!("the accessibility bus gave no name");
    return None;
  };
  // One argument, the plug: a struct of the name and the root's path.
  let plug = ((
    name.as_str(),
    zbus::ObjectPath::from_static_str_unchecked(ROOT),
  ),);
  let embed = connection.call_method(Some(REGISTRY), ROOT, Some(SOCKET), "Embed", &plug);
  let desktop = match shared.timeout(CALL_TIMEOUT, embed).await {
    Ok(reply) => reply
      .body()
      .deserialize::<(String, zbus::OwnedObjectPath)>(),
    Err(error) => Err(error),
  };
  let desktop = desktop
    .inspect_err(|error| tracing::warn!(%error, "the accessibility registry refused the shell"))
    .ok()?;
  Some((connection, messages, name, desktop))
}

/// Sends the changes the windows keep as AT-SPI events, while run `run`
/// lasts.
async fn emit(status: Rc<Status>, connection: Connection, name: String, run: u64) {
  // The windows a screen reader was last told of.
  let mut listed = status.access.windows();
  loop {
    let events = poll_fn(|cx| {
      if !status.current(run) {
        return Poll::Ready(None);
      }
      let events = status.access.take_events(cx.waker());
      if events.is_empty() {
        Poll::Pending
      } else {
        Poll::Ready(Some(events))
      }
    })
    .await;
    let Some(events) = events else {
      return;
    };
    for event in events {
      for (path, interface, member, body) in signals(&status.access, &name, &mut listed, event) {
        let emit = connection.emit_signal(None::<&str>, path.as_str(), interface, member, &body);
        if let Err(error) = emit.await {
          tracing::debug!(%error, member, "an accessibility event wasn't sent");
        }
      }
    }
  }
}

/// An event's body: its detail, detail1, detail2, its data, and no
/// properties.
type EventBody = (
  &'static str,
  i32,
  i32,
  Variant<'static>,
  HashMap<&'static str, Variant<'static>>,
);

/// One signal: its path, interface, member and body.
type Outgoing = (String, &'static str, &'static str, EventBody);

fn body(detail: &'static str, detail1: i32, data: Variant<'static>) -> EventBody {
  (detail, detail1, 0, data, HashMap::new())
}

/// The signals that tell a screen reader of `event`, the ones it keeps
/// its copy of the shell by. `listed` is the windows it was told of.
fn signals(
  access: &Accessibility,
  name: &str,
  listed: &mut Vec<AccessId>,
  event: AccessEvent,
) -> Vec<Outgoing> {
  let state = |id: AccessId, states: &[&'static str], on: bool| -> Vec<Outgoing> {
    let each = states.iter().map(|&state| {
      let body = body(state, i32::from(on), Variant::from(0));
      (server::path(id), EVENT_OBJECT, "StateChanged", body)
    });
    each.collect()
  };
  let child = |parent: String, child: AccessId, index: usize, added: bool| -> Outgoing {
    let detail = if added { "add" } else { "remove" };
    let index = i32::try_from(index).unwrap_or(-1);
    let data = Variant::from(server::reference(name, server::path(child)));
    (
      parent,
      EVENT_OBJECT,
      "ChildrenChanged",
      body(detail, index, data),
    )
  };
  match event {
    AccessEvent::Focused(id, on) => state(id, &["focused"], on),
    AccessEvent::Shown(id, on) => state(id, &["showing", "visible"], on),
    AccessEvent::Enabled(id, on) => state(id, &["enabled", "sensitive"], on),
    AccessEvent::NameChanged(id) => match access.node(id) {
      Some(node) => {
        let body = body("accessible-name", 0, Variant::from(node.name));
        vec![(server::path(id), EVENT_OBJECT, "PropertyChange", body)]
      }
      None => Vec::new(),
    },
    AccessEvent::Active(id, on) => {
      let title = access.node(id).map(|node| node.name).unwrap_or_default();
      let member = if on { "Activate" } else { "Deactivate" };
      let window = (
        server::path(id),
        EVENT_WINDOW,
        member,
        body("", 0, Variant::from(title)),
      );
      let mut signals = vec![window];
      signals.extend(state(id, &["active"], on));
      signals
    }
    AccessEvent::Children {
      parent,
      child: id,
      index,
      added,
    } => vec![child(server::path(parent), id, index, added)],
    AccessEvent::TextChanged {
      id,
      start,
      text,
      inserted,
    } => {
      let detail = if inserted { "insert" } else { "delete" };
      let offset = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
      let length = offset(text.chars().count());
      let body = (
        detail,
        offset(start),
        length,
        Variant::from(text),
        HashMap::new(),
      );
      vec![(server::path(id), EVENT_OBJECT, "TextChanged", body)]
    }
    AccessEvent::CaretMoved(id, at) => {
      let at = i32::try_from(at).unwrap_or(i32::MAX);
      let body = body("", at, Variant::from(0));
      vec![(server::path(id), EVENT_OBJECT, "TextCaretMoved", body)]
    }
    AccessEvent::ValueChanged(id) => match access.node(id).and_then(|node| node.value) {
      Some(value) => {
        let body = body("accessible-value", 0, Variant::from(value.current));
        vec![(server::path(id), EVENT_OBJECT, "PropertyChange", body)]
      }
      None => Vec::new(),
    },
    AccessEvent::WindowsChanged => {
      let now = access.windows();
      let gone = listed
        .iter()
        .enumerate()
        .filter(|(_, id)| !now.contains(id));
      let mut signals: Vec<Outgoing> = gone
        .map(|(index, &id)| child(ROOT.to_owned(), id, index, false))
        .collect();
      let new = now
        .iter()
        .enumerate()
        .filter(|(_, id)| !listed.contains(id));
      signals.extend(new.map(|(index, &id)| child(ROOT.to_owned(), id, index, true)));
      *listed = now;
      signals
    }
  }
}
