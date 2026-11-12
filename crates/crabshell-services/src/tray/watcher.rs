//! The watcher: the shell's answers to calls on `/StatusNotifierWatcher`.
//! zbus's object server would answer on a thread of its own, so the
//! calls are answered here, on the main thread, by hand.

use std::collections::HashMap;
use std::rc::Rc;

use zbus::message::Flags;
use zbus::{Connection, Message, Variant};

use super::{Service, WATCHER, WATCHER_PATH, parse_service};

/// What the watcher offers, for callers that look before calling, as Qt's
/// `QDBusInterface` does.
const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.kde.StatusNotifierWatcher">
    <method name="RegisterStatusNotifierItem"><arg name="service" type="s" direction="in"/></method>
    <method name="RegisterStatusNotifierHost"><arg name="service" type="s" direction="in"/></method>
    <property name="RegisteredStatusNotifierItems" type="as" access="read"/>
    <property name="IsStatusNotifierHostRegistered" type="b" access="read"/>
    <property name="ProtocolVersion" type="i" access="read"/>
    <signal name="StatusNotifierItemRegistered"><arg type="s"/></signal>
    <signal name="StatusNotifierItemUnregistered"><arg type="s"/></signal>
    <signal name="StatusNotifierHostRegistered"/>
    <signal name="StatusNotifierHostUnregistered"/>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get">
      <arg name="interface" type="s" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="GetAll">
      <arg name="interface" type="s" direction="in"/>
      <arg name="properties" type="a{sv}" direction="out"/>
    </method>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="xml" type="s" direction="out"/></method>
  </interface>
</node>
"#;

/// Answers one method call to this connection. Anything but the watcher's
/// methods gets the bus's usual error, so no caller waits for a timeout.
/// A call that asks for no reply is still acted on, as GDBus sends a
/// call with no callback.
pub(super) async fn answer(service: &Rc<Service>, connection: &Connection, call: &Message) {
  let header = call.header();
  let path = header.path().map(|path| path.as_str());
  let member = header.member().map_or("", |member| member.as_str());
  let body = call.body();
  let reply = Message::method_return(&header);
  let refuse = |error: &str, why: &str| Message::error(&header, error).build(&why);
  let answer = match (path, member) {
    (Some(WATCHER_PATH), "RegisterStatusNotifierItem") => {
      let sender = header.sender().map(|sender| sender.as_str());
      let item = body
        .deserialize::<&str>()
        .ok()
        .and_then(|service| parse_service(service, sender));
      match item {
        Some((bus, path)) => {
          service.add(connection, bus, path);
          reply.build(&())
        }
        None => refuse(INVALID_ARGS, "not a bus name or object path"),
      }
    }
    (Some(WATCHER_PATH), "RegisterStatusNotifierHost") => {
      service.emit(connection, "StatusNotifierHostRegistered", None);
      reply.build(&())
    }
    (Some(WATCHER_PATH), "Get") => {
      let value = match body.deserialize::<(&str, &str)>() {
        Ok((WATCHER, name)) => property(service, name),
        _ => None,
      };
      match value {
        Some(value) => reply.build(&value),
        None => refuse(UNKNOWN_PROPERTY, "no such property"),
      }
    }
    (Some(WATCHER_PATH), "GetAll") => {
      let names = match body.deserialize::<&str>() {
        Ok(WATCHER) => PROPERTIES_OF_WATCHER,
        _ => &[],
      };
      let all: HashMap<&str, Variant<'_>> = names
        .iter()
        .filter_map(|&name| Some((name, property(service, name)?)))
        .collect();
      reply.build(&all)
    }
    (Some(WATCHER_PATH), "Introspect") => reply.build(&INTROSPECTION),
    _ => refuse(UNKNOWN_METHOD, "no such method"),
  };
  if header.primary().flags().contains(Flags::NO_REPLY_EXPECTED) {
    return;
  }
  let sent = match answer {
    Ok(answer) => connection.send(&answer).await,
    Err(error) => Err(error),
  };
  if let Err(error) = sent {
    tracing::debug!(member, %error, "the tray watcher's answer wasn't sent");
  }
}

const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_PROPERTY: &str = "org.freedesktop.DBus.Error.UnknownProperty";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

const PROPERTIES_OF_WATCHER: &[&str] = &[
  "RegisteredStatusNotifierItems",
  "IsStatusNotifierHostRegistered",
  "ProtocolVersion",
];

/// One of the watcher's properties. This shell is a host, so one is
/// always registered.
fn property(service: &Service, name: &str) -> Option<Variant<'static>> {
  match name {
    "RegisteredStatusNotifierItems" => Some(Variant::from(service.ids())),
    "IsStatusNotifierHostRegistered" => Some(Variant::from(true)),
    "ProtocolVersion" => Some(Variant::from(0)),
    _ => None,
  }
}
