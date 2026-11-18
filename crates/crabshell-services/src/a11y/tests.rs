use std::cell::RefCell;
use std::future::poll_fn;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{
  Accessibility, Button, Key, KeyEvent, Modifiers, Output, PanelWindow, Row, Size, Text,
  TextSystem, Window,
};
use serde::de::DeserializeOwned;
use zbus::export::futures_core::Stream;
use zbus::{Connection, ObjectPath, OwnedObjectPath, Variant};

use super::{A11Y_BUS, A11Y_PATH, REGISTRY, ROOT, STATUS, follow_accessibility};
use crate::Config;
use crate::testing::{FakeService, Harness, PrivateBus, capture_logs, offline, reply};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const DESKTOP: &str = "/org/a11y/atspi/accessible/root";

fn online(bus: &PrivateBus) -> Config {
  Config {
    session_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

/// The launcher, saying `enabled` and naming `bus` as the accessibility
/// bus too, and the registry, which keeps each plug it embeds.
struct Fakes {
  launcher: FakeService,
  registry: FakeService,
  plugs: Rc<RefCell<Vec<(String, OwnedObjectPath)>>>,
}

fn fakes(shell: &mut Harness, bus: &PrivateBus, enabled: bool) -> Fakes {
  let launcher = FakeService::start(shell, bus, A11Y_BUS);
  launcher.add(
    A11Y_PATH,
    STATUS,
    &[
      ("IsEnabled", Variant::from(enabled)),
      ("ScreenReaderEnabled", Variant::from(false)),
    ],
  );
  let address = bus.address.clone();
  launcher.on("GetAddress", move |call| reply(call, &address));
  launcher.own(shell);
  let registry = FakeService::start(shell, bus, REGISTRY);
  let plugs = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&plugs);
  registry.on("Embed", move |call| {
    let plug = call
      .body()
      .deserialize::<(String, OwnedObjectPath)>()
      .ok()?;
    kept.borrow_mut().push(plug);
    let desktop = (REGISTRY, ObjectPath::from_static_str_unchecked(DESKTOP));
    reply(call, &(desktop,))
  });
  registry.own(shell);
  Fakes {
    launcher,
    registry,
    plugs,
  }
}

/// The counter bar: a button counting its clicks into `count`.
fn counter(shell: &Harness, count: Signal<u32>) -> Window {
  let panel = PanelWindow::new(&Output::new("DP-1")).child(
    Row::new()
      .child(
        Button::new(Text::new(move || format!("clicked {} times", count.get())))
          .on_click(move |_| count.update(|n| *n += 1)),
      )
      .child(Text::new("hello")),
  );
  let window = Window::new(&shell.cx, panel);
  window.set_size(Size::new(300.0, 30.0));
  lay_out(&window);
  window
}

fn lay_out(window: &Window) {
  Runtime::current().flush();
  window.layout(&mut TextSystem::new());
}

/// A shell with the registry, its counter bar, and the fakes.
fn shell(bus: &PrivateBus, enabled: bool) -> (Harness, Fakes, Window, Signal<u32>) {
  let mut shell = Harness::new(online(bus));
  shell.cx.provide(Accessibility::default());
  let fakes = fakes(&mut shell, bus, enabled);
  let count = shell.cx.signal(0);
  let window = counter(&shell, count);
  follow_accessibility(&shell.cx);
  (shell, fakes, window, count)
}

/// Waits for the shell to embed, and returns its bus name.
fn embedded(shell: &mut Harness, fakes: &Fakes) -> String {
  let plugs = Rc::clone(&fakes.plugs);
  shell.retry_until("the embed", || !plugs.borrow().is_empty(), |_| {});
  let (name, path) = fakes.plugs.borrow()[0].clone();
  assert_eq!(path.as_str(), ROOT);
  name
}

/// A screen reader's call to `member` on the shell's object `path`.
fn call<R: DeserializeOwned + zbus::Type + 'static>(
  shell: &mut Harness,
  client: &Connection,
  (name, path): (&str, &str),
  (interface, member): (&str, &str),
  body: impl serde::Serialize + zbus::DynamicType + 'static,
) -> zbus::Result<R> {
  let (client, name, path) = (client.clone(), name.to_owned(), path.to_owned());
  let (interface, member) = (interface.to_owned(), member.to_owned());
  shell.block_on(async move {
    let reply = client
      .call_method(
        Some(name.as_str()),
        path.as_str(),
        Some(interface.as_str()),
        member.as_str(),
        &body,
      )
      .await?;
    reply.body().deserialize::<R>()
  })
}

