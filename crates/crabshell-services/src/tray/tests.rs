use std::cell::RefCell;
use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::{Cx, IconPixels, IconSource};
use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{Connection, MessageStream, OwnedVariant, Variant};

use super::{
  BUS, BUS_PATH, ITEM, ITEM_PATH, PROPERTIES, Properties, TrayItem, TrayStatus, WATCHER,
  WATCHER_PATH,
};
use crate::testing::{Harness, PrivateBus, capture_logs, offline};
use crate::{Config, ServicesExt};

fn online(bus: &PrivateBus) -> Config {
  Config {
    session_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

/// What a test app sends: a title, an icon name, one 2×1 pixmap and a
/// tooltip.
fn sample() -> Properties {
  Properties {
    id: Some("test-app".into()),
    title: Some("Test".into()),
    status: Some("NeedsAttention".into()),
    icon_name: Some("network-wireless".into()),
    icon_pixmap: Some(vec![(2, 1, vec![255, 1, 2, 3, 255, 4, 5, 6])]),
    attention_icon_name: Some("dialog-warning".into()),
    icon_theme_path: Some("/nonexistent".into()),
    tool_tip: Some((
      String::new(),
      Vec::new(),
      "Connected".into(),
      "Wi-Fi".into(),
    )),
    item_is_menu: Some(true),
  }
}

/// [`sample`] as the tray shows it, at `id`.
fn sample_item(id: &str) -> TrayItem {
  TrayItem {
    id: id.to_owned(),
    app_id: "test-app".into(),
    title: "Test".into(),
    status: TrayStatus::NeedsAttention,
    icon_name: "network-wireless".into(),
    icon_pixmaps: vec![IconPixels {
      width: 2,
      height: 1,
      data: Rc::from(&[255, 1, 2, 3, 255, 4, 5, 6][..]),
    }],
    attention_icon_name: "dialog-warning".into(),
    icon_theme_path: "/nonexistent".into(),
    tooltip: "Connected".into(),
    item_is_menu: true,
  }
}

/// An app's tray item on the private bus. It answers `GetAll` with
/// `properties`, or with a wrongly typed title while that is `None`, and
/// writes down every call it gets.
struct FakeItem {
  connection: Connection,
  path: &'static str,
  properties: Rc<RefCell<Option<Properties>>>,
  calls: Rc<RefCell<Vec<String>>>,
  /// While `Some`, each `GetAll` waits here for the test to answer it.
  held: Rc<RefCell<Option<Vec<zbus::Message>>>>,
}

impl FakeItem {
  fn start(
    shell: &mut Harness,
    bus: &PrivateBus,
    names: &[&'static str],
    path: &'static str,
    properties: Option<Properties>,
  ) -> FakeItem {
    let connection = shell.block_on(bus.connect(&[]));
    let item = FakeItem {
      connection: connection.clone(),
      path,
      properties: Rc::new(RefCell::new(properties)),
      calls: Rc::default(),
      held: Rc::default(),
    };
    let (properties, calls) = (Rc::clone(&item.properties), Rc::clone(&item.calls));
    let held = Rc::clone(&item.held);
    // Before the names, so a call that comes at once isn't missed.
    let mut messages = MessageStream::from(&connection);
    for &name in names {
      let connection = connection.clone();
      shell
        .block_on(async move { connection.request_name(name).await })
        .expect("a name on the private bus");
    }
    shell.spawn(async move {
      while let Some(Ok(call)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
        let header = call.header();
        if header.message_type() != Kind::MethodCall
          || header.path().map(|path| path.as_str()) != Some(path)
        {
          continue;
        }
        let member = header.member().map_or("", |member| member.as_str());
        let sent = match member {
          "GetAll" => {
            calls.borrow_mut().push("GetAll".into());
            if let Some(held) = held.borrow_mut().as_mut() {
              held.push(call.clone());
              continue;
            }
            let properties = properties.borrow().clone();
            match properties {
              Some(properties) => connection.reply(&header, &properties).await,
              None => {
                let wrong = HashMap::from([("Title", Variant::from(7u32))]);
                connection.reply(&header, &wrong).await
              }
            }
          }
          "Activate" | "SecondaryActivate" => {
            let at: (i32, i32) = call.body().deserialize().expect("(ii)");
            calls.borrow_mut().push(format!("{member}{at:?}"));
            connection.reply(&header, &()).await
          }
          "Scroll" => {
            let scroll: (i32, String) = call.body().deserialize().expect("(is)");
            calls.borrow_mut().push(format!("{member}{scroll:?}"));
            connection.reply(&header, &()).await
          }
          _ => continue,
        };
        sent.expect("the fake item's reply");
      }
    });
    item
  }

  /// Registers with whichever watcher runs, retrying until one does.
  fn register(&self, shell: &mut Harness, service: &'static str) {
    let connection = self.connection.clone();
    eventually(shell, "a watcher to register with", move || {
      let connection = connection.clone();
      async move {
        let method = "RegisterStatusNotifierItem";
        let call =
          connection.call_method(Some(WATCHER), WATCHER_PATH, Some(WATCHER), method, &service);
        call.await.ok()
      }
    });
  }

  fn emit(&self, shell: &mut Harness, signal: &'static str) {
    emit_from(shell, &self.connection, self.path, signal);
  }

  fn calls(&self) -> Vec<String> {
    self.calls.borrow().clone()
  }

  /// Answers the held `GetAll` number `index` with `title`.
  fn answer_held(&self, shell: &mut Harness, index: usize, title: &str) {
    let call = self.held.borrow().as_ref().expect("held calls")[index].clone();
    let properties = Properties {
      title: Some(title.into()),
      ..sample()
    };
    let connection = self.connection.clone();
    shell
      .block_on(async move { connection.reply(&call.header(), &properties).await })
      .expect("the held reply");
  }
}

fn emit_from(
  shell: &mut Harness,
  connection: &Connection,
  path: &'static str,
  signal: &'static str,
) {
  let connection = connection.clone();
  shell
    .block_on(async move {
      connection
        .emit_signal(None::<&str>, path, ITEM, signal, &())
        .await
    })
    .expect("a signal on the private bus");
}

/// Runs `attempt` until it gives `Some`, with the loop running between
/// tries. Gives up after 5 s.
fn eventually<T, F>(shell: &mut Harness, what: &str, mut attempt: impl FnMut() -> F) -> T
where
  T: 'static,
  F: Future<Output = Option<T>> + 'static,
{
  let give_up = Instant::now() + Duration::from_secs(5);
  loop {
    if let Some(value) = shell.block_on(attempt()) {
      return value;
    }
    assert!(Instant::now() < give_up, "no {what} after 5 s");
    shell.run_until(Instant::now() + Duration::from_millis(50));
  }
}

/// Runs the loop until `done`. Gives up after 5 s.
fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

/// The watcher's property `name`, read the way another host reads it.
fn watcher_property(shell: &mut Harness, from: &Connection, name: &'static str) -> OwnedVariant {
  let from = from.clone();
  shell
    .block_on(async move {
      let get = (WATCHER, name);
      let call = from.call_method(Some(WATCHER), WATCHER_PATH, Some(PROPERTIES), "Get", &get);
      call.await?.body().deserialize::<OwnedVariant>()
    })
    .expect("the watcher's property")
}

fn registered(shell: &mut Harness, from: &Connection) -> Vec<String> {
  let value = watcher_property(shell, from, "RegisteredStatusNotifierItems");
  value.try_into().expect("a list of strings")
}

/// Unique well-known names, since every test's apps share the process id.
fn item_name() -> &'static str {
  static NEXT: AtomicUsize = AtomicUsize::new(0);
  let name = format!(
    "org.kde.StatusNotifierItem-{}-{}",
    std::process::id(),
    NEXT.fetch_add(1, Ordering::Relaxed)
  );
  name.leak()
}

#[test]
fn the_shell_runs_the_watcher_and_shows_an_item_registered_by_name() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
  let id = format!("{name}{ITEM_PATH}");
  assert_eq!(items.get_untracked(), [sample_item(&id)]);

  // Another host sees it through the watcher, and the watcher describes
  // itself to apps that look first.
  let other = shell.block_on(bus.connect(&[]));
  assert_eq!(registered(&mut shell, &other), [id]);
  let host = watcher_property(&mut shell, &other, "IsStatusNotifierHostRegistered");
  assert_eq!(bool::try_from(host), Ok(true));
  let version = watcher_property(&mut shell, &other, "ProtocolVersion");
  assert_eq!(i32::try_from(version), Ok(0));
  let xml: String = shell
    .block_on(async move {
      let introspect = "org.freedesktop.DBus.Introspectable";
      let call = other.call_method(
        Some(WATCHER),
        WATCHER_PATH,
        Some(introspect),
        "Introspect",
        &(),
      );
      call.await?.body().deserialize::<String>()
    })
    .expect("the watcher's introspection");
  assert!(
    xml.contains(r#"<method name="RegisterStatusNotifierItem">"#),
    "{xml}"
  );
}

#[test]
fn an_item_registered_by_path_is_found_at_its_sender() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let path = "/org/ayatana/NotificationItem/test";
  let app = FakeItem::start(&mut shell, &bus, &[], path, Some(sample()));
  app.register(&mut shell, path);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
  let sender = app.connection.unique_name().expect("a unique name");
  assert_eq!(items.get_untracked()[0].id, format!("{sender}{path}"));
}

#[test]
fn a_change_signal_reads_the_item_again_and_only_from_its_app() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());

  if let Some(properties) = app.properties.borrow_mut().as_mut() {
    properties.title = Some("Renamed".into());
  }
  app.emit(&mut shell, "NewTitle");
  let title = || items.get_untracked()[0].title.clone();
  wait_for(&mut shell, "the new title", || title() == "Renamed");
  let reads = app.calls().len();

  // The same signal from another app, at the same path, reads nothing.
  let stranger = shell.block_on(bus.connect(&[]));
  emit_from(&mut shell, &stranger, ITEM_PATH, "NewIcon");
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert_eq!(app.calls().len(), reads, "{:?}", app.calls());
}

