use std::collections::HashMap;

use serde_json::json;
use zbus::Variant;

use super::typed;
use crate::network::saved::json;

/// `value`'s types and values all the way down, dictionaries sorted, so
/// two built in another order compare equal.
fn deep(value: &Variant<'_>) -> String {
  match value {
    Variant::Variant(inner) => format!("<{}>", deep(inner)),
    Variant::Array(array) => {
      let items = array.inner().iter().map(deep).collect::<Vec<_>>();
      format!("a{}[{}]", array.element_signature(), items.join(","))
    }
    Variant::Dict(dict) => {
      let mut entries = dict
        .iter()
        .map(|(key, value)| format!("{}:{}", deep(key), deep(value)))
        .collect::<Vec<_>>();
      entries.sort();
      format!("{{{}}}", entries.join(","))
    }
    Variant::Structure(structure) => {
      let fields = structure.fields().iter().map(deep).collect::<Vec<_>>();
      format!("({})", fields.join(","))
    }
    value => format!("{}:{value}", value.value_signature()),
  }
}

fn dict(entries: Vec<(&str, Variant<'static>)>) -> HashMap<String, Variant<'static>> {
  let entries = entries
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value));
  entries.collect()
}

/// Settings as libnm 1.58's `nm_connection_to_dbus` types them, with every
/// kind of value NetworkManager has but the old IPv6 forms, which only go
/// with their new ones.
fn networkmanagers() -> Vec<(&'static str, Vec<(&'static str, Variant<'static>)>)> {
  let rule = dict(vec![
    ("family", Variant::from(2i32)),
    ("priority", Variant::from(5u32)),
    ("tos", Variant::from(0x10u8)),
    ("ipproto", Variant::from(6u8)),
    ("sport-start", Variant::from(10u16)),
    ("dport-end", Variant::from(30u16)),
    ("from", Variant::from("10.0.0.0")),
    ("from-len", Variant::from(8u8)),
    ("to-len", Variant::from(16u8)),
    ("action", Variant::from(1u8)),
    ("table", Variant::from(100u32)),
    ("suppress-prefixlength", Variant::from(4i32)),
    ("invert", Variant::from(false)),
  ]);
  let route = dict(vec![
    ("dest", Variant::from("10.1.0.0")),
    ("prefix", Variant::from(16u32)),
    ("metric", Variant::from(5u32)),
    ("scope", Variant::from(0u8)),
    ("tos", Variant::from(4u8)),
  ]);
  let address = dict(vec![
    ("address", Variant::from("10.0.0.2")),
    ("prefix", Variant::from(24u32)),
  ]);
  let vlan = dict(vec![
    ("vid-start", Variant::from(10u16)),
    ("vid-end", Variant::from(20u16)),
    ("pvid", Variant::from(false)),
  ]);
  let watcher = dict(vec![
    ("name", Variant::from("arp_ping")),
    ("interval", Variant::from(2i32)),
    ("init-wait", Variant::from(1i32)),
    ("vlanid", Variant::from(4i32)),
  ]);
  let trunk = dict(vec![
    ("start", Variant::from(10u64)),
    ("end", Variant::from(20u64)),
  ]);
  let peer = dict(vec![
    (
      "public-key",
      Variant::from("Ny0nt5yGJLljtqI5uU+eK4X1q+6x2/MVYNiCQjmqyw4="),
    ),
    ("persistent-keepalive", Variant::from(25u32)),
    ("allowed-ips", Variant::from(vec!["10.0.0.0/8"])),
  ]);
  let vf_vlan = dict(vec![
    ("id", Variant::from(5u32)),
    ("qos", Variant::from(3u32)),
  ]);
  let vf = dict(vec![
    ("index", Variant::from(1u32)),
    ("spoof-check", Variant::from(true)),
    ("vlans", Variant::from(vec![vf_vlan])),
  ]);
  let action = dict(vec![
    ("kind", Variant::from("simple")),
    ("sdata", Variant::from(b"Hello\0".to_vec())),
  ]);
  let tfilter = dict(vec![
    ("kind", Variant::from("matchall")),
    ("parent", Variant::from(0x10000u32)),
    ("action", Variant::from(action)),
  ]);
  vec![
    ("tc", vec![("tfilters", Variant::from(vec![tfilter]))]),
    (
      "802-3-ethernet",
      vec![(
        "mac-address",
        Variant::from(vec![0x52u8, 0x54, 0, 0x12, 0x34, 0x56]),
      )],
    ),
    (
      "connection",
      vec![
        ("id", Variant::from("x")),
        ("autoconnect", Variant::from(false)),
        ("autoconnect-retries", Variant::from(-1i32)),
        ("timestamp", Variant::from(5_000_000_000u64)),
        ("secondaries", Variant::from(vec!["a", "b"])),
      ],
    ),
    (
      "802-11-wireless",
      vec![
        ("ssid", Variant::from(b"caf\xe9".to_vec())),
        ("mtu", Variant::from(1400u32)),
      ],
    ),
    (
      "ipv4",
      vec![
        ("dns", Variant::from(vec![16843009u32])),
        ("addresses", Variant::from(vec![vec![33554442u32, 24, 0]])),
        ("route-data", Variant::from(vec![route])),
        ("routing-rules", Variant::from(vec![rule])),
        ("route-metric", Variant::from(-1i32)),
      ],
    ),
    (
      "ipv6",
      vec![
        ("dns", Variant::from(vec![vec![0u8; 16]])),
        ("address-data", Variant::from(vec![address])),
      ],
    ),
    ("bridge", vec![("vlans", Variant::from(vec![vlan]))]),
    (
      "team",
      vec![("link-watchers", Variant::from(vec![watcher]))],
    ),
    ("ovs-port", vec![("trunks", Variant::from(vec![trunk]))]),
    ("wireguard", vec![("peers", Variant::from(vec![peer]))]),
    ("sriov", vec![("vfs", Variant::from(vec![vf]))]),
    (
      "vpn",
      vec![(
        "data",
        Variant::from(HashMap::from([(
          "gateway".to_owned(),
          "vpn.example.org".to_owned(),
        )])),
      )],
    ),
  ]
}