fn children(shell: &mut Harness, client: &Connection, at: (&str, &str)) -> Vec<String> {
  let children: Vec<(String, OwnedObjectPath)> =
    call(shell, client, at, (ACCESSIBLE, "GetChildren"), ()).expect("the children");
  children
    .into_iter()
    .map(|(_, path)| path.to_string())
    .collect()
}

fn name_of(shell: &mut Harness, client: &Connection, at: (&str, &str)) -> String {
  let value: zbus::OwnedVariant = call(
    shell,
    client,
    at,
    (crate::dbus::PROPERTIES, "Get"),
    (ACCESSIBLE, "Name"),
  )
  .expect("the name");
  String::try_from(value).expect("a string")
}

/// The counter bar's button, found from the application root as a
/// screen reader walks to it.
fn find_button(shell: &mut Harness, client: &Connection, name: &str) -> String {
  let windows = children(shell, client, (name, ROOT));
  assert_eq!(windows.len(), 1, "one window");
  let row = children(shell, client, (name, &windows[0]));
  let items = children(shell, client, (name, &row[0]));
  assert_eq!(items.len(), 2, "the button and the text");
  items[0].clone()
}

#[test]
fn a_screen_reader_reads_the_counter_bar_and_clicks_its_button() {
  let bus = PrivateBus::start();
  let (mut shell, fakes, window, count) = shell(&bus, true);
  let name = embedded(&mut shell, &fakes);
  let client = shell.block_on(bus.connect(&[]));
  let windows = children(&mut shell, &client, (&name, ROOT));
  let role = |shell: &mut Harness, path: &str| -> u32 {
    call(shell, &client, (&name, path), (ACCESSIBLE, "GetRole"), ()).expect("a role")
  };
  assert_eq!(role(&mut shell, ROOT), 75, "the application");
  assert_eq!(role(&mut shell, &windows[0]), 23, "a frame");
  let button = find_button(&mut shell, &client, &name);
  assert_eq!(role(&mut shell, &button), 43, "a push button");
  assert_eq!(
    name_of(&mut shell, &client, (&name, &button)),
    "clicked 0 times"
  );
  let states: Vec<u32> = call(
    &mut shell,
    &client,
    (&name, &button),
    (ACCESSIBLE, "GetState"),
    (),
  )
  .expect("states");
  // Focusable, enabled, sensitive, showing and visible.
  assert_eq!(states, [1 << 8 | 1 << 11 | 1 << 24 | 1 << 25 | 1 << 30, 0]);
  let interfaces: Vec<String> = call(
    &mut shell,
    &client,
    (&name, &button),
    (ACCESSIBLE, "GetInterfaces"),
    (),
  )
  .expect("interfaces");
  assert!(
    interfaces
      .iter()
      .any(|interface| interface == "org.a11y.atspi.Action")
  );
  let ((x, y, width, height),): ((i32, i32, i32, i32),) = call(
    &mut shell,
    &client,
    (&name, &button),
    ("org.a11y.atspi.Component", "GetExtents"),
    (1u32,),
  )
  .expect("extents");
  assert_eq!((x, y), (0, 0));
  assert!(width > 0 && height == 30, "{width}x{height}");
  // The registry is the application's parent, and the application the
  // window's.
  let parent = |shell: &mut Harness, path: &str| -> (String, String) {
    let value: zbus::OwnedVariant = call(
      shell,
      &client,
      (&name, path),
      (crate::dbus::PROPERTIES, "Get"),
      (ACCESSIBLE, "Parent"),
    )
    .expect("the parent");
    let (bus, path): (String, OwnedObjectPath) = value.try_into().expect("a reference");
    (bus, path.to_string())
  };
  assert_eq!(
    parent(&mut shell, ROOT),
    (REGISTRY.to_owned(), DESKTOP.to_owned())
  );
  assert_eq!(
    parent(&mut shell, &windows[0]),
    (name.clone(), ROOT.to_owned())
  );

  let clicked: bool = call(
    &mut shell,
    &client,
    (&name, &button),
    ("org.a11y.atspi.Action", "DoAction"),
    (0i32,),
  )
  .expect("the click");
  assert!(clicked);
  assert_eq!(count.get_untracked(), 1);
  lay_out(&window);
  assert_eq!(
    name_of(&mut shell, &client, (&name, &button)),
    "clicked 1 times"
  );
  let gone = call::<u32>(
    &mut shell,
    &client,
    (&name, "/org/a11y/atspi/accessible/7_7"),
    (ACCESSIBLE, "GetRole"),
    (),
  );
  assert!(gone.is_err(), "no such object");
  drop(fakes.registry);
}