#[test]
fn an_item_that_leaves_the_bus_is_dropped_and_unregistered() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let other = shell.block_on(bus.connect(&[]));
  let heard = listen_to_the_watcher(&mut shell, &other);
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());

  let connection = app.connection.clone();
  shell
    .block_on(connection.close())
    .expect("the app leaves the bus");
  wait_for(&mut shell, "the item to go", || {
    items.get_untracked().is_empty()
  });
  assert_eq!(registered(&mut shell, &other), Vec::<String>::new());
  // Other hosts heard the shell's watcher start, and the item come and go.
  let id = format!("{name}{ITEM_PATH}");
  wait_for(&mut shell, "three signals", || heard.borrow().len() == 3);
  assert_eq!(
    *heard.borrow(),
    [
      "StatusNotifierHostRegistered".to_owned(),
      format!("StatusNotifierItemRegistered {id}"),
      format!("StatusNotifierItemUnregistered {id}"),
    ]
  );
}

/// Every signal the watcher sends, as `listener` hears it: the name, and
/// the item's id if there is one.
fn listen_to_the_watcher(shell: &mut Harness, listener: &Connection) -> Rc<RefCell<Vec<String>>> {
  let heard: Rc<RefCell<Vec<String>>> = Rc::default();
  let rule = zbus::MatchRule::builder()
    .msg_type(Kind::Signal)
    .interface(WATCHER)
    .build()
    .expect("a match rule for the watcher");
  let listener = listener.clone();
  let mut messages = shell
    .block_on(async move { MessageStream::for_match_rule(rule, &listener, None).await })
    .expect("a match on the private bus");
  let sink = Rc::clone(&heard);
  shell.spawn(async move {
    while let Some(Ok(signal)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
      let member = signal.header().member().map(|member| member.to_string());
      let id = signal.body().deserialize::<String>().ok();
      let line = [member, id]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
      sink.borrow_mut().push(line);
    }
  });
  heard
}

