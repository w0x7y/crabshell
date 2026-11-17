use std::cell::RefCell;
use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crabshell_reactive::Runtime;
use crabshell_ui::Color;
use crabshell_ui::{
  Content, Cx, IconPixels, IconSource, NodeView, Output, PanelWindow, Point, PointerButton, Rect,
  ScrollSource, Size, Style, TextSystem, Wheel, WheelAxis, Window,
};
use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{Connection, MessageStream, OwnedVariant, Variant};

use super::menu::{Child, DBUSMENU, Layout, Props, entries, without_access_keys};
use super::{
  BUS, BUS_PATH, ITEM, ITEM_PATH, MenuEntry, MenuToggle, PROPERTIES, Properties, Tray,
  TrayCategory, TrayItem, TrayMenu, TrayStatus, WATCHER, WATCHER_PATH,
};
use crate::testing::{Harness, PrivateBus, capture_logs, capture_logs_from, offline};
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
    menu: Some(MENU_PATH.try_into().expect("an object path")),
    category: Some("Hardware".into()),
  }
}

const MENU_PATH: &str = "/MenuBar";

/// The test app's menu: an entry with an access key, a separator, a
/// checked checkmark, a hidden entry, a disabled submenu, and a submenu
/// the app fills only when it opens.
fn sample_menu() -> Layout {
  let node =
    |id, props, children: Vec<Layout>| Layout(id, props, children.into_iter().map(Child).collect());
  let label = |label: &str| Props {
    label: Some(label.into()),
    ..Props::default()
  };
  let separator = Props {
    kind: Some("separator".into()),
    ..Props::default()
  };
  let checked = Props {
    toggle_type: Some("checkmark".into()),
    toggle_state: Some(1),
    ..label("Wi-Fi")
  };
  let hidden = Props {
    visible: Some(false),
    ..label("Hidden")
  };
  let disabled = Props {
    enabled: Some(false),
    ..label("More")
  };
  let later = Props {
    children_display: Some("submenu".into()),
    ..label("Later")
  };
  let radio = Props {
    toggle_type: Some("radio".into()),
    toggle_state: Some(0),
    ..label("A__B")
  };
  node(
    0,
    Props::default(),
    vec![
      node(1, label("_Connect"), vec![]),
      node(2, separator, vec![]),
      node(3, checked, vec![]),
      node(4, hidden, vec![]),
      node(5, disabled, vec![node(6, radio, vec![])]),
      node(7, later, vec![]),
    ],
  )
}

/// [`sample_menu`] with its first entry called `label`.
fn renamed_menu(label: &str) -> Layout {
  let mut layout = sample_menu();
  layout.2[0].0.1.label = Some(label.into());
  layout
}

/// [`sample_menu`] as the tray shows it.
fn sample_entries() -> Vec<MenuEntry> {
  let entry = |id, label: &str| MenuEntry {
    id,
    label: label.into(),
    separator: false,
    enabled: true,
    toggle: None,
    checked: false,
    submenu: false,
    children: Vec::new(),
  };
  vec![
    entry(1, "Connect"),
    MenuEntry {
      separator: true,
      ..entry(2, "")
    },
    MenuEntry {
      toggle: Some(MenuToggle::Checkmark),
      checked: true,
      ..entry(3, "Wi-Fi")
    },
    MenuEntry {
      enabled: false,
      submenu: true,
      children: vec![MenuEntry {
        toggle: Some(MenuToggle::Radio),
        ..entry(6, "A_B")
      }],
      ..entry(5, "More")
    },
    MenuEntry {
      submenu: true,
      ..entry(7, "Later")
    },
  ]
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
    menu: MENU_PATH.into(),
    category: TrayCategory::Hardware,
  }
}