/// Collects the accessibility events the shell broadcasts, as
/// `member path detail detail1`.
fn listen(shell: &mut Harness, bus: &PrivateBus) -> Rc<RefCell<Vec<String>>> {
  let client = shell.block_on(bus.connect(&[]));
  let rule = "type='signal',interface='org.a11y.atspi.Event.Object'";
  let added = shell.block_on({
    let client = client.clone();
    async move { crate::dbus::bus_call(&client, "AddMatch", rule).await }
  });
  added.expect("the match");
  let heard = Rc::new(RefCell::new(Vec::new()));
  let kept = Rc::clone(&heard);
  let mut messages = zbus::MessageStream::from(&client);
  shell.spawn(async move {
    while let Some(Ok(message)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
      let header = message.header();
      let Some(member) = header.member().map(|member| member.to_string()) else {
        continue;
      };
      let path = header.path().map_or(String::new(), |path| path.to_string());
      if let Ok((detail, one, _, _, _)) = message.body().deserialize::<(
        String,
        i32,
        i32,
        zbus::OwnedVariant,
        std::collections::HashMap<String, zbus::OwnedVariant>,
      )>() {
        kept
          .borrow_mut()
          .push(format!("{member} {path} {detail} {one}"));
      }
    }
  });
  heard
}

#[test]
fn tab_and_a_new_label_are_heard_by_a_screen_reader() {
  let bus = PrivateBus::start();
  let (mut shell, fakes, window, count) = shell(&bus, true);
  let name = embedded(&mut shell, &fakes);
  let heard = listen(&mut shell, &bus);
  let client = shell.block_on(bus.connect(&[]));
  let button = find_button(&mut shell, &client, &name);
  window.keyboard_enter();
  window.key(&KeyEvent {
    key: Key::Tab,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  });
  let focused = format!("StateChanged {button} focused 1");
  let seen = Rc::clone(&heard);
  shell.retry_until(
    "the focus event",
    || seen.borrow().contains(&focused),
    |_| {},
  );
  count.set(5);
  lay_out(&window);
  let renamed = format!("PropertyChange {button} accessible-name 0");
  let seen = Rc::clone(&heard);
  shell.retry_until(
    "the name event",
    || seen.borrow().contains(&renamed),
    |_| {},
  );
}