#[test]
fn clicks_and_scrolls_reach_the_item() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let tray = shell.cx.tray();
  let items = tray.items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());

  let id = items.get_untracked()[0].id.clone();
  tray.activate(&id);
  tray.secondary_activate(&id);
  tray.scroll(&id, -2);
  let calls = || app.calls()[1..].to_vec();
  wait_for(&mut shell, "three calls", || calls().len() == 3);
  assert_eq!(
    calls(),
    [
      "Activate(0, 0)",
      "SecondaryActivate(0, 0)",
      r#"Scroll(-2, "vertical")"#
    ]
  );

  let logs = capture_logs(|| tray.activate("no-such-item"));
  assert!(logs.contains("no such tray item"), "{logs}");
}

/// A watcher another program runs: it lists `items`, writes down each
/// host that registers, and leaves on drop of its connection.
fn fake_watcher(
  shell: &mut Harness,
  bus: &PrivateBus,
  items: Vec<String>,
) -> (Connection, Rc<RefCell<Vec<String>>>) {
  let connection = shell.block_on(bus.connect(&[WATCHER]));
  let hosts: Rc<RefCell<Vec<String>>> = Rc::default();
  let (answering, registered) = (connection.clone(), Rc::clone(&hosts));
  shell.spawn(async move {
    let mut messages = MessageStream::from(&answering);
    while let Some(Ok(call)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
      let header = call.header();
      if header.message_type() != Kind::MethodCall {
        continue;
      }
      let sent = match header.member().map_or("", |member| member.as_str()) {
        "RegisterStatusNotifierHost" => {
          registered
            .borrow_mut()
            .push(call.body().deserialize().expect("a host name"));
          answering.reply(&header, &()).await
        }
        "Get" => {
          answering
            .reply(&header, &Variant::from(items.clone()))
            .await
        }
        _ => continue,
      };
      sent.expect("the fake watcher's reply");
    }
  });
  (connection, hosts)
}

