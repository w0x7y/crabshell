//! The answers to AT-SPI calls on the application and its windows'
//! widgets, from at-spi2-core's interfaces: `Accessible`, `Application`,
//! `Component`, `Action` and `Properties`.

use std::collections::HashMap;

use crabshell_ui::{AccessId, AccessNode, AccessRole, Accessibility};
use zbus::message::{Flags, Header};
use zbus::{Connection, Message, ObjectPath, Variant};

use super::{App, PREFIX, ROOT};
use crate::dbus::PROPERTIES;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const APPLICATION: &str = "org.a11y.atspi.Application";
const COMPONENT: &str = "org.a11y.atspi.Component";
const ACTION: &str = "org.a11y.atspi.Action";
const CACHE: &str = "org.a11y.atspi.Cache";
const CACHE_PATH: &str = "/org/a11y/atspi/cache";
const NULL: &str = "/org/a11y/atspi/null";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";

// A reply's tuple is its list of arguments, so one struct argument, as
// `(so)`, is sent as a tuple of one.

/// AT-SPI's states, by bit.
const ACTIVE: u32 = 1;
const EDITABLE: u32 = 7;
const ENABLED: u32 = 8;
const FOCUSABLE: u32 = 11;
const FOCUSED: u32 = 12;
const MULTI_LINE: u32 = 17;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
const SINGLE_LINE: u32 = 26;
const VISIBLE: u32 = 30;
/// `Component`'s coordinates relative to the parent; the screen's and
/// the window's are both the window's, which is all Wayland tells.
const COORD_PARENT: u32 = 2;

/// The path a screen reader names widget `id` by.
pub(crate) fn path(id: AccessId) -> String {
  format!("{PREFIX}{}_{}", id.window, id.node)
}