#[test]
fn settings_read_as_json_go_back_with_networkmanagers_types() {
  let groups = networkmanagers();
  let mut settings = serde_json::Map::new();
  for (group, props) in &groups {
    let props = props
      .iter()
      .map(|(name, value)| (name.to_string(), json(value, &mut 0)));
    settings.insert(group.to_string(), props.collect());
  }
  let typed = typed(&serde_json::Value::Object(settings)).expect("typed");
  for (group, props) in &groups {
    for (name, value) in props {
      let back = typed[*group].get(*name).map(deep);
      assert_eq!(back, Some(deep(value)), "{group}.{name}");
    }
  }
}

#[test]
fn empty_values_and_old_forms_beside_new_ones_are_left_out() {
  let typed = typed(&json!({
    "connection": { "permissions": [], "id": "x", "uuid": null },
    "802-3-ethernet": {},
    "vpn": { "data": {} },
    "ipv4": {
      "addresses": [[33554442, 24, 0]],
      "address-data": [{ "address": "10.0.0.2", "prefix": 24, "label": null, "tags": [] }],
      "routes": [],
    },
    // IPv6's old forms are structures, which JSON can't say, so they go
    // only beside their new ones.
    "ipv6": {
      "addresses": [[[0, 1], 64, [0, 2]]],
      "address-data": [],
      "routes": [[[0, 1], 64, [0, 2], 5]],
      "route-data": [],
    },
  }))
  .expect("typed");
  let names = |group: &str| {
    let mut names = typed[group].keys().cloned().collect::<Vec<_>>();
    names.sort();
    names
  };
  assert_eq!(names("connection"), ["id"]);
  assert_eq!(names("802-3-ethernet"), Vec::<String>::new());
  assert_eq!(names("vpn"), Vec::<String>::new());
  assert_eq!(names("ipv4"), ["address-data"]);
  assert_eq!(names("ipv6"), Vec::<String>::new());
  let address = &typed["ipv4"]["address-data"];
  assert_eq!(
    deep(address),
    deep(&Variant::from(vec![dict(vec![
      ("address", Variant::from("10.0.0.2")),
      ("prefix", Variant::from(24u32)),
    ])]))
  );
}

