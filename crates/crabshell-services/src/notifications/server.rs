//! The server's answers to calls on `/org/freedesktop/Notifications`,
//! and how a `Notify` call's arguments become a [`Notification`].

use std::collections::HashMap;
use std::fmt;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crabshell_ui::{IconPixels, IconSource};
use serde::de::{Deserialize, Deserializer, IgnoredAny, SeqAccess, Visitor};
use zbus::message::Flags;
use zbus::{Connection, Message, Signature, Type};

use super::{CLOSED, NOTIFICATIONS_PATH, Notification, Service, Timeout, Urgency};

/// The most bytes kept of a summary and of a body. With every other
/// cap, 256 notifications stay within a few MiB however apps fill them.
const MAX_SUMMARY: usize = 1024;
const MAX_BODY: usize = 4 * 1024;
/// The most bytes kept of every other string: names, icons and actions.
const MAX_TEXT: usize = 256;
const MAX_ACTIONS: usize = 16;
/// The largest picture side kept, the same as tray pixmaps'.
const MAX_IMAGE_SIDE: i32 = 512;
/// The most bytes a picture may send: one 512 px picture with alpha.
const MAX_IMAGE_BYTES: usize = 1024 * 1024;

/// What the server offers: action buttons, a body, a still image, and a
/// history that outlasts the popups.
const CAPABILITIES: [&str; 4] = ["actions", "body", "icon-static", "persistence"];

/// What the server offers, for callers that look before calling.
const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="Notify">
      <arg name="app_name" type="s" direction="in"/>
      <arg name="replaces_id" type="u" direction="in"/>
      <arg name="app_icon" type="s" direction="in"/>
      <arg name="summary" type="s" direction="in"/>
      <arg name="body" type="s" direction="in"/>
      <arg name="actions" type="as" direction="in"/>
      <arg name="hints" type="a{sv}" direction="in"/>
      <arg name="expire_timeout" type="i" direction="in"/>
      <arg name="id" type="u" direction="out"/>
    </method>
    <method name="CloseNotification"><arg name="id" type="u" direction="in"/></method>
    <method name="GetCapabilities"><arg name="capabilities" type="as" direction="out"/></method>
    <method name="GetServerInformation">
      <arg name="name" type="s" direction="out"/>
      <arg name="vendor" type="s" direction="out"/>
      <arg name="version" type="s" direction="out"/>
      <arg name="spec_version" type="s" direction="out"/>
    </method>
    <signal name="NotificationClosed"><arg name="id" type="u"/><arg name="reason" type="u"/></signal>
    <signal name="ActionInvoked"><arg name="id" type="u"/><arg name="action_key" type="s"/></signal>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="xml" type="s" direction="out"/></method>
  </interface>
</node>
"#;

const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_METHOD: &str = "org.freedesktop.DBus.Error.UnknownMethod";

/// Answers one method call to this connection, while it serves. Anything
/// else gets the bus's usual error, so no caller waits for a timeout. A
/// call that asks for no reply is still acted on.
pub(super) async fn answer(service: &Rc<Service>, connection: &Connection, call: &Message) {
  let header = call.header();
  let path = header.path().map(|path| path.as_str());
  let member = header.member().map_or("", |member| member.as_str());
  let body = call.body();
  let reply = Message::method_return(&header);
  let refuse = |error: &str, why: &str| Message::error(&header, error).build(&why);
  let answer = match (path, member) {
    _ if !service.serving() => refuse(UNKNOWN_METHOD, "not serving notifications"),
    (Some(NOTIFICATIONS_PATH), "Notify") => match body.deserialize::<NotifyArgs<'_>>() {
      Ok(args) => {
        let (replaces, notification) = notification(args);
        reply.build(&service.notify(replaces, notification))
      }
      Err(_) => refuse(INVALID_ARGS, "not a notification"),
    },
    (Some(NOTIFICATIONS_PATH), "CloseNotification") => match body.deserialize::<u32>() {
      Ok(id) if service.close(&[id], CLOSED) => reply.build(&()),
      _ => refuse(INVALID_ARGS, "no such notification"),
    },
    (Some(NOTIFICATIONS_PATH), "GetCapabilities") => reply.build(&CAPABILITIES.as_slice()),
    (Some(NOTIFICATIONS_PATH), "GetServerInformation") => {
      let version = env!("CARGO_PKG_VERSION");
      reply.build(&("crabshell", "crabshell", version, "1.2"))
    }
    (Some(NOTIFICATIONS_PATH), "Introspect") => reply.build(&INTROSPECTION),
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
    tracing::debug!(member, %error, "the notification server's answer wasn't sent");
  }
}

/// `Notify`'s arguments: the app's name, the id to replace, the app's
/// icon, the summary, the body, the actions, the hints and the timeout.
type NotifyArgs<'a> = (
  &'a str,
  u32,
  &'a str,
  &'a str,
  &'a str,
  Vec<&'a str>,
  HashMap<&'a str, Hint<'a>>,
  i32,
);