#[test]
fn another_watcher_is_used_and_taken_over_when_it_leaves() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (first, second) = (item_name(), item_name());
  let _apps = [first, second]
    .map(|name| FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample())));
  let (first, second) = (
    format!("{first}{ITEM_PATH}"),
    format!("{second}{ITEM_PATH}"),
  );
  let (watcher, hosts) = fake_watcher(&mut shell, &bus, vec![first.clone()]);

  let items = shell.cx.tray().items();
  let ids = || -> Vec<String> {
    items
      .get_untracked()
      .into_iter()
      .map(|item| item.id)
      .collect()
  };
  wait_for(&mut shell, "the listed item", || ids() == [first.clone()]);
  let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
  assert_eq!(*hosts.borrow(), std::slice::from_ref(&host));

  // Another app can address the watcher's signals to the shell, but they
  // don't count.
  let stranger = shell.block_on(bus.connect(&[]));
  let spoofed = first.clone();
  let logs = capture_logs(|| {
    shell
      .block_on(async move {
        let ask = stranger.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &host);
        let shell: String = ask.await?.body().deserialize()?;
        let unregistered = "StatusNotifierItemUnregistered";
        let emit = stranger.emit_signal(
          Some(shell.as_str()),
          WATCHER_PATH,
          WATCHER,
          unregistered,
          &spoofed,
        );
        emit.await
      })
      .expect("a signal addressed to the shell");
    shell.run_until(Instant::now() + Duration::from_millis(200));
  });
  assert_eq!(ids(), std::slice::from_ref(&first), "{logs}");

  let announce = |shell: &mut Harness, signal: &'static str, id: &str| {
    let (watcher, id) = (watcher.clone(), id.to_owned());
    shell
      .block_on(async move {
        let emit = watcher.emit_signal(None::<&str>, WATCHER_PATH, WATCHER, signal, &id);
        emit.await
      })
      .expect("a signal from the fake watcher");
  };
  announce(&mut shell, "StatusNotifierItemRegistered", &second);
  wait_for(&mut shell, "the second item", || ids().len() == 2);
  announce(&mut shell, "StatusNotifierItemUnregistered", &first);
  wait_for(&mut shell, "the first item to go", || {
    ids() == [second.clone()]
  });

  // The watcher leaves, and the shell takes its name and keeps the items.
  shell
    .block_on(watcher.clone().close())
    .expect("the fake watcher leaves");
  let other = shell.block_on(bus.connect(&[]));
  let listed = eventually(&mut shell, "the shell's watcher", || {
    let other = other.clone();
    async move {
      let get = (WATCHER, "RegisteredStatusNotifierItems");
      let call = other.call_method(Some(WATCHER), WATCHER_PATH, Some(PROPERTIES), "Get", &get);
      let value = call.await.ok()?.body().deserialize::<OwnedVariant>().ok()?;
      Vec::<String>::try_from(value).ok()
    }
  });
  assert_eq!(listed, std::slice::from_ref(&second));
  assert_eq!(ids(), [second]);
}