#[test]
fn numbers_take_the_smallest_type_networkmanager_converts_from() {
  let typed = typed(&json!({
    "connection": { "a": 5, "b": -1, "c": 5_000_000_000u64, "d": -5_000_000_000i64, "e": 0.5 },
  }))
  .expect("typed");
  let signature = |name: &str| typed["connection"][name].value_signature().to_string();
  let signatures = ["a", "b", "c", "d", "e"].map(signature);
  assert_eq!(signatures, ["u", "i", "t", "x", "d"]);
}

#[test]
fn shapes_networkmanager_has_no_type_for_are_refused_by_name() {
  let refused = [
    (json!([]), "settings aren't an object of groups"),
    (json!({ "ipv4": 5 }), "ipv4 isn't an object"),
    (
      json!({ "vpn": { "data": { "port": 1194 } } }),
      "vpn.data: a dictionary of other than strings",
    ),
    (
      json!({ "x": { "y": ["one", 2] } }),
      "x.y: an array with an item of another type or out of range",
    ),
    (
      json!({ "x": { "ssid": [256] } }),
      "x.ssid: an array with an item of another type or out of range",
    ),
    (
      json!({ "ipv4": { "dns": [-1] } }),
      "ipv4.dns: an array with an item of another type or out of range",
    ),
    (
      json!({ "x": { "y": [{}, 1] } }),
      "x.y: an array with an item of another type or out of range",
    ),
    (
      json!({ "x": { "y": [true] } }),
      "x.y: an array NetworkManager has no type for",
    ),
    (
      json!({ "x": { "y": [{ "tos": 256 }] } }),
      "x.y: a number out of its range",
    ),
    (
      json!({ "x": { "y": [{ "z": [[1]] }] } }),
      "x.y: an array NetworkManager has no type for",
    ),
    (
      json!({ "x": { "y": [{ "z": { "z": { "z": { "z": { "z": 1 } } } } }] } }),
      "x.y: objects nested too deep",
    ),
    (
      json!({ "bridge": { "mac-address": "52:54:00:12:34:5" } }),
      "bridge.mac-address: a hardware address that isn't hex bytes split by colons",
    ),
    (
      json!({ "x": { "y": [{ "z": [{ "z": [{ "z": [{ "z": [{}] }] }] }] }] } }),
      "x.y: objects nested too deep",
    ),
  ];
  for (settings, expected) in refused {
    assert_eq!(
      typed(&settings).err().as_deref(),
      Some(expected),
      "{settings}"
    );
  }
  // Three deep is fine.
  let fine = json!({ "x": { "y": [{ "z": [{ "z": [{ "z": [{}] }] }] }] } });
  assert!(typed(&fine).is_ok());
}

#[test]
fn ssids_and_hardware_addresses_may_come_as_text() {
  let shown = typed(&json!({
    "802-11-wireless": {
      "ssid": "café",
      "bssid": "A0:B1:C2:D3:E4:F5",
      "cloned-mac-address": "a0-b1-c2-d3-e4-f5",
      "mode": "infrastructure",
    },
    "802-3-ethernet": { "mac-address": [82, 84, 0, 18, 52, 86] },
  }))
  .expect("typed");
  let wifi = &shown["802-11-wireless"];
  assert_eq!(
    deep(&wifi["ssid"]),
    deep(&Variant::from("café".as_bytes().to_vec()))
  );
  let address = deep(&Variant::from(vec![0xa0u8, 0xb1, 0xc2, 0xd3, 0xe4, 0xf5]));
  assert_eq!(deep(&wifi["bssid"]), address);
  assert_eq!(deep(&wifi["cloned-mac-address"]), address);
  assert_eq!(
    deep(&wifi["mode"]),
    "s:\"infrastructure\"",
    "other text stays text"
  );
  let wired = &shown["802-3-ethernet"]["mac-address"];
  assert_eq!(wired.value_signature().to_string(), "ay");

  for refused in ["", "52:54:00:12:34:5", "52:54:00:12:34:+5", "random"] {
    let settings = json!({ "802-3-ethernet": { "mac-address": refused } });
    assert!(typed(&settings).is_err(), "{refused:?}");
  }
}
