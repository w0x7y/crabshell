//! Settings as a config writes them, in JSON, turned into the typed
//! values NetworkManager's `AddConnection` and `Update` take. JSON has
//! one kind of number and one kind of array, so the D-Bus types come from
//! NetworkManager's documented settings (nm-settings-dbus(5)).

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use serde_json::{Map, Number, Value as Json};
use zbus::Variant;

/// Settings by group and name, as NetworkManager takes them.
pub(super) type Typed = HashMap<String, HashMap<String, Variant<'static>>>;

/// The properties whose arrays of numbers are `au`. Every other array of
/// numbers NetworkManager has is a byte array.
const U32_ARRAYS: [(&str, &str); 7] = [
  ("ipv4", "dns"),
  ("dcb", "priority-bandwidth"),
  ("dcb", "priority-flow-control"),
  ("dcb", "priority-group-bandwidth"),
  ("dcb", "priority-group-id"),
  ("dcb", "priority-strict-bandwidth"),
  ("dcb", "priority-traffic-class"),
];

/// The byte arrays a config may give as text, sent as its UTF-8 bytes.
const SSIDS: [(&str, &str); 2] = [("802-11-wireless", "ssid"), ("802-11-olpc-mesh", "ssid")];

/// The byte arrays a config may give as a hardware address, such as
/// `52:54:00:12:34:56`, sent as the bytes it spells.
const HARDWARE_ADDRESSES: [(&str, &str); 11] = [
  ("802-3-ethernet", "mac-address"),
  ("802-3-ethernet", "cloned-mac-address"),
  ("802-11-wireless", "mac-address"),
  ("802-11-wireless", "cloned-mac-address"),
  ("802-11-wireless", "bssid"),
  ("802-11-olpc-mesh", "dhcp-anycast-address"),
  ("bluetooth", "bdaddr"),
  ("bridge", "mac-address"),
  ("bridge", "group-address"),
  ("infiniband", "mac-address"),
  ("wimax", "mac-address"),
];

/// How deep objects may nest inside a property, as SR-IOV's VLANs inside
/// its VFs, or a traffic filter's action.
const MAX_DEPTH: usize = 4;

/// `settings`, an object of groups of properties, typed for
/// NetworkManager. An error names the property, never its value, which
/// may be a secret.
pub(super) fn typed(settings: &Json) -> Result<Typed, String> {
  let Json::Object(groups) = settings else {
    return Err("settings aren't an object of groups".into());
  };
  let mut typed = Typed::new();
  for (group, props) in groups {
    let Json::Object(props) = props else {
      return Err(format!("{group} isn't an object"));
    };
    let mut values = HashMap::new();
    for (name, value) in props {
      if legacy(group, name, props) {
        continue;
      }
      let value = property(group, name, value).map_err(|why| format!("{group}.{name}: {why}"))?;
      if let Some(value) = value {
        values.insert(name.clone(), value);
      }
    }
    typed.insert(group.clone(), values);
  }
  Ok(typed)
}

/// Whether `name` is IP's old form of addresses or routes, given beside
/// the new one. NetworkManager reads only the new one then, but still
/// checks the old one's type, which for IPv6 JSON can't say.
fn legacy(group: &str, name: &str, props: &Map<String, Json>) -> bool {
  let newer = match name {
    "addresses" => "address-data",
    "routes" => "route-data",
    _ => return false,
  };
  matches!(group, "ipv4" | "ipv6") && props.contains_key(newer)
}

/// One property's value. `None` for `null` and an empty array or object,
/// which NetworkManager reads as its default, so the element type JSON
/// can't give isn't needed.
fn property(
  group: &str,
  name: &str,
  value: &Json,
) -> Result<Option<Variant<'static>>, &'static str> {
  Ok(Some(match value {
    Json::Null => return Ok(None),
    Json::Array(items) if items.is_empty() => return Ok(None),
    Json::Object(entries) if entries.is_empty() => return Ok(None),
    Json::Bool(b) => Variant::from(*b),
    Json::String(text) if SSIDS.contains(&(group, name)) => Variant::from(text.as_bytes().to_vec()),
    Json::String(text) if HARDWARE_ADDRESSES.contains(&(group, name)) => {
      let bytes = hardware_address(text);
      Variant::from(bytes.ok_or("a hardware address that isn't hex bytes split by colons")?)
    }
    Json::String(text) => Variant::from(text.clone()),
    // NetworkManager converts a number to the type a property has.
    Json::Number(n) => number(n),
    // Every dictionary property NetworkManager has holds strings.
    Json::Object(entries) => {
      let strings = entries
        .iter()
        .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())));
      let strings = strings.collect::<Option<HashMap<_, _>>>();
      Variant::from(strings.ok_or("a dictionary of other than strings")?)
    }
    Json::Array(items) => {
      let u32s = U32_ARRAYS.contains(&(group, name));
      match &items[0] {
        Json::Number(_) if u32s => {
          Variant::from(all(items, |item| u32::try_from(item.as_u64()?).ok())?)
        }
        Json::Number(_) => Variant::from(bytes(items)?),
        Json::String(_) => Variant::from(all(items, |item| Some(item.as_str()?.to_owned()))?),
        Json::Object(_) => Variant::from(dicts(items, 0)?),
        // IPv6's DNS servers as byte arrays, and IPv4's old addresses and
        // routes as arrays of three or four numbers.
        Json::Array(_) if (group, name) == ("ipv6", "dns") => {
          Variant::from(all(items, |item| bytes(item.as_array()?).ok())?)
        }
        Json::Array(_) => {
          let numbers =
            |item: &Json| all(item.as_array()?, |n| u32::try_from(n.as_u64()?).ok()).ok();
          Variant::from(all(items, numbers)?)
        }
        _ => return Err("an array NetworkManager has no type for"),
      }
    }
  }))
}