/// An AT-SPI reference: the shell's bus name and an object's path.
pub(super) type Reference = (String, ObjectPath<'static>);

pub(super) fn reference(name: &str, path: String) -> Reference {
  let path = ObjectPath::try_from(path).expect("invariant: the shell's object paths are valid");
  (name.to_owned(), path)
}

/// What a path names: the application, or a widget as it is now.
enum Object {
  App,
  Node(AccessId, AccessNode),
}

fn object(access: &Accessibility, path: &str) -> Option<Object> {
  if path == ROOT {
    return Some(Object::App);
  }
  let (window, node) = path.strip_prefix(PREFIX)?.split_once('_')?;
  let id = AccessId {
    window: window.parse().ok()?,
    node: node.parse().ok()?,
  };
  Some(Object::Node(id, access.node(id)?))
}

/// AT-SPI's number and name for a role.
fn role(role: AccessRole) -> (u32, &'static str) {
  match role {
    AccessRole::Frame => (23, "frame"),
    AccessRole::Window => (69, "window"),
    AccessRole::Filler => (20, "filler"),
    AccessRole::Button => (43, "push button"),
    AccessRole::Label => (29, "label"),
    AccessRole::Entry => (79, "entry"),
    AccessRole::PasswordText => (40, "password text"),
    AccessRole::Slider => (51, "slider"),
    AccessRole::Image => (27, "image"),
  }
}

/// The state set as AT-SPI sends it: 64 bits in two words, low first.
fn states(node: &AccessNode) -> [u32; 2] {
  let bits = [
    (node.active, ACTIVE),
    (node.editable, EDITABLE),
    (node.enabled, ENABLED),
    (node.enabled, SENSITIVE),
    (node.focusable, FOCUSABLE),
    (node.focused, FOCUSED),
    (node.editable && node.multi_line, MULTI_LINE),
    (node.editable && !node.multi_line, SINGLE_LINE),
    (node.shown, SHOWING),
    (node.shown, VISIBLE),
  ];
  let set = bits
    .iter()
    .filter(|(on, _)| *on)
    .fold(0u64, |set, (_, bit)| set | 1 << bit);
  [set as u32, (set >> 32) as u32]
}

/// What a call on an object is answered with: its parent, children and
/// so on, the same for the application as for a widget.
struct Facts {
  name: String,
  role: (u32, &'static str),
  parent: Reference,
  children: Vec<Reference>,
  index: i32,
  states: [u32; 2],
  interfaces: Vec<&'static str>,
}

fn facts(access: &Accessibility, app: &App, name: &str, object: &Object) -> Facts {
  let node_ref = |id: AccessId| reference(name, path(id));
  match object {
    Object::App => {
      let desktop = app.desktop.borrow().clone();
      let parent = desktop.map_or_else(
        || reference(name, NULL.to_owned()),
        |(bus, path)| (bus, path.into_inner()),
      );
      Facts {
        name: app_name(),
        role: (75, "application"),
        parent,
        children: access.windows().into_iter().map(node_ref).collect(),
        index: -1,
        states: [0, 0],
        interfaces: vec![ACCESSIBLE, APPLICATION],
      }
    }
    Object::Node(_, node) => {
      let mut interfaces = vec![ACCESSIBLE, COMPONENT];
      if node.clickable {
        interfaces.push(ACTION);
      }
      Facts {
        name: node.name.clone(),
        role: role(node.role),
        parent: node
          .parent
          .map_or_else(|| reference(name, ROOT.to_owned()), node_ref),
        children: node.children.iter().copied().map(node_ref).collect(),
        index: i32::try_from(node.index).unwrap_or(i32::MAX),
        states: states(node),
        interfaces,
      }
    }
  }
}

/// The shell's program name, as the application's.
fn app_name() -> String {
  let program = std::env::args_os().next().unwrap_or_default();
  let name = std::path::Path::new(&program)
    .file_name()
    .unwrap_or_default();
  name.to_string_lossy().into_owned()
}

/// Answers one method call, as a toolkit's AT-SPI bridge does. Anything
/// it doesn't know gets the bus's usual error, so no caller waits for a
/// timeout.
pub(super) async fn answer(
  access: &Accessibility,
  app: &App,
  connection: &Connection,
  name: &str,
  call: &Message,
) {
  let header = call.header();
  let answer = reply(access, app, name, call, &header);
  if header.primary().flags().contains(Flags::NO_REPLY_EXPECTED) {
    return;
  }
  let sent = match answer {
    Ok(answer) => connection.send(&answer).await,
    Err(error) => Err(error),
  };
  if let Err(error) = sent {
    tracing::debug!(%error, "an accessibility answer wasn't sent");
  }
}

fn reply(
  access: &Accessibility,
  app: &App,
  name: &str,
  call: &Message,
  header: &Header<'_>,
) -> zbus::Result<Message> {
  let refuse = |error: &str, why: &str| Message::error(header, error).build(&why);
  let path = header.path().map_or("", |path| path.as_str());
  let interface = header
    .interface()
    .map_or("", |interface| interface.as_str());
  let member = header.member().map_or("", |member| member.as_str());
  // Nothing cached up front, so a screen reader asks for what it reads
  // and never holds a stale copy.
  if (path, interface, member) == (CACHE_PATH, CACHE, "GetItems") {
    return reply_empty_cache(header);
  }
  let Some(object) = object(access, path) else {
    return refuse(UNKNOWN_OBJECT, "no such object");
  };
  let reply = Message::method_return(header);
  let body = call.body();
  let node = match &object {
    Object::Node(id, node) => Some((*id, node)),
    Object::App => None,
  };
  match (interface, member) {
    (PROPERTIES, "Get") => {
      let Ok((interface, property)) = body.deserialize::<(&str, &str)>() else {
        return refuse(INVALID_ARGS, "not an interface and a name");
      };
      let facts = facts(access, app, name, &object);
      match properties(&facts, node.map(|(_, node)| node), app, interface).remove(property) {
        Some(value) => reply.build(&value),
        None => refuse(INVALID_ARGS, "no such property"),
      }
    }
    (PROPERTIES, "GetAll") => {
      let Ok(interface) = body.deserialize::<&str>() else {
        return refuse(INVALID_ARGS, "not an interface");
      };
      let facts = facts(access, app, name, &object);
      reply.build(&properties(
        &facts,
        node.map(|(_, node)| node),
        app,
        interface,
      ))
    }
    // The registry numbers the application; nothing else is writable.
    (PROPERTIES, "Set") => match body.deserialize::<(&str, &str, Variant<'_>)>() {
      Ok((APPLICATION, "Id", value)) if node.is_none() => match i32::try_from(&value) {
        Ok(id) => {
          app.id.set(id);
          reply.build(&())
        }
        Err(_) => refuse(INVALID_ARGS, "the id isn't an int32"),
      },
      _ => refuse(INVALID_ARGS, "read-only"),
    },
    (ACCESSIBLE, member) => {
      let facts = facts(access, app, name, &object);
      match member {
        "GetChildAtIndex" => match body.deserialize::<i32>() {
          Ok(at) => {
            let child = usize::try_from(at)
              .ok()
              .and_then(|at| facts.children.get(at).cloned());
            reply.build(&(child.unwrap_or_else(|| reference(name, NULL.to_owned())),))
          }
          Err(_) => refuse(INVALID_ARGS, "not an index"),
        },
        "GetChildren" => reply.build(&facts.children),
        "GetIndexInParent" => reply.build(&facts.index),
        "GetRelationSet" => reply.build(&Vec::<(u32, Vec<Reference>)>::new()),
        "GetRole" => reply.build(&facts.role.0),
        "GetRoleName" | "GetLocalizedRoleName" => reply.build(&facts.role.1),
        "GetState" => reply.build(&facts.states.as_slice()),
        "GetAttributes" => reply.build(&HashMap::<&str, &str>::new()),
        "GetApplication" => reply.build(&(reference(name, ROOT.to_owned()),)),
        "GetInterfaces" => reply.build(&facts.interfaces),
        _ => refuse(UNKNOWN_METHOD, "no such method"),
      }
    }
    (APPLICATION, "GetLocale") if node.is_none() => reply.build(&""),
    (COMPONENT, member) => match node {
      Some((_, node)) => component(access, node, name, member, call, reply, refuse),
      None => refuse(UNKNOWN_METHOD, "no such method"),
    },
    (ACTION, member) => match node {
      Some((id, node)) if node.clickable => match member {
        "GetActions" => reply.build(&[("click", "Click", "")].as_slice()),
        "GetName" => reply.build(&"click"),
        "GetLocalizedName" => reply.build(&"Click"),
        "GetDescription" | "GetKeyBinding" => reply.build(&""),
        "DoAction" => match body.deserialize::<i32>() {
          Ok(0) => reply.build(&access.click(id)),
          _ => refuse(INVALID_ARGS, "no such action"),
        },
        _ => refuse(UNKNOWN_METHOD, "no such method"),
      },
      _ => refuse(UNKNOWN_METHOD, "no such method"),
    },
    _ => refuse(UNKNOWN_METHOD, "no such method"),
  }
}

/// An empty `GetItems`, in the signature libatspi reads.
fn reply_empty_cache(header: &Header<'_>) -> zbus::Result<Message> {
  type Item = (
    Reference,
    Reference,
    Reference,
    i32,
    i32,
    Vec<String>,
    String,
    u32,
    Vec<u32>,
  );
  Message::method_return(header).build(&Vec::<Item>::new())
}

/// An interface's properties, by name.
fn properties(
  facts: &Facts,
  node: Option<&AccessNode>,
  app: &App,
  interface: &str,
) -> HashMap<&'static str, Variant<'static>> {
  let text = |text: &str| Variant::from(text.to_owned());
  match interface {
    ACCESSIBLE => HashMap::from([
      ("Name", text(&facts.name)),
      ("Description", text("")),
      ("Parent", Variant::from(facts.parent.clone())),
      (
        "ChildCount",
        Variant::from(i32::try_from(facts.children.len()).unwrap_or(i32::MAX)),
      ),
      ("Locale", text("")),
      ("AccessibleId", text("")),
      ("HelpText", text("")),
    ]),
    APPLICATION if node.is_none() => HashMap::from([
      ("ToolkitName", text("crabshell")),
      ("Version", text(env!("CARGO_PKG_VERSION"))),
      ("AtspiVersion", text("2.1")),
      ("Id", Variant::from(app.id.get())),
    ]),
    ACTION if node.is_some_and(|node| node.clickable) => {
      HashMap::from([("NActions", Variant::from(1i32))])
    }
    _ => HashMap::new(),
  }
}

/// `Component`'s answers: where a widget is, in the window's logical
/// pixels.
fn component(
  access: &Accessibility,
  node: &AccessNode,
  name: &str,
  member: &str,
  call: &Message,
  reply: zbus::message::Builder<'_>,
  refuse: impl Fn(&str, &str) -> zbus::Result<Message>,
) -> zbus::Result<Message> {
  let body = call.body();
  // Rounded to whole pixels, as AT-SPI's integers are.
  let extents = |coords: u32| {
    let parent = node
      .parent
      .filter(|_| coords == COORD_PARENT)
      .and_then(|parent| access.node(parent));
    let (x, y) = parent.map_or((0.0, 0.0), |parent| (parent.rect.x, parent.rect.y));
    let rect = node.rect;
    let round = |n: f32| n.round() as i32;
    (
      round(rect.x - x),
      round(rect.y - y),
      round(rect.width),
      round(rect.height),
    )
  };
  match member {
    "GetExtents" => match body.deserialize::<u32>() {
      Ok(coords) => reply.build(&(extents(coords),)),
      Err(_) => refuse(INVALID_ARGS, "not a coordinate type"),
    },
    "GetPosition" => match body.deserialize::<u32>() {
      Ok(coords) => {
        let (x, y, _, _) = extents(coords);
        reply.build(&(x, y))
      }
      Err(_) => refuse(INVALID_ARGS, "not a coordinate type"),
    },
    "GetSize" => {
      let (_, _, width, height) = extents(0);
      reply.build(&(width, height))
    }
    "Contains" => match body.deserialize::<(i32, i32, u32)>() {
      Ok((px, py, coords)) => {
        let (x, y, width, height) = extents(coords);
        reply.build(&(px >= x && py >= y && px < x + width && py < y + height))
      }
      Err(_) => refuse(INVALID_ARGS, "not a point"),
    },
    // Mouse review isn't served: which window a point is in is unknown.
    "GetAccessibleAtPoint" => reply.build(&(reference(name, NULL.to_owned()),)),
    "GetLayer" => match node.role {
      AccessRole::Frame | AccessRole::Window => reply.build(&7u32),
      _ => reply.build(&3u32),
    },
    "GetMDIZOrder" => reply.build(&0i16),
    "GetAlpha" => reply.build(&1.0f64),
    "GrabFocus" => reply.build(&false),
    _ => refuse(UNKNOWN_METHOD, "no such method"),
  }
}