#[test]
fn bad_registrations_and_unknown_calls_get_errors() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  shell.cx.tray();
  let app = shell.block_on(bus.connect(&[]));
  let call = |member: &'static str, arg: &'static str| {
    let app = app.clone();
    async move {
      let call = app.call_method(Some(WATCHER), WATCHER_PATH, Some(WATCHER), member, &arg);
      call.await.err().map(|error| error.to_string())
    }
  };
  let refused = eventually(&mut shell, "a refusal from the watcher", || {
    let refusal = call("RegisterStatusNotifierItem", "not a name!");
    async move {
      refusal
        .await
        .filter(|error| !error.contains("ServiceUnknown"))
    }
  });
  assert!(refused.contains("InvalidArgs"), "{refused}");
  let unknown = shell.block_on(call("Frobnicate", "")).expect("an error");
  assert!(unknown.contains("UnknownMethod"), "{unknown}");
}

#[test]
fn an_item_with_wrongly_typed_properties_is_logged_and_not_shown() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, None);
  let logs = capture_logs(|| {
    app.register(&mut shell, name);
    wait_for(&mut shell, "a read", || !app.calls().is_empty());
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  assert!(logs.contains("can't read a tray item"), "{logs}");
  assert_eq!(items.get_untracked(), []);

  // It shows once it sends good properties and says so.
  *app.properties.borrow_mut() = Some(sample());
  app.emit(&mut shell, "NewIcon");
  wait_for(&mut shell, "the item", || items.get_untracked().len() == 1);
}

#[test]
fn bad_pixmaps_are_skipped() {
  let raw = vec![
    (1, 1, vec![255, 0, 0, 0]),
    (2, 2, vec![0; 15]),
    (513, 1, vec![0; 513 * 4]),
    (0, 0, Vec::new()),
    (-1, -1, vec![0; 4]),
  ];
  let kept = super::pixmaps("test", raw);
  assert_eq!(kept.len(), 1);
  assert_eq!((kept[0].width, kept[0].height), (1, 1));
}

#[test]
fn registrations_parse_as_the_spec_and_watchers_write_them() {
  use super::parse_service;
  let pair = |bus: &str, path: &str| Some((bus.to_owned(), path.to_owned()));
  assert_eq!(
    parse_service("org.kde.StatusNotifierItem-1-1", None),
    pair("org.kde.StatusNotifierItem-1-1", ITEM_PATH)
  );
  assert_eq!(
    parse_service("/org/ayatana/NotificationItem/nm", Some(":1.7")),
    pair(":1.7", "/org/ayatana/NotificationItem/nm")
  );
  assert_eq!(
    parse_service(":1.7/org/ayatana/NotificationItem/nm", None),
    pair(":1.7", "/org/ayatana/NotificationItem/nm")
  );
  assert_eq!(parse_service("/StatusNotifierItem", None), None);
  assert_eq!(parse_service("not a name", None), None);
  assert_eq!(parse_service(":1.7/bad path", None), None);
}

#[test]
fn the_icon_is_the_attention_name_the_name_or_the_closest_pixmap() {
  let pixels = |side: u32| IconPixels {
    width: side,
    height: side,
    data: vec![0; (side * side * 4) as usize].into(),
  };
  let mut item = sample_item("x");
  item.icon_theme_path.clear();
  let name = |name: &str| Some(IconSource::Name(name.into()));
  // A name comes before pixmaps.
  assert_eq!(item.icon(16), name("dialog-warning"));
  item.status = TrayStatus::Active;
  assert_eq!(item.icon(16), name("network-wireless"));

  // The app's own directory comes first. Where that lacks the name, the
  // name is the app's own, so its pixmaps come next.
  let dir = std::env::temp_dir().join(format!("crabshell-tray-{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("a temporary directory");
  let file = dir.join("network-wireless.png");
  std::fs::write(&file, b"").expect("an icon file");
  item.icon_theme_path = dir.display().to_string();
  assert_eq!(item.icon(16), Some(IconSource::File(file)));
  item.icon_name = "../network-wireless".into();
  let sent = item.icon_pixmaps[0].clone();
  assert_eq!(item.icon(16), Some(IconSource::Pixels(sent)));
  item.icon_pixmaps.clear();
  assert_eq!(item.icon(16), name("../network-wireless"));
  std::fs::remove_dir_all(&dir).expect("the directory removed");

  item.icon_name.clear();
  item.icon_pixmaps = vec![pixels(16), pixels(22), pixels(48)];
  assert_eq!(item.icon(24), Some(IconSource::Pixels(pixels(22))));
  assert_eq!(item.icon(40), Some(IconSource::Pixels(pixels(48))));
  item.icon_pixmaps.clear();
  assert_eq!(item.icon(24), None);
}

#[test]
fn pixmaps_past_the_budget_are_dropped_largest_first() {
  let square = |side: i32| (side, side, vec![0; (side * side * 4) as usize]);
  let sides = |kept: Vec<IconPixels>| kept.iter().map(|pixels| pixels.width).collect::<Vec<_>>();
  let kept = super::pixmaps("test", vec![square(512), square(16), square(256)]);
  assert_eq!(sides(kept), [16, 256]);
  let kept = super::pixmaps("test", vec![square(512)]);
  assert_eq!(sides(kept), [512]);
  let many = super::pixmaps("test", (0..64).map(|_| square(512)).collect());
  assert_eq!(many.len(), 1);
}

#[test]
fn a_slow_read_never_overwrites_a_newer_one() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());

  *app.held.borrow_mut() = Some(Vec::new());
  app.emit(&mut shell, "NewTitle");
  app.emit(&mut shell, "NewTitle");
  let held = || app.held.borrow().as_ref().map_or(0, Vec::len);
  wait_for(&mut shell, "two reads", || held() == 2);
  app.answer_held(&mut shell, 1, "Newer");
  let title = || items.get_untracked()[0].title.clone();
  wait_for(&mut shell, "the newer title", || title() == "Newer");
  app.answer_held(&mut shell, 0, "Older");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(title(), "Newer");
}

#[test]
fn an_item_name_taken_by_another_app_is_read_from_it() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let first = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  first.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());

  // A restarted app takes the name over before it registers again.
  let renamed = Properties {
    title: Some("Second".into()),
    ..sample()
  };
  let second = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(renamed));
  let title = || items.get_untracked()[0].title.clone();
  wait_for(&mut shell, "the second app's title", || title() == "Second");
  // Its signals count now, and the first app's don't.
  if let Some(properties) = second.properties.borrow_mut().as_mut() {
    properties.title = Some("Third".into());
  }
  let reads = first.calls().len();
  first.emit(&mut shell, "NewTitle");
  second.emit(&mut shell, "NewTitle");
  wait_for(&mut shell, "the third title", || title() == "Third");
  assert_eq!(first.calls().len(), reads);
}

