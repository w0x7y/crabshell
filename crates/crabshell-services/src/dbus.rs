//! What the D-Bus services share: calls to the bus itself, and objects
//! whose properties are read once and then kept up to date from
//! `PropertiesChanged`.

use std::cell::Cell;
use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{Connection, Message, MessageStream, OwnedVariant};

use crate::Shared;

pub(crate) const BUS: &str = "org.freedesktop.DBus";
pub(crate) const BUS_PATH: &str = "/org/freedesktop/DBus";
pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// How long a call to another program may take. The timer is the
/// loop's, and goes with the call.
pub(crate) const CALL_TIMEOUT: Duration = Duration::from_secs(25);

/// One object's properties on one interface, by name.
pub(crate) type Props = HashMap<String, OwnedVariant>;

/// Calls the bus itself, such as `AddMatch` with a match rule.
pub(crate) async fn bus_call(
  connection: &Connection,
  method: &str,
  rule: &str,
) -> zbus::Result<()> {
  let call = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), method, &rule);
  call.await.map(drop)
}

/// The match rule that says when `bus` gets a new owner or none. `bus`
/// is a checked bus name, so it holds no quote.
pub(crate) fn owner_rule(bus: &str) -> String {
  format!("type='signal',sender='{BUS}',interface='{BUS}',member='NameOwnerChanged',arg0='{bus}'")
}

/// The unique name that owns `bus` now, or `None` while nobody does.
pub(crate) async fn owner(connection: &Connection, bus: &str) -> Option<String> {
  let call = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &bus);
  call.await.ok()?.body().deserialize::<String>().ok()
}

/// A new owner of a name the bus announces, `""` for none: the name and
/// its new owner, if `message` is the bus's `NameOwnerChanged`.
pub(crate) fn owner_changed(message: &Message) -> Option<(String, String)> {
  let header = message.header();
  if header.sender().map(|sender| sender.as_str()) != Some(BUS)
    || header.member().map(|member| member.as_str()) != Some("NameOwnerChanged")
  {
    return None;
  }
  let (name, _, owner): (String, String, String) = message.body().deserialize().ok()?;
  Some((name, owner))
}

/// `interface`'s properties on `path` at `destination`, or the error.
pub(crate) async fn get_all(
  shared: &Shared,
  connection: &Connection,
  destination: &str,
  path: &str,
  interface: &str,
) -> zbus::Result<Props> {
  let call = connection.call_method(
    Some(destination),
    path,
    Some(PROPERTIES),
    "GetAll",
    &interface,
  );
  let reply = shared.timeout(CALL_TIMEOUT, call).await?;
  reply.body().deserialize::<Props>()
}

/// Sets `interface`'s property `name` on `path` at `destination`.
pub(crate) async fn set(
  shared: &Shared,
  connection: &Connection,
  destination: &str,
  (path, interface): (&str, &str),
  name: &str,
  value: zbus::Variant<'_>,
) -> zbus::Result<()> {
  let body = (interface, name, value);
  let call = connection.call_method(Some(destination), path, Some(PROPERTIES), "Set", &body);
  shared.timeout(CALL_TIMEOUT, call).await.map(drop)
}

/// What a `PropertiesChanged` signal says: the object's path, the
/// interface, and the properties it set and dropped. `None` for any
/// other message, or one with a body of the wrong type.
pub(crate) fn properties_changed(
  message: &Message,
) -> Option<(String, String, Props, Vec<String>)> {
  let header = message.header();
  if header.interface().map(|interface| interface.as_str()) != Some(PROPERTIES)
    || header.member().map(|member| member.as_str()) != Some("PropertiesChanged")
  {
    return None;
  }
  let path = header.path()?.as_str().to_owned();
  let (interface, changed, dropped): (String, Props, Vec<String>) =
    message.body().deserialize().ok()?;
  Some((path, interface, changed, dropped))
}

/// `props` with `changed` set and `dropped` taken out.
pub(crate) fn apply(props: &mut Props, changed: Props, dropped: Vec<String>) {
  for name in dropped {
    props.remove(&name);
  }
  props.extend(changed);
}

/// The property `name` as a `T`, or `None` when it is missing or of
/// another type.
pub(crate) fn get<'a, T>(props: &'a Props, name: &str) -> Option<T>
where
  T: TryFrom<&'a OwnedVariant>,
{
  T::try_from(props.get(name)?).ok()
}

/// The property `name` as a string, or `""`.
pub(crate) fn text(props: &Props, name: &str) -> String {
  get::<&str>(props, name).unwrap_or_default().to_owned()
}