/// The id a `Notify` call replaces, and its notification, with id 0.
fn notification(args: NotifyArgs<'_>) -> (u32, Notification) {
  let (app_name, replaces, app_icon, summary, body, actions, hints, timeout) = args;
  let text = |key: &str| match hints.get(key) {
    Some(Hint::Text(text)) => cut(text, MAX_TEXT),
    _ => String::new(),
  };
  let flag = |key: &str| matches!(hints.get(key), Some(Hint::Number(n)) if *n != 0);
  let pixels = |key: &str| match hints.get(key) {
    Some(Hint::Image(raw)) => image_pixels(raw).map(IconSource::Pixels),
    _ => None,
  };
  let file = |key: &str| match hints.get(key) {
    Some(Hint::Text(path)) => icon(path),
    _ => None,
  };
  let app_icon = cut(app_icon, MAX_TEXT);
  // The spec's order, with the names of its earlier versions.
  let image = pixels("image-data")
    .or_else(|| pixels("image_data"))
    .or_else(|| file("image-path"))
    .or_else(|| file("image_path"))
    .or_else(|| icon(&app_icon))
    .or_else(|| pixels("icon_data"));
  let notification = Notification {
    id: 0,
    app_name: cut(app_name, MAX_TEXT),
    summary: cut(summary, MAX_SUMMARY),
    // Read past the cap only so far, since tags and entities shrink.
    body: cut(
      &plain(&body[..body.floor_char_boundary(MAX_BODY * 4)]),
      MAX_BODY,
    ),
    actions: actions
      .as_chunks::<2>()
      .0
      .iter()
      .take(MAX_ACTIONS)
      .map(|[key, label]| (cut(key, MAX_TEXT), cut(label, MAX_TEXT)))
      .collect(),
    urgency: match hints.get("urgency") {
      Some(Hint::Number(0)) => Urgency::Low,
      Some(Hint::Number(2)) => Urgency::Critical,
      _ => Urgency::Normal,
    },
    timeout: match u64::try_from(timeout) {
      Ok(0) => Timeout::Never,
      Ok(ms) => Timeout::After(Duration::from_millis(ms)),
      Err(_) => Timeout::Default,
    },
    image,
    desktop_entry: text("desktop-entry"),
    category: text("category"),
    resident: flag("resident"),
    transient: flag("transient"),
    received: SystemTime::now(),
    app_icon,
  };
  (replaces, notification)
}

/// `body` as the text it shows. The server doesn't offer `body-markup`,
/// but many apps send it anyway, so tags are taken out and XML's five
/// entities and numeric ones decoded. An unknown entity and a `<` with no
/// `>` stay as they are.
fn plain(body: &str) -> String {
  let mut shown = String::with_capacity(body.len());
  let mut rest = body;
  while let Some(at) = rest.find(['<', '&']) {
    shown.push_str(&rest[..at]);
    rest = &rest[at..];
    if rest.starts_with('<') {
      match rest.find('>') {
        Some(end) => rest = &rest[end + 1..],
        None => {
          shown.push_str(rest);
          rest = "";
        }
      }
      continue;
    }
    let decoded = rest
      .find(';')
      .filter(|end| *end <= 10)
      .and_then(|end| Some((entity(&rest[1..end])?, end)));
    match decoded {
      Some((character, end)) => {
        shown.push(character);
        rest = &rest[end + 1..];
      }
      None => {
        shown.push('&');
        rest = &rest[1..];
      }
    }
  }
  shown.push_str(rest);
  shown
}

/// The character an entity's name between `&` and `;` stands for.
fn entity(name: &str) -> Option<char> {
  match name {
    "amp" => Some('&'),
    "lt" => Some('<'),
    "gt" => Some('>'),
    "quot" => Some('"'),
    "apos" => Some('\''),
    _ => {
      let number = name.strip_prefix('#')?;
      let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse().ok()?,
      };
      char::from_u32(code)
    }
  }
}

/// `text`'s first `max` bytes at most, cut at a char boundary.
fn cut(text: &str, max: usize) -> String {
  text[..text.floor_char_boundary(max)].to_owned()
}

/// What an icon name or image path names: a file for an absolute path or
/// a `file://` URI, a themed icon for a plain name, and `None` for
/// nothing, another URI, or a relative path.
pub(super) fn icon(name: &str) -> Option<IconSource> {
  if name.starts_with('/') {
    return Some(IconSource::File(name.into()));
  }
  if name.starts_with("file://") {
    return file_path(name).map(IconSource::File);
  }
  let named = !name.is_empty() && !name.contains('/');
  named.then(|| IconSource::Name(name.to_owned()))
}