/// An app's tray item on the private bus. It answers `GetAll` with
/// `properties`, or with a wrongly typed title while that is `None`, and
/// writes down every call it gets. Its menu at [`MENU_PATH`] answers
/// `GetLayout` with `layout`, or with a wrongly typed label while that is
/// `None`.
struct FakeItem {
  connection: Connection,
  path: &'static str,
  properties: Rc<RefCell<Option<Properties>>>,
  calls: Rc<RefCell<Vec<String>>>,
  /// While `Some`, each `GetAll` and `GetLayout` waits here for the test
  /// to answer it.
  held: Rc<RefCell<Option<Vec<zbus::Message>>>>,
  layout: Rc<RefCell<Option<Layout>>>,
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
      layout: Rc::new(RefCell::new(Some(sample_menu()))),
    };
    let (properties, calls) = (Rc::clone(&item.properties), Rc::clone(&item.calls));
    let (held, layout) = (Rc::clone(&item.held), Rc::clone(&item.layout));
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
        let at = header.path().map(|path| path.as_str());
        if header.message_type() != Kind::MethodCall {
          continue;
        }
        // The item's methods at its path, the menu's at the menu's.
        let member = match header.member().map_or("", |member| member.as_str()) {
          "GetAll" | "Activate" | "SecondaryActivate" | "Scroll" if at == Some(path) => {
            member_of(&header)
          }
          "AboutToShow" | "GetLayout" | "Event" if at == Some(MENU_PATH) => member_of(&header),
          _ => continue,
        };
        let member = member.as_str();
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
          "AboutToShow" => {
            let id: i32 = call.body().deserialize().expect("i");
            calls.borrow_mut().push(format!("{member}({id})"));
            // A submenu changes as it opens; the root doesn't.
            connection.reply(&header, &(id != 0)).await
          }
          "GetLayout" => {
            let args: (i32, i32, Vec<String>) = call.body().deserialize().expect("(iias)");
            calls.borrow_mut().push(format!("{member}{args:?}"));
            if let Some(held) = held.borrow_mut().as_mut() {
              held.push(call.clone());
              continue;
            }
            let layout = layout.borrow().clone();
            match layout {
              Some(layout) => connection.reply(&header, &(1u32, layout)).await,
              None => {
                let wrong = HashMap::from([("label", Variant::from(7u32))]);
                let root = (0, wrong, Vec::<Variant<'_>>::new());
                connection.reply(&header, &(1u32, root)).await
              }
            }
          }
          "Event" => {
            let (id, event, _, _): (i32, String, OwnedVariant, u32) =
              call.body().deserialize().expect("(isvu)");
            calls
              .borrow_mut()
              .push(format!("{member}({id}, {event:?})"));
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

  /// Says its menu changed, from `connection`, as `LayoutUpdated` does:
  /// to anyone listening, or only to `to`.
  fn menu_changed(shell: &mut Harness, connection: &Connection, to: Option<String>) {
    let connection = connection.clone();
    shell
      .block_on(async move {
        let signal = "LayoutUpdated";
        let emit = connection.emit_signal(to.as_deref(), MENU_PATH, DBUSMENU, signal, &(2u32, 0));
        emit.await
      })
      .expect("a signal on the private bus");
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
fn a_category_reads_as_the_spec_names_it_and_anything_else_as_application_status() {
  let category = |sent: Option<&str>| {
    let properties = Properties {
      category: sent.map(str::to_owned),
      ..sample()
    };
    properties.into_item("x".into()).category
  };
  assert_eq!(
    category(Some("ApplicationStatus")),
    TrayCategory::ApplicationStatus
  );
  assert_eq!(
    category(Some("Communications")),
    TrayCategory::Communications
  );
  assert_eq!(
    category(Some("SystemServices")),
    TrayCategory::SystemServices
  );
  assert_eq!(category(Some("Hardware")), TrayCategory::Hardware);
  assert_eq!(category(None), TrayCategory::ApplicationStatus);
  let logs = capture_logs_from(tracing::Level::DEBUG, || {
    assert_eq!(category(Some("hardware")), TrayCategory::ApplicationStatus);
  });
  assert!(logs.contains("a tray item's category is unknown"), "{logs}");
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

  let tray = shell.cx.tray();
  tray.open_menu(&items.get_untracked()[0].id);
  let menu = tray.menu();
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());

  // A restarted app takes the name over before it registers again.
  let renamed = Properties {
    title: Some("Second".into()),
    ..sample()
  };
  let second = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(renamed));
  let title = || items.get_untracked()[0].title.clone();
  wait_for(&mut shell, "the second app's title", || title() == "Second");
  // The first app's menu closed with it.
  assert_eq!(menu.get_untracked(), None);
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

fn member_of(header: &zbus::message::Header<'_>) -> String {
  header
    .member()
    .map(|member| member.to_string())
    .unwrap_or_default()
}

/// A shell with one registered, read test app: the app and its item's id.
fn one_item(shell: &mut Harness, bus: &PrivateBus) -> (FakeItem, String) {
  let items = shell.cx.tray().items();
  let name = item_name();
  let app = FakeItem::start(shell, bus, &[name], ITEM_PATH, Some(sample()));
  app.register(shell, name);
  let id = format!("{name}{ITEM_PATH}");
  // This item, not any: another may be shown already.
  wait_for(shell, "the item", || {
    items.get_untracked().iter().any(|item| item.id == id)
  });
  (app, id)
}

#[test]
fn a_menu_opens_after_about_to_show_and_reads_the_layout() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  assert_eq!(menu.get_untracked(), None);

  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  let expected = TrayMenu {
    item: id,
    entries: sample_entries(),
  };
  assert_eq!(menu.get_untracked(), Some(expected));
  let names =
    r#"["type", "label", "enabled", "visible", "toggle-type", "toggle-state", "children-display"]"#;
  assert_eq!(
    app.calls()[1..],
    [
      "AboutToShow(0)".to_owned(),
      format!("GetLayout(0, -1, {names})")
    ]
  );

  // An open menu wakes nothing while nothing changes.
  shell.run_until(Instant::now() + Duration::from_millis(100));
  let wakeups = shell.run_until(Instant::now() + Duration::from_secs(1));
  assert_eq!(wakeups, 1);
}

#[test]
fn an_open_menu_follows_its_app_until_it_closes() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());

  let renamed = renamed_menu;
  let first = || {
    menu
      .get_untracked()
      .map(|menu| menu.entries[0].label.clone())
  };
  *app.layout.borrow_mut() = Some(renamed("Disconnect"));
  FakeItem::menu_changed(&mut shell, &app.connection, None);
  wait_for(&mut shell, "the new label", || {
    first().as_deref() == Some("Disconnect")
  });

  // Another app saying so reads nothing, even addressed to the shell, and
  // neither does the app once the menu is closed.
  let reads = || {
    app
      .calls()
      .iter()
      .filter(|call| call.starts_with("GetLayout"))
      .count()
  };
  let before = reads();
  let stranger = shell.block_on(bus.connect(&[]));
  let asking = stranger.clone();
  let host = format!("org.kde.StatusNotifierHost-{}", std::process::id());
  let shell_name: String = shell
    .block_on(async move {
      let ask = asking.call_method(Some(BUS), BUS_PATH, Some(BUS), "GetNameOwner", &host);
      ask.await?.body().deserialize::<String>()
    })
    .expect("the shell's unique name");
  FakeItem::menu_changed(&mut shell, &stranger, None);
  FakeItem::menu_changed(&mut shell, &stranger, Some(shell_name));
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(reads(), before);
  tray.close_menu();
  assert_eq!(menu.get_untracked(), None);
  FakeItem::menu_changed(&mut shell, &app.connection, None);
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(reads(), before);
  assert_eq!(menu.get_untracked(), None);
}