#[test]
fn a_registration_that_wants_no_reply_still_counts() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  let connection = app.connection.clone();
  eventually(&mut shell, "the shell's watcher", move || {
    let connection = connection.clone();
    async move {
      let ask = connection.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &WATCHER);
      ask.await.ok()
    }
  });
  let connection = app.connection.clone();
  shell
    .block_on(async move {
      let call = zbus::Message::method_call(WATCHER_PATH, "RegisterStatusNotifierItem")
        .destination(WATCHER)
        .interface(WATCHER)
        .with_flags(zbus::message::Flags::NO_REPLY_EXPECTED)
        .build(&name)?;
      connection.send(&call).await
    })
    .expect("a call that wants no reply");
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
}

#[test]
fn a_frozen_watcher_leaves_the_shell_answering() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  // It owns the name and never reads a message.
  let _frozen = shell.block_on(bus.connect(&[WATCHER]));
  shell.cx.tray();
  let other = shell.block_on(bus.connect(&[]));
  let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
  let xml = eventually(&mut shell, "an answer from the shell", move || {
    let (other, host) = (other.clone(), host.clone());
    async move {
      let ask = other.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &host);
      let shell: String = ask.await.ok()?.body().deserialize().ok()?;
      let introspect = "org.freedesktop.DBus.Introspectable";
      let call = other.call_method(
        Some(shell.as_str()),
        WATCHER_PATH,
        Some(introspect),
        "Introspect",
        &(),
      );
      call.await.ok()?.body().deserialize::<String>().ok()
    }
  });
  assert!(xml.contains("StatusNotifierWatcher"));
}