/// A system daemon a service follows, such as UPower: read whole when it
/// appears on the bus, kept up to date from its signals, and emptied when
/// it leaves.
pub(crate) trait Daemon: 'static {
  /// The bus name it owns.
  const NAME: &'static str;

  /// Whether the service's signals are still there: their owner, the
  /// root, isn't disposed.
  fn alive(&self) -> bool;

  /// Empties what the service shows, as when the daemon left, and
  /// returns a new generation, so a read started before is thrown away.
  fn clear(&self) -> u64;

  /// Reads everything from the daemon and shows it, unless the
  /// generation moved on meanwhile.
  async fn read(self: Rc<Self>, connection: Connection, generation: u64);

  /// Acts on one signal from the daemon's current owner.
  fn signal(self: &Rc<Self>, connection: &Connection, message: &Message);

  /// Whether `message`, a signal from the daemon, is best met by reading
  /// everything again, as when it names a new object without its
  /// properties. Such a signal isn't passed to [`signal`](Self::signal).
  fn reads_again(&self, _message: &Message) -> bool {
    false
  }
}

/// Where the reading of a daemon stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
  Idle,
  Busy,
  /// Busy, and a signal came meanwhile, which the read may have missed,
  /// so it reads again when done.
  Stale,
}

/// Reads `daemon` whole in the background, then again for as long as
/// signals keep coming during a read, each time for the generation in
/// effect then. A read asked for while one runs only marks it stale.
fn read<D: Daemon>(
  shared: &Rc<Shared>,
  daemon: &Rc<D>,
  connection: &Connection,
  (reading, generation): (&Rc<Cell<Reading>>, &Rc<Cell<u64>>),
) {
  if reading.get() != Reading::Idle {
    return reading.set(Reading::Stale);
  }
  reading.set(Reading::Busy);
  let (daemon, connection) = (Rc::clone(daemon), connection.clone());
  let (reading, generation) = (Rc::clone(reading), Rc::clone(generation));
  shared.spawn(async move {
    loop {
      Rc::clone(&daemon)
        .read(connection.clone(), generation.get())
        .await;
      if reading.get() != Reading::Stale {
        return reading.set(Reading::Idle);
      }
      reading.set(Reading::Busy);
    }
  });
}

/// Follows `daemon` on the bus at `address`, until the bus closes or the
/// service's signals are disposed. While the daemon isn't running, this
/// waits on the bus and never wakes the shell.
pub(crate) async fn follow<D: Daemon>(shared: Rc<Shared>, daemon: Rc<D>, address: zbus::Address) {
  let name = D::NAME;
  let connection = match zbus::connection::Builder::address(address).build().await {
    Ok(connection) => connection,
    Err(error) => return tracing::info!(%error, name, "can't reach the system bus"),
  };
  // Made before anything is asked, so no signal is missed.
  let mut messages = MessageStream::from(&connection);
  for rule in [format!("type='signal',sender='{name}'"), owner_rule(name)] {
    if let Err(error) = bus_call(&connection, "AddMatch", &rule).await {
      return tracing::warn!(%error, name, "couldn't follow it on the bus");
    }
  }
  let reading = Rc::new(Cell::new(Reading::Idle));
  let generation = Rc::new(Cell::new(daemon.clear()));
  let state = (&reading, &generation);
  let mut current = owner(&connection, name).await;
  match &current {
    Some(_) => read(&shared, &daemon, &connection, state),
    None => tracing::info!(name, "not running; waiting for it"),
  }
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !daemon.alive() {
      return;
    }
    let message = match next {
      Some(Ok(message)) => message,
      Some(Err(error)) => {
        tracing::warn!(%error, name, "a bad message on the bus");
        continue;
      }
      None => break,
    };
    let header = message.header();
    if header.message_type() != Kind::Signal {
      continue;
    }
    if let Some((changed, new)) = owner_changed(&message) {
      if changed == name {
        generation.set(daemon.clear());
        current = (!new.is_empty()).then_some(new);
        if current.is_some() {
          tracing::info!(name, "started; reading it");
          read(&shared, &daemon, &connection, state);
        } else {
          tracing::info!(name, "left the bus");
        }
      }
      continue;
    }
    // Its broadcasts only: any program can aim a signal at this
    // connection, and anyone can claim the daemon's name in the header.
    let from_daemon = current.is_some()
      && header.sender().map(|sender| sender.as_str()) == current.as_deref()
      && header.destination().is_none();
    if !from_daemon {
      continue;
    }
    // A read under way may have missed it: read again after.
    if reading.get() != Reading::Idle || daemon.reads_again(&message) {
      read(&shared, &daemon, &connection, state);
    } else {
      daemon.signal(&connection, &message);
    }
  }
  tracing::warn!(name, "the bus closed");
  daemon.clear();
}