#[test]
fn clicking_a_menu_entry_sends_an_event() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  shell.cx.tray().menu_event(&id, 3);
  let events = || {
    app
      .calls()
      .into_iter()
      .filter(|call| call.starts_with("Event"))
      .collect::<Vec<_>>()
  };
  wait_for(&mut shell, "the event", || !events().is_empty());
  assert_eq!(events(), [r#"Event(3, "clicked")"#]);
}

#[test]
fn a_bad_menu_is_logged_and_stays_closed() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  *app.layout.borrow_mut() = None;
  let logs = capture_logs(|| {
    tray.open_menu(&id);
    let read = || app.calls().iter().any(|call| call.starts_with("GetLayout"));
    wait_for(&mut shell, "a read", read);
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  assert!(logs.contains("can't read a tray item's menu"), "{logs}");
  assert_eq!(tray.menu().get_untracked(), None);

  let logs = capture_logs(|| tray.open_menu("no-such-item"));
  assert!(logs.contains("this tray item has no menu"), "{logs}");
}

#[test]
fn an_item_that_leaves_closes_its_menu() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  let connection = app.connection.clone();
  shell
    .block_on(connection.close())
    .expect("the app leaves the bus");
  wait_for(&mut shell, "the menu to close", || {
    menu.get_untracked().is_none()
  });
}

#[test]
fn labels_lose_their_access_key_underscores() {
  assert_eq!(without_access_keys("_File"), "File");
  assert_eq!(without_access_keys("Save _As"), "Save As");
  assert_eq!(without_access_keys("snake__case"), "snake_case");
  assert_eq!(without_access_keys("trailing_"), "trailing");
  assert_eq!(without_access_keys(""), "");
}

#[test]
fn a_no_menu_path_means_no_menu() {
  let none = Properties {
    menu: Some("/NO_DBUSMENU".try_into().expect("an object path")),
    ..sample()
  };
  assert_eq!(none.into_item("x".into()).menu, "");
  assert_eq!(sample().into_item("x".into()).menu, MENU_PATH);
}

#[test]
fn a_menu_closed_while_its_layout_is_read_stays_closed() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  *app.held.borrow_mut() = Some(Vec::new());
  tray.open_menu(&id);
  let held = || app.held.borrow().as_ref().map_or(0, Vec::len);
  wait_for(&mut shell, "the layout read", || held() == 1);
  tray.close_menu();
  let call = app.held.borrow().as_ref().expect("held calls")[0].clone();
  let connection = app.connection.clone();
  shell
    .block_on(async move {
      connection
        .reply(&call.header(), &(1u32, sample_menu()))
        .await
    })
    .expect("the late layout");
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(tray.menu().get_untracked(), None);
}