#[test]
fn an_idle_tray_wakes_nothing() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
  shell.run_until(Instant::now() + Duration::from_millis(200));

  let wakeups = shell.run_until(Instant::now() + Duration::from_secs(2));
  // The one that ends the run.
  assert_eq!(wakeups, 1);
}

#[test]
fn without_a_session_bus_the_tray_is_empty_and_quiet() {
  let mut shell = Harness::new(offline());
  let items = shell.cx.tray().items();
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_millis(200)),
    1
  );
  assert_eq!(items.get_untracked(), []);

  let mut shell = Harness::new(Config {
    session_bus: Some(
      "unix:path=/nonexistent/crabshell-test-bus"
        .parse()
        .expect("an address"),
    ),
    ..offline()
  });
  let logs = capture_logs(|| {
    let items = shell.cx.tray().items();
    shell.run_until(Instant::now() + Duration::from_millis(100));
    assert_eq!(items.get_untracked(), []);
  });
  assert!(logs.contains("can't reach the session bus"), "{logs}");
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_millis(500)),
    1
  );
}

#[test]
fn every_caller_shares_one_tray() {
  let shell = Harness::new(offline());
  let items = shell.cx.tray().items();
  let output = shell.cx.child();
  assert_eq!(output.tray().items(), items);
  output.owner().dispose();
  assert!(items.try_get().is_some());
}

#[test]
fn without_provide_the_tray_is_empty_under_its_context() {
  let cx = Cx::new(Runtime::current().new_owner());
  let items = cx.tray().items();
  assert_eq!(items.get_untracked(), []);
  cx.owner().dispose();
  assert_eq!(items.try_get(), None);
}

#[test]
fn disposing_the_root_frees_the_tray() {
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(sample()));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
  shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  // A message after that finds the list gone, and nothing panics.
  app.emit(&mut shell, "NewTitle");
  shell.run_until(Instant::now() + Duration::from_millis(100));
}