#[test]
fn with_the_status_off_nothing_joins_until_it_turns_on_and_back() {
  let bus = PrivateBus::start();
  let (mut shell, fakes, _window, _count) = shell(&bus, false);
  shell.settle();
  // Idle: the status read, nothing asked of the launcher or registry.
  let wakeups = shell.run_until(Instant::now() + Duration::from_secs(1));
  assert_eq!(wakeups, 1, "woke while the status said no");
  assert!(fakes.plugs.borrow().is_empty());
  assert!(
    !fakes
      .launcher
      .calls
      .borrow()
      .iter()
      .any(|call| call.starts_with("GetAddress")),
    "asked for the accessibility bus"
  );

  fakes.launcher.set(
    &mut shell,
    A11Y_PATH,
    STATUS,
    &[("ScreenReaderEnabled", Variant::from(true))],
  );
  let name = embedded(&mut shell, &fakes);
  // Still on, so served as it was.
  fakes.launcher.set(
    &mut shell,
    A11Y_PATH,
    STATUS,
    &[("IsEnabled", Variant::from(true))],
  );
  shell.settle();
  assert_eq!(fakes.plugs.borrow().len(), 1, "embedded again");
  let logs = capture_logs(|| {
    fakes.launcher.set(
      &mut shell,
      A11Y_PATH,
      STATUS,
      &[
        ("IsEnabled", Variant::from(false)),
        ("ScreenReaderEnabled", Variant::from(false)),
      ],
    );
    shell.settle();
  });
  assert!(logs.contains("no screen reader listens now"), "{logs}");
  // Its connection to the accessibility bus is gone with its name.
  let client = shell.block_on(bus.connect(&[]));
  let role = call::<u32>(
    &mut shell,
    &client,
    (&name, ROOT),
    (ACCESSIBLE, "GetRole"),
    (),
  );
  assert!(role.is_err(), "still served");
  let wakeups = shell.run_until(Instant::now() + Duration::from_secs(1));
  assert_eq!(wakeups, 1, "woke after it stopped");
}

#[test]
fn a_panicking_click_is_logged_and_the_shell_still_answers() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  shell.cx.provide(Accessibility::default());
  let fakes = fakes(&mut shell, &bus, true);
  let panel = PanelWindow::new(&Output::new("DP-1"))
    .child(Row::new().child(Button::new(Text::new("boom")).on_click(|_| panic!("clicked badly"))));
  let window = Window::new(&shell.cx, panel);
  window.set_size(Size::new(300.0, 30.0));
  lay_out(&window);
  follow_accessibility(&shell.cx);
  let name = embedded(&mut shell, &fakes);
  let client = shell.block_on(bus.connect(&[]));
  let windows = children(&mut shell, &client, (&name, ROOT));
  let row = children(&mut shell, &client, (&name, &windows[0]));
  let button = children(&mut shell, &client, (&name, &row[0]))[0].clone();
  let logs = capture_logs(|| {
    let clicked: bool = call(
      &mut shell,
      &client,
      (&name, &button),
      ("org.a11y.atspi.Action", "DoAction"),
      (0i32,),
    )
    .expect("an answer");
    assert!(clicked);
  });
  assert!(
    logs.contains(
      "handler PanelWindow[DP-1]/Row/Button[0].on_click panicked and is disabled: clicked badly"
    ),
    "{logs}"
  );
  assert_eq!(name_of(&mut shell, &client, (&name, &button)), "boom");
}