/// Every `AddMatch` and `RemoveMatch` any connection sends the bus, as a
/// monitor sees them: the method, a space and the rule.
fn match_calls(shell: &mut Harness, bus: &PrivateBus) -> Rc<RefCell<Vec<String>>> {
  let monitor = shell.block_on(bus.connect(&[]));
  let mut messages = MessageStream::from(&monitor);
  let becoming = monitor.clone();
  shell
    .block_on(async move {
      let monitoring = "org.freedesktop.DBus.Monitoring";
      let rules = (Vec::<&str>::new(), 0u32);
      let call = becoming.call_method(
        Some(BUS),
        BUS_PATH,
        Some(monitoring),
        "BecomeMonitor",
        &rules,
      );
      call.await
    })
    .expect("a monitor on the private bus");
  let seen: Rc<RefCell<Vec<String>>> = Rc::default();
  let sink = Rc::clone(&seen);
  shell.spawn(async move {
    let _monitor = monitor;
    while let Some(Ok(message)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
      let header = message.header();
      let member = member_of(&header);
      if header.message_type() == Kind::MethodCall
        && matches!(member.as_str(), "AddMatch" | "RemoveMatch")
        && let Ok(rule) = message.body().deserialize::<String>()
      {
        sink.borrow_mut().push(format!("{member} {rule}"));
      }
    }
  });
  seen
}

#[test]
fn a_menu_follows_its_app_only_while_open() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let matches = match_calls(&mut shell, &bus);
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  let sender = app
    .connection
    .unique_name()
    .expect("a unique name")
    .to_string();
  let rule = format!("type='signal',sender='{sender}',path='{MENU_PATH}',interface='{DBUSMENU}'");
  let count = |call: &str| {
    let line = format!("{call} {rule}");
    matches
      .borrow()
      .iter()
      .filter(|seen| **seen == line)
      .count()
  };

  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  tray.close_menu();
  wait_for(&mut shell, "the match removed", || {
    count("RemoveMatch") == 1
  });
  assert_eq!(count("AddMatch"), 1);

  // A menu that can't be read closes, and its match goes too.
  *app.layout.borrow_mut() = None;
  tray.open_menu(&id);
  wait_for(&mut shell, "the second match removed", || {
    count("RemoveMatch") == 2
  });
  assert_eq!(count("AddMatch"), 2);
  assert_eq!(menu.get_untracked(), None);
}