/// The path a local `file://` URI names, with its `%xx` escapes undone.
fn file_path(uri: &str) -> Option<PathBuf> {
  let path = uri.strip_prefix("file://")?;
  if !path.starts_with('/') {
    return None;
  }
  let mut bytes = Vec::with_capacity(path.len());
  let mut rest = path.bytes();
  while let Some(byte) = rest.next() {
    if byte != b'%' {
      bytes.push(byte);
      continue;
    }
    let mut digit = || char::from(rest.next()?).to_digit(16);
    let (high, low) = (digit()?, digit()?);
    // Two hex digits, so at most 255.
    bytes.push((high * 16 + low) as u8);
  }
  Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// A picture in the spec's `(iiibiiay)`: width, height, row stride,
/// alpha, bits per sample, channels, and the RGB or RGBA bytes.
pub(super) type RawImage<'a> = (i32, i32, i32, bool, i32, i32, &'a [u8]);

/// A picture as the renderer takes it. One over 512 px a side or 1 MiB,
/// in a format other than 8-bit RGB or RGBA, or with too few bytes for
/// its size, is logged and refused.
pub(super) fn image_pixels(raw: &RawImage<'_>) -> Option<IconPixels> {
  let &(width, height, rowstride, alpha, bits, channels, data) = raw;
  let refuse = || {
    let bytes = data.len();
    tracing::debug!(
      width,
      height,
      rowstride,
      bits,
      channels,
      bytes,
      "a bad notification picture; skipped"
    );
    None
  };
  let side = 1..=MAX_IMAGE_SIDE;
  let format = bits == 8 && channels == if alpha { 4 } else { 3 };
  let sized = side.contains(&width) && side.contains(&height) && data.len() <= MAX_IMAGE_BYTES;
  if !format || !sized {
    return refuse();
  }
  // The sides are from 1 to 512 and there are 3 or 4 channels, so the
  // casts are exact.
  let (width, height, channels) = (width as usize, height as usize, channels as usize);
  let row = width * channels;
  // The last row may end at its pixels, as GdkPixbuf's does.
  let stride = usize::try_from(rowstride).ok().filter(|&stride| {
    let needed = stride
      .checked_mul(height - 1)
      .and_then(|n| n.checked_add(row));
    stride >= row && needed.is_some_and(|needed| needed <= data.len())
  });
  let Some(stride) = stride else {
    return refuse();
  };
  let mut argb = Vec::with_capacity(width * height * 4);
  for y in 0..height {
    for pixel in data[y * stride..][..row].chunks_exact(channels) {
      let opacity = pixel.get(3).copied().unwrap_or(255);
      argb.extend_from_slice(&[opacity, pixel[0], pixel[1], pixel[2]]);
    }
  }
  Some(IconPixels {
    width: width as u32,
    height: height as u32,
    data: argb.into(),
  })
}

/// A hint's value, as far as the server reads one. Any other type is
/// skipped unread, so a large value of the wrong type costs nothing, and
/// a picture's bytes stay borrowed from the message.
enum Hint<'a> {
  /// A boolean or an integer of any width.
  Number(i64),
  Text(&'a str),
  Image(RawImage<'a>),
  Other,
}

impl Type for Hint<'_> {
  const SIGNATURE: &'static Signature = &Signature::Variant;
}

impl<'de: 'a, 'a> Deserialize<'de> for Hint<'a> {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    deserializer.deserialize_any(HintVisitor)
  }
}

struct HintVisitor;

impl<'de> Visitor<'de> for HintVisitor {
  type Value = Hint<'de>;

  fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    formatter.write_str("a variant")
  }

  /// zbus hands a variant over as its signature, then its value.
  fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Hint<'de>, A::Error> {
    let Some(signature) = seq.next_element::<Signature>()? else {
      return Ok(Hint::Other);
    };
    let number = |n: Option<i64>| n.map(Hint::Number);
    let hint = match signature {
      Signature::Bool => number(seq.next_element::<bool>()?.map(i64::from)),
      Signature::U8 => number(seq.next_element::<u8>()?.map(i64::from)),
      Signature::I16 => number(seq.next_element::<i16>()?.map(i64::from)),
      Signature::U16 => number(seq.next_element::<u16>()?.map(i64::from)),
      Signature::I32 => number(seq.next_element::<i32>()?.map(i64::from)),
      Signature::U32 => number(seq.next_element::<u32>()?.map(i64::from)),
      Signature::I64 => number(seq.next_element::<i64>()?),
      Signature::U64 => number(
        seq
          .next_element::<u64>()?
          .map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
      ),
      Signature::Str => seq.next_element::<&str>()?.map(Hint::Text),
      _ if signature == "(iiibiiay)" => seq.next_element::<RawImage<'de>>()?.map(Hint::Image),
      _ => seq.next_element::<IgnoredAny>()?.map(|_| Hint::Other),
    };
    Ok(hint.unwrap_or(Hint::Other))
  }
}