/// A number as `u`, or as `i`, `x`, `t` or `d` when it doesn't fit.
fn number(n: &Number) -> Variant<'static> {
  if let Some(n) = n.as_u64() {
    return u32::try_from(n).map_or(Variant::from(n), Variant::from);
  }
  if let Some(n) = n.as_i64() {
    return i32::try_from(n).map_or(Variant::from(n), Variant::from);
  }
  Variant::from(n.as_f64().unwrap_or(0.0))
}

/// Each item as `item` reads it, or an error if one doesn't read.
fn all<T>(items: &[Json], item: impl Fn(&Json) -> Option<T>) -> Result<Vec<T>, &'static str> {
  let all = items.iter().map(item).collect::<Option<Vec<_>>>();
  all.ok_or("an array with an item of another type or out of range")
}

fn bytes(items: &[Json]) -> Result<Vec<u8>, &'static str> {
  all(items, |item| u8::try_from(item.as_u64()?).ok())
}

/// The bytes `text` spells as two hex digits each, split by `:` or `-`.
fn hardware_address(text: &str) -> Option<Vec<u8>> {
  let byte = |pair: &str| {
    let hex = pair.len() == 2 && pair.bytes().all(|digit| digit.is_ascii_hexdigit());
    hex.then(|| u8::from_str_radix(pair, 16).ok()).flatten()
  };
  text.split([':', '-']).map(byte).collect()
}

/// An array of objects, `aa{sv}`, as IP's address and route data.
fn dicts(
  items: &[Json],
  depth: usize,
) -> Result<Vec<HashMap<String, Variant<'static>>>, &'static str> {
  let dict = |item: &Json| match item {
    Json::Object(entries) => dict(entries, depth),
    _ => Err("an array with an item of another type or out of range"),
  };
  items.iter().map(dict).collect()
}

/// An object inside a property, `a{sv}`.
fn dict(
  entries: &Map<String, Json>,
  depth: usize,
) -> Result<HashMap<String, Variant<'static>>, &'static str> {
  if depth == MAX_DEPTH {
    return Err("objects nested too deep");
  }
  let mut dict = HashMap::new();
  for (key, value) in entries {
    if let Some(value) = entry(key, value, depth)? {
      dict.insert(key.clone(), value);
    }
  }
  Ok(dict)
}

/// A value inside an object inside a property. NetworkManager checks
/// these types exactly, so the keys it gives another type than `u` get it
/// here.
fn entry(key: &str, value: &Json, depth: usize) -> Result<Option<Variant<'static>>, &'static str> {
  Ok(Some(match value {
    Json::Null => return Ok(None),
    Json::Array(items) if items.is_empty() => return Ok(None),
    Json::Object(entries) if entries.is_empty() => return Ok(None),
    Json::Bool(b) => Variant::from(*b),
    Json::String(text) => Variant::from(text.clone()),
    Json::Number(n) => keyed(key, n).ok_or("a number out of its range")?,
    Json::Array(items) => match &items[0] {
      Json::String(_) => Variant::from(all(items, |item| Some(item.as_str()?.to_owned()))?),
      // As a traffic filter action's `sdata`.
      Json::Number(_) => Variant::from(bytes(items)?),
      Json::Object(_) => Variant::from(dicts(items, depth + 1)?),
      _ => return Err("an array NetworkManager has no type for"),
    },
    // As a traffic filter's `action`.
    Json::Object(entries) => Variant::from(dict(entries, depth + 1)?),
  }))
}

/// A number inside an array of objects: routes' and routing rules' bytes
/// and ports, bridge VLAN ids, teams' signed link watcher times, and Open
/// vSwitch trunks' 64-bit ranges.
fn keyed(key: &str, n: &Number) -> Option<Variant<'static>> {
  Some(match key {
    "tos" | "scope" | "from-len" | "to-len" | "ipproto" | "action" => {
      Variant::from(u8::try_from(n.as_u64()?).ok()?)
    }
    "sport-start" | "sport-end" | "dport-start" | "dport-end" | "vid-start" | "vid-end" => {
      Variant::from(u16::try_from(n.as_u64()?).ok()?)
    }
    "family"
    | "suppress-prefixlength"
    | "interval"
    | "init-wait"
    | "missed-max"
    | "vlanid"
    | "delay-up"
    | "delay-down" => Variant::from(i32::try_from(n.as_i64()?).ok()?),
    "start" | "end" => Variant::from(n.as_u64()?),
    _ => number(n),
  })
}