#[test]
fn a_slow_menu_read_never_overwrites_a_newer_one() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  *app.held.borrow_mut() = Some(Vec::new());
  tray.open_menu(&id);
  let held = || app.held.borrow().as_ref().map_or(0, Vec::len);
  wait_for(&mut shell, "the first read", || held() == 1);
  FakeItem::menu_changed(&mut shell, &app.connection, None);
  wait_for(&mut shell, "the second read", || held() == 2);

  let answer = |shell: &mut Harness, index: usize, layout: Layout| {
    let call = app.held.borrow().as_ref().expect("held calls")[index].clone();
    let connection = app.connection.clone();
    shell
      .block_on(async move { connection.reply(&call.header(), &(1u32, layout)).await })
      .expect("a held layout");
  };
  answer(&mut shell, 1, renamed_menu("Newer"));
  let first = || {
    menu
      .get_untracked()
      .map(|menu| menu.entries[0].label.clone())
  };
  wait_for(&mut shell, "the newer menu", || {
    first().as_deref() == Some("Newer")
  });
  answer(&mut shell, 0, sample_menu());
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(first().as_deref(), Some("Newer"));
}

#[test]
fn an_unchanged_menu_is_not_set_again() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  let runs = Rc::new(std::cell::Cell::new(0));
  let counting = Rc::clone(&runs);
  shell.cx.effect(move || {
    menu.with(|_| ());
    counting.set(counting.get() + 1);
  });
  Runtime::current().flush();
  let before = runs.get();

  FakeItem::menu_changed(&mut shell, &app.connection, None);
  let reads = || {
    app
      .calls()
      .iter()
      .filter(|call| call.starts_with("GetLayout"))
      .count()
  };
  wait_for(&mut shell, "the second read", || reads() == 2);
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(runs.get(), before);
}

#[test]
fn a_menu_opened_over_another_is_read_once_after_it_is_told() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (first, first_id) = one_item(&mut shell, &bus);
  let (second, second_id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  tray.open_menu(&first_id);
  tray.open_menu(&second_id);
  let shown = || menu.get_untracked().map(|menu| menu.item);
  wait_for(&mut shell, "the second menu", || {
    shown() == Some(second_id.clone())
  });
  shell.run_until(Instant::now() + Duration::from_millis(200));
  // The first opening's task, left over, reads neither menu.
  let menu_calls = |app: &FakeItem| {
    let calls = app.calls().into_iter();
    calls
      .filter(|call| !call.starts_with("GetAll"))
      .collect::<Vec<_>>()
  };
  assert!(
    !menu_calls(&first)
      .iter()
      .any(|call| call.starts_with("GetLayout"))
  );
  let second_calls = menu_calls(&second);
  assert_eq!(second_calls.len(), 2, "{second_calls:?}");
  assert_eq!(second_calls[0], "AboutToShow(0)");
}

#[test]
fn opening_a_submenu_asks_the_app_and_reads_what_changed() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let (app, id) = one_item(&mut shell, &bus);
  let tray = shell.cx.tray();
  let menu = tray.menu();
  tray.open_menu(&id);
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  tray.open_submenu(7);
  let reads = || {
    app
      .calls()
      .iter()
      .filter(|call| call.starts_with("GetLayout"))
      .count()
  };
  wait_for(&mut shell, "a second read", || reads() == 2);
  assert!(app.calls().contains(&"AboutToShow(7)".to_owned()));

  tray.close_menu();
  let logs = capture_logs(|| tray.open_submenu(5));
  assert!(logs.contains("no tray menu is open"), "{logs}");
}

#[test]
fn a_huge_menu_keeps_its_first_entries() {
  let leaf = |id| Child(Layout(id, Props::default(), Vec::new()));
  let mut left = 1024;
  assert_eq!(
    entries((0..1100).map(leaf).collect(), &mut left).len(),
    1024
  );
  let mut left = 1024;
  let nested = vec![Child(Layout(
    0,
    Props::default(),
    (1..1100).map(leaf).collect(),
  ))];
  assert_eq!(entries(nested, &mut left)[0].children.len(), 1023);
}