#[test]
fn what_a_screen_reader_caches_by_is_announced() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  shell.cx.provide(Accessibility::default());
  let fakes = fakes(&mut shell, &bus, true);
  let (on, shown) = (shell.cx.signal(true), shell.cx.signal(true));
  let items = shell.cx.signal(vec![1]);
  let panel = PanelWindow::new(&Output::new("DP-1")).child(
    Row::new()
      .child(
        Button::new(Text::new("mute"))
          .enabled(move || on.get())
          .visible(move || shown.get()),
      )
      .children_keyed(
        move || items.get(),
        |n| *n,
        |_, n| Text::new(move || n.get().to_string()),
      ),
  );
  let window = Window::new(&shell.cx, panel);
  window.set_size(Size::new(300.0, 30.0));
  lay_out(&window);
  follow_accessibility(&shell.cx);
  let name = embedded(&mut shell, &fakes);
  let heard = listen(&mut shell, &bus);
  let client = shell.block_on(bus.connect(&[]));
  let windows = children(&mut shell, &client, (&name, ROOT));
  let row = children(&mut shell, &client, (&name, &windows[0]))[0].clone();
  let button = children(&mut shell, &client, (&name, &row))[0].clone();
  let wait = |shell: &mut Harness, what: String| {
    let seen = Rc::clone(&heard);
    shell.retry_until(&what.clone(), move || seen.borrow().contains(&what), |_| {});
  };
  on.set(false);
  lay_out(&window);
  wait(&mut shell, format!("StateChanged {button} enabled 0"));
  wait(&mut shell, format!("StateChanged {button} sensitive 0"));
  shown.set(false);
  lay_out(&window);
  wait(&mut shell, format!("StateChanged {button} showing 0"));
  wait(&mut shell, format!("StateChanged {button} visible 0"));
  items.set(vec![1, 2]);
  lay_out(&window);
  wait(&mut shell, format!("ChildrenChanged {row} add 2"));
  // A second window opens, then closes.
  let other = counter(&shell, shell.cx.signal(0));
  wait(&mut shell, format!("ChildrenChanged {ROOT} add 1"));
  drop(other);
  wait(&mut shell, format!("ChildrenChanged {ROOT} remove 1"));
}

#[test]
fn while_locked_a_panel_button_is_unknown_and_never_clicked() {
  let bus = PrivateBus::start();
  let (mut shell, fakes, _window, count) = shell(&bus, true);
  let name = embedded(&mut shell, &fakes);
  let client = shell.block_on(bus.connect(&[]));
  let button = find_button(&mut shell, &client, &name);
  let lock = crabshell_ui::Lock::new(
    &shell.cx,
    crabshell_ui::SessionLock::new().surface(|_, _| Text::new("locked")),
  );
  let surface = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  surface.set_size(Size::new(300.0, 30.0));
  lay_out(&surface);
  let clicked = call::<bool>(
    &mut shell,
    &client,
    (&name, &button),
    ("org.a11y.atspi.Action", "DoAction"),
    (0i32,),
  );
  assert!(clicked.is_err(), "the button behind the lock answered");
  assert_eq!(count.get_untracked(), 0);
  let windows = children(&mut shell, &client, (&name, ROOT));
  assert_eq!(windows.len(), 1);
  assert_eq!(name_of(&mut shell, &client, (&name, &windows[0])), "");
  let surface_children = children(&mut shell, &client, (&name, &windows[0]));
  assert_eq!(
    name_of(&mut shell, &client, (&name, &surface_children[0])),
    "locked"
  );
}

#[test]
fn a_refused_embed_is_tried_again_at_the_next_word_from_the_status() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  shell.cx.provide(Accessibility::default());
  let fakes = fakes(&mut shell, &bus, false);
  // The registry leaves before the screen reader comes, so Embed fails.
  let Fakes {
    launcher,
    registry,
    plugs,
  } = fakes;
  registry.leave(&mut shell);
  let _window = counter(&shell, shell.cx.signal(0));
  follow_accessibility(&shell.cx);
  shell.settle();
  let on = [("IsEnabled", Variant::from(true))];
  let logs = capture_logs(|| {
    launcher.set(&mut shell, A11Y_PATH, STATUS, &on);
    shell.settle();
  });
  assert!(
    logs.contains("the accessibility registry refused the shell"),
    "{logs}"
  );
  assert!(plugs.borrow().is_empty());
  let registry = FakeService::start(&mut shell, &bus, REGISTRY);
  let kept = Rc::clone(&plugs);
  registry.on("Embed", move |call| {
    kept.borrow_mut().push(call.body().deserialize().ok()?);
    let desktop = (REGISTRY, ObjectPath::from_static_str_unchecked(DESKTOP));
    reply(call, &(desktop,))
  });
  registry.own(&mut shell);
  launcher.set(&mut shell, A11Y_PATH, STATUS, &on);
  let seen = Rc::clone(&plugs);
  shell.retry_until("the second embed", || !seen.borrow().is_empty(), |_| {});
}