/// A 400 by 30 bar holding a tray of 16 px icons, laid out.
fn tray_bar(shell: &mut Harness, text: &mut TextSystem) -> Window {
  let panel = PanelWindow::new(&Output::new("DP-1")).child(Tray::new().icon_size(16));
  let window = Window::new(&shell.cx, panel);
  window.set_size(Size::new(400.0, 30.0));
  frame(&window, text);
  window
}

/// Runs the bindings and lays `window` out if it needs it.
fn frame(window: &Window, text: &mut TextSystem) {
  Runtime::current().flush();
  if window.needs_layout() {
    window.layout(text);
  }
}

/// The tray's item buttons: the bar holds the tray's row, which holds the
/// row of items.
fn tray_buttons(window: &Window) -> Vec<(Rect, Option<IconSource>)> {
  let view = window.view();
  let tray = view.root().children().next().expect("the tray");
  let items = tray.children().next().expect("the tray's row of items");
  items
    .children()
    .map(|button| {
      let icon = button
        .children()
        .next()
        .and_then(|icon| match icon.content() {
          Content::Icon(icon) => Some(icon.source.clone()),
          _ => None,
        });
      (button.rect(), icon)
    })
    .collect()
}

/// The rectangle of the button whose text is `label`, anywhere in `window`.
fn labeled(window: &Window, label: &str) -> Option<Rect> {
  fn find(node: NodeView<'_>, label: &str, button: Option<Rect>) -> Option<Rect> {
    let button = if node.name() == "Button" {
      Some(node.rect())
    } else {
      button
    };
    if let Content::Text(text) = node.content()
      && text.content() == label
    {
      return button;
    }
    node.children().find_map(|child| find(child, label, button))
  }
  find(window.view().root(), label, None)
}

fn press(window: &Window, at: Rect, button: PointerButton) {
  window.pointer_motion(Point::new(at.x + at.width / 2.0, at.y + at.height / 2.0));
  window.pointer_button(button, true);
  window.pointer_button(button, false);
}

#[test]
fn the_tray_shows_each_item_and_passes_its_clicks_on() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  assert!(tray_buttons(&window).is_empty());

  // An app that isn't only a menu, so a left click activates it.
  let items = shell.cx.tray().items();
  let name = item_name();
  let properties = Properties {
    item_is_menu: Some(false),
    ..sample()
  };
  let app = FakeItem::start(&mut shell, &bus, &[name], ITEM_PATH, Some(properties));
  app.register(&mut shell, name);
  wait_for(&mut shell, "the item", || !items.get_untracked().is_empty());
  frame(&window, &mut text);
  let buttons = tray_buttons(&window);
  assert_eq!(buttons.len(), 1);
  let item = shell.cx.tray().items().get_untracked().remove(0);
  assert_eq!(buttons[0].1, item.icon(32), "the item's icon");

  let at = buttons[0].0;
  press(&window, at, PointerButton::Left);
  press(&window, at, PointerButton::Middle);
  window.pointer_scroll(Wheel {
    source: ScrollSource::Wheel,
    x: WheelAxis::default(),
    y: WheelAxis {
      pixels: -15.0,
      notches: -1.0,
      stop: false,
    },
  });
  let calls = || app.calls()[1..].to_vec();
  wait_for(&mut shell, "three calls", || calls().len() == 3);
  assert_eq!(
    calls(),
    [
      "Activate(0, 0)",
      "SecondaryActivate(0, 0)",
      r#"Scroll(1, "vertical")"#
    ]
  );
  assert!(
    window.take_popups().is_empty(),
    "the item isn't only a menu"
  );

  // The item leaves, and its button goes.
  let connection = app.connection.clone();
  shell
    .block_on(connection.close())
    .expect("the app leaves the bus");
  wait_for(&mut shell, "the item to go", || {
    items.get_untracked().is_empty()
  });
  frame(&window, &mut text);
  assert!(tray_buttons(&window).is_empty());
}

#[test]
fn a_click_opens_the_menu_and_an_entry_tells_the_app() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  let (app, _) = one_item(&mut shell, &bus);
  frame(&window, &mut text);
  let menu = shell.cx.tray().menu();
  Runtime::current().flush();
  let nodes = Runtime::current().node_count();

  // The sample item is only a menu, so a left click opens it, as a right
  // click does.
  press(&window, tray_buttons(&window)[0].0, PointerButton::Left);
  let popup = window.take_popups().pop().expect("the popup opens at once");
  frame(&popup, &mut text);
  assert_eq!(
    popup.size(),
    Size::new(1.0, 1.0),
    "empty until the layout comes"
  );
  let background = |popup: &Window| {
    let view = popup.view();
    let menu = view.root().children().next().expect("the menu");
    menu.style().background
  };
  assert_eq!(background(&popup), None, "and clear");

  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  frame(&popup, &mut text);
  assert!(popup.size().height > 20.0, "{:?}", popup.size());
  assert!(background(&popup).is_some(), "styled once it shows");
  let connect = labeled(&popup, "Connect").expect("the Connect entry");
  press(&popup, connect, PointerButton::Left);
  let events = || {
    let calls = app.calls().into_iter();
    calls
      .filter(|call| call.starts_with("Event"))
      .collect::<Vec<_>>()
  };
  wait_for(&mut shell, "the event", || !events().is_empty());
  assert_eq!(events(), [r#"Event(1, "clicked")"#]);
  assert!(popup.wants_close());

  // The platform drops the closed popup, and the tray stops following the
  // app's menu.
  drop(popup);
  Runtime::current().flush();
  assert_eq!(
    Runtime::current().node_count(),
    nodes,
    "the popup's nodes went"
  );
  assert_eq!(menu.get_untracked(), None);
  let service = shell.cx.tray().service.expect("the shell's tray");
  assert!(!service.is_open(|_| true), "no menu is followed");
}

#[test]
fn a_menu_opened_again_outlives_the_popup_it_replaced() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  one_item(&mut shell, &bus);
  frame(&window, &mut text);
  let menu = shell.cx.tray().menu();
  let at = tray_buttons(&window)[0].0;

  press(&window, at, PointerButton::Right);
  let first = window.take_popups().pop().expect("a popup");
  press(&window, at, PointerButton::Right);
  let second = window.take_popups().pop().expect("another popup");
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  // The platform drops the first popup for the second.
  drop(first);
  assert!(menu.get_untracked().is_some(), "the second opening stays");
  drop(second);
  assert_eq!(menu.get_untracked(), None);
}

#[test]
fn a_menu_the_app_can_no_longer_give_closes_its_popup() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  let (app, _) = one_item(&mut shell, &bus);
  frame(&window, &mut text);
  let menu = shell.cx.tray().menu();

  press(&window, tray_buttons(&window)[0].0, PointerButton::Right);
  let popup = window.take_popups().pop().expect("a popup");
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  frame(&popup, &mut text);
  assert!(!popup.wants_close());
  // The app stays, but its menu can't be read any more.
  *app.layout.borrow_mut() = None;
  FakeItem::menu_changed(&mut shell, &app.connection, None);
  wait_for(&mut shell, "the popup to close", || popup.wants_close());
  assert_eq!(tray_buttons(&window).len(), 1, "the item stays");
}

#[test]
fn a_panicking_icon_size_or_menu_style_is_logged_and_the_tray_runs_on() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let (_app, _) = one_item(&mut shell, &bus);
  let tray = Tray::new()
    .icon_size(|| -> u32 { panic!("no size") })
    .menu_style(|| -> Style { panic!("no style") });
  let window = Window::new(
    &shell.cx,
    PanelWindow::new(&Output::new("DP-1")).child(tray),
  );
  window.set_size(Size::new(400.0, 30.0));
  let logs = capture_logs(|| frame(&window, &mut text));
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/Row[0]/Button[0]/Icon.size panicked"),
    "{logs}"
  );
  assert_eq!(tray_buttons(&window).len(), 1, "the button is there");

  let menu = shell.cx.tray().menu();
  press(&window, tray_buttons(&window)[0].0, PointerButton::Right);
  let popup = window.take_popups().pop().expect("a popup");
  let logs = capture_logs(|| {
    wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
    frame(&popup, &mut text);
  });
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/Row[0]/Button[0]/PopupWindow/Menu.style panicked"),
    "{logs}"
  );
  assert!(labeled(&popup, "Connect").is_some(), "the menu shows");
}

#[test]
fn a_menu_whose_first_read_fails_closes_its_popup() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  let (app, _) = one_item(&mut shell, &bus);
  frame(&window, &mut text);
  *app.layout.borrow_mut() = None;
  press(&window, tray_buttons(&window)[0].0, PointerButton::Right);
  let popup = window.take_popups().pop().expect("a popup");
  frame(&popup, &mut text);
  assert!(!popup.wants_close());
  // Clear and empty, it would hold the pointer grab until a click outside.
  wait_for(&mut shell, "the popup to close", || popup.wants_close());
}

#[test]
fn a_submenu_has_the_menus_style() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let mut text = TextSystem::new();
  let window = tray_bar(&mut shell, &mut text);
  one_item(&mut shell, &bus);
  frame(&window, &mut text);
  let menu = shell.cx.tray().menu();
  press(&window, tray_buttons(&window)[0].0, PointerButton::Right);
  let popup = window.take_popups().pop().expect("a popup");
  wait_for(&mut shell, "the menu", || menu.get_untracked().is_some());
  frame(&popup, &mut text);
  let later = labeled(&popup, "Later").expect("the Later submenu entry");
  press(&popup, later, PointerButton::Left);
  let submenu = popup.take_popups().pop().expect("the submenu opens");
  frame(&submenu, &mut text);
  let view = submenu.view();
  let column = view.root().children().next().expect("the submenu's menu");
  assert_eq!(
    column.style().background,
    Some(Color::rgb(0x2b, 0x2b, 0x2b))
  );
}

#[test]
fn a_call_that_ends_leaves_no_timer_and_one_that_hangs_times_out() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  one_item(&mut shell, &bus);
  let shared = Rc::clone(&shell.cx.tray().service.expect("the shell's tray").shared);
  let limit = Duration::from_millis(300);

  let answered = {
    let shared = Rc::clone(&shared);
    shell.block_on(async move { shared.timeout(limit, async { Ok(7) }).await })
  };
  assert_eq!(answered.expect("answered in time"), 7);
  // Nothing wakes the loop when the limit would have ended.
  let wakeups = shell.run_until(Instant::now() + limit * 2);
  assert_eq!(wakeups, 1, "only the wait itself ends");

  let hung = shell.block_on(async move {
    let never = std::future::pending::<zbus::Result<()>>();
    shared.timeout(limit, never).await
  });
  let error = hung.expect_err("no answer");
  assert!(error.to_string().contains("no answer in time"), "{error}");
}

#[test]
fn a_tray_hides_and_shares_its_rectangle_as_every_widget_does() {
  let cx = Cx::new(Runtime::current().new_owner());
  let shown = cx.signal(true);
  let geometry = cx.signal(Rect::default());
  let row = crabshell_ui::Row::new()
    .gap(4)
    .child(crabshell_ui::Icon::new("before").size(10))
    .child(Tray::new().visible(move || shown.get()).geometry(geometry));
  let window = Window::new(&cx, PanelWindow::new(&Output::new("DP-1")).child(row));
  window.set_size(Size::new(200.0, 30.0));
  let mut text = TextSystem::new();
  frame(&window, &mut text);
  assert_eq!(geometry.get_untracked(), Rect::new(14.0, 0.0, 0.0, 30.0));
  let shown_children = || {
    let view = window.view();
    let row = view.root().children().next().expect("the row");
    row.children().count()
  };
  assert_eq!(shown_children(), 2);
  shown.set(false);
  frame(&window, &mut text);
  assert_eq!(shown_children(), 1);
  cx.owner().dispose();
}
