use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use crabshell_reactive::{ReadSignal, Runtime};
use crabshell_ui::{Cx, IconPixels, IconSource};
use zbus::export::futures_core::Stream;
use zbus::message::Type as Kind;
use zbus::{Connection, MessageStream, Structure, Variant};

use super::server::{RawImage, cut_markup, icon, image_pixels};
use super::{
  CloseReason, MAX_KEPT, NOTIFICATIONS, NOTIFICATIONS_PATH, Notification, Notifications, Service,
  Timeout, Urgency, keep_pixels_within,
};
use crate::testing::{Harness, PrivateBus, capture_logs, offline};
use crate::{Config, ServicesExt};

fn online(bus: &PrivateBus) -> Config {
  Config {
    session_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  }
}

fn service(notifications: &Notifications) -> Rc<Service> {
  Rc::clone(notifications.service.as_ref().expect("the shell's server"))
}

fn wait_for(shell: &mut Harness, what: &str, done: impl Fn() -> bool) {
  shell.retry_until(what, done, |_| {});
}

/// A shell serving notifications on `bus` with `config`, and an app's
/// connection to the same bus.
fn serve(bus: &PrivateBus, config: Config) -> (Harness, Notifications, Connection) {
  let mut shell = Harness::new(config);
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  wait_for(&mut shell, "the server", || server.serving());
  let app = shell.block_on(bus.connect(&[]));
  (shell, notifications, app)
}

/// A `Notify` call as an app makes it.
struct Note {
  replaces: u32,
  app_icon: &'static str,
  summary: String,
  body: String,
  actions: Vec<&'static str>,
  hints: Vec<(&'static str, Variant<'static>)>,
  timeout: i32,
}

fn note(summary: &str) -> Note {
  Note {
    replaces: 0,
    app_icon: "",
    summary: summary.to_owned(),
    body: "the body".to_owned(),
    actions: Vec::new(),
    hints: Vec::new(),
    timeout: -1,
  }
}

fn notify(shell: &mut Harness, app: &Connection, note: Note) -> zbus::Result<u32> {
  let app = app.clone();
  shell.block_on(async move {
    let hints: HashMap<&str, Variant<'_>> = note.hints.into_iter().collect();
    let body = (
      "test app",
      note.replaces,
      note.app_icon,
      note.summary.as_str(),
      note.body.as_str(),
      note.actions,
      hints,
      note.timeout,
    );
    let call = app.call_method(
      Some(NOTIFICATIONS),
      NOTIFICATIONS_PATH,
      Some(NOTIFICATIONS),
      "Notify",
      &body,
    );
    call.await?.body().deserialize::<u32>()
  })
}

/// Calls `member` with `body` on the server, and returns the reply.
fn call<B>(
  shell: &mut Harness,
  app: &Connection,
  member: &'static str,
  body: B,
) -> zbus::Result<zbus::Message>
where
  B: serde::Serialize + zbus::DynamicType + 'static,
{
  let app = app.clone();
  shell.block_on(async move {
    let call = app.call_method(
      Some(NOTIFICATIONS),
      NOTIFICATIONS_PATH,
      Some(NOTIFICATIONS),
      member,
      &body,
    );
    call.await
  })
}

/// Every signal the server sends, as another app hears it, such as
/// `NotificationClosed 1 3`.
fn listen(shell: &mut Harness, bus: &PrivateBus) -> Rc<RefCell<Vec<String>>> {
  let listener = shell.block_on(bus.connect(&[]));
  let rule = zbus::MatchRule::builder()
    .msg_type(Kind::Signal)
    .interface(NOTIFICATIONS)
    .build()
    .expect("a match rule for the server");
  let mut messages = shell
    .block_on(async move { MessageStream::for_match_rule(rule, &listener, None).await })
    .expect("a match on the private bus");
  let heard: Rc<RefCell<Vec<String>>> = Rc::default();
  let sink = Rc::clone(&heard);
  shell.spawn(async move {
    while let Some(Ok(signal)) = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await {
      let member = signal
        .header()
        .member()
        .map_or(String::new(), |m| m.to_string());
      let body = signal.body();
      let line = match member.as_str() {
        "NotificationClosed" => body
          .deserialize::<(u32, u32)>()
          .map(|(id, reason)| format!("{member} {id} {reason}")),
        _ => body
          .deserialize::<(u32, String)>()
          .map(|(id, key)| format!("{member} {id} {key}")),
      };
      sink
        .borrow_mut()
        .push(line.expect("a signal of the spec's type"));
    }
  });
  heard
}

/// What `on_closed` hears from `notifications`, as each summary and why.
fn closes(notifications: &Notifications) -> Rc<RefCell<Vec<(String, CloseReason)>>> {
  let heard: Rc<RefCell<Vec<_>>> = Rc::default();
  let sink = Rc::clone(&heard);
  notifications.on_closed(move |gone, reason| {
    sink.borrow_mut().push((gone.summary.clone(), reason));
  });
  heard
}

fn heard(summary: &str, reason: CloseReason) -> (String, CloseReason) {
  (summary.to_owned(), reason)
}

fn ids(signal: ReadSignal<Vec<Notification>>) -> Vec<u32> {
  signal.get_untracked().iter().map(|n| n.id).collect()
}

fn sample(summary: &str) -> Notification {
  Notification {
    id: 0,
    app_name: "app".to_owned(),
    app_icon: String::new(),
    summary: summary.to_owned(),
    body: String::new(),
    actions: Vec::new(),
    urgency: Urgency::Normal,
    timeout: Timeout::Default,
    image: None,
    desktop_entry: String::new(),
    category: String::new(),
    resident: false,
    transient: false,
    received: SystemTime::now(),
  }
}

/// A picture hint: `width` by `height` pixels of `channels` bytes each,
/// in rows of `stride` bytes.
fn picture(width: i32, height: i32, stride: i32, channels: i32, data: Vec<u8>) -> Variant<'static> {
  let alpha = channels == 4;
  Variant::from(Structure::from((
    width, height, stride, alpha, 8, channels, data,
  )))
}

#[test]
fn a_notify_shows_in_the_list_and_the_popups_and_returns_its_id() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let mut first = note("hello");
  first.actions = vec!["default", "Open", "reply", "Reply"];
  first.hints = vec![
    ("urgency", Variant::from(0u8)),
    ("desktop-entry", Variant::from("org.example.App")),
    ("category", Variant::from("im.received")),
    ("resident", Variant::from(true)),
  ];
  first.timeout = 3000;
  assert_eq!(notify(&mut shell, &app, first).expect("an id"), 1);
  let list = notifications.list();
  wait_for(&mut shell, "the notification", || ids(list) == [1]);
  let shown = list.get_untracked()[0].clone();
  assert_eq!(shown.app_name, "test app");
  assert_eq!(shown.summary, "hello");
  assert_eq!(shown.body, "the body");
  assert_eq!(
    shown.actions,
    [
      ("default".to_owned(), "Open".to_owned()),
      ("reply".to_owned(), "Reply".to_owned())
    ]
  );
  assert_eq!(shown.urgency, Urgency::Low);
  assert_eq!(shown.timeout, Timeout::After(Duration::from_secs(3)));
  assert_eq!(shown.desktop_entry, "org.example.App");
  assert_eq!(shown.category, "im.received");
  assert!(shown.resident && !shown.transient);
  assert_eq!(shown.image, None);

  assert_eq!(notify(&mut shell, &app, note("again")).expect("an id"), 2);
  assert_eq!(ids(list), [1, 2], "oldest first");
  assert_eq!(ids(notifications.popups()), [2, 1], "newest first");
  assert_eq!(list.get_untracked()[1].urgency, Urgency::Normal);
  assert_eq!(list.get_untracked()[1].timeout, Timeout::Default);
}

#[test]
fn replacing_keeps_the_place_and_a_gone_id_names_the_new_one() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let first = notify(&mut shell, &app, note("first")).expect("an id");
  let second = notify(&mut shell, &app, note("second")).expect("an id");
  let mut again = note("first, again");
  again.replaces = first;
  assert_eq!(notify(&mut shell, &app, again).expect("an id"), first);
  let summaries = |signal: ReadSignal<Vec<Notification>>| -> Vec<String> {
    signal
      .get_untracked()
      .into_iter()
      .map(|n| n.summary)
      .collect()
  };
  assert_eq!(summaries(notifications.list()), ["first, again", "second"]);
  assert_eq!(ids(notifications.popups()), [second, first]);

  let mut gone = note("new");
  gone.replaces = 99;
  assert_eq!(notify(&mut shell, &app, gone).expect("an id"), 99);
  assert_eq!(ids(notifications.list()), [first, second, 99]);
  assert_eq!(notify(&mut shell, &app, note("next")).expect("an id"), 3);
}

#[test]
fn close_notification_takes_it_away_with_reason_3_and_refuses_an_unknown_id() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let heard = listen(&mut shell, &bus);
  let closed = closes(&notifications);
  let id = notify(&mut shell, &app, note("hello")).expect("an id");
  call(&mut shell, &app, "CloseNotification", id).expect("closed");
  assert_eq!(
    *closed.borrow(),
    [self::heard("hello", CloseReason::CloseRequested)]
  );
  assert_eq!(ids(notifications.list()), [0u32; 0]);
  assert_eq!(ids(notifications.popups()), [0u32; 0]);
  wait_for(&mut shell, "the signal", || !heard.borrow().is_empty());
  assert_eq!(*heard.borrow(), [format!("NotificationClosed {id} 3")]);

  let error = call(&mut shell, &app, "CloseNotification", id).expect_err("gone");
  assert!(error.to_string().contains("InvalidArgs"), "{error}");
}

#[test]
fn dismissing_invoking_and_clearing_tell_the_app() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let heard = listen(&mut shell, &bus);
  let mut plain = note("plain");
  plain.actions = vec!["default", "Open"];
  let plain = notify(&mut shell, &app, plain).expect("an id");
  let mut resident = note("resident");
  resident.actions = vec!["reply", "Reply"];
  resident.hints = vec![("resident", Variant::from(true))];
  let resident = notify(&mut shell, &app, resident).expect("an id");
  let third = notify(&mut shell, &app, note("third")).expect("an id");
  let fourth = notify(&mut shell, &app, note("fourth")).expect("an id");

  notifications.invoke(plain, "default");
  notifications.invoke(resident, "reply");
  let logs = capture_logs(|| notifications.invoke(resident, "frobnicate"));
  assert!(logs.contains("no such notification action"), "{logs}");
  assert_eq!(ids(notifications.list()), [resident, third, fourth]);
  notifications.dismiss(third);
  assert_eq!(ids(notifications.list()), [resident, fourth]);
  notifications.clear();
  assert_eq!(ids(notifications.list()), [0u32; 0]);
  assert_eq!(ids(notifications.popups()), [0u32; 0]);
  wait_for(&mut shell, "the signals", || heard.borrow().len() == 6);
  assert_eq!(
    *heard.borrow(),
    [
      format!("ActionInvoked {plain} default"),
      format!("NotificationClosed {plain} 2"),
      format!("ActionInvoked {resident} reply"),
      format!("NotificationClosed {third} 2"),
      format!("NotificationClosed {resident} 2"),
      format!("NotificationClosed {fourth} 2"),
    ]
  );
}

#[test]
fn close_handlers_hear_each_notification_that_goes_and_why() {
  let config = Config {
    popup_timeout: Duration::from_millis(50),
    ..offline()
  };
  let mut shell = Harness::new(config);
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let closed = closes(&notifications);
  let list = notifications.list();
  let shown = Rc::new(Cell::new(false));
  let seen = Rc::clone(&shown);
  notifications.on_closed(move |gone, _| {
    let still = list.get_untracked().iter().any(|kept| kept.id == gone.id);
    seen.set(seen.get() || still);
  });
  let mut picked = sample("picked");
  picked.actions = vec![("default".into(), "Open".into())];
  let picked = server.notify(0, picked);
  let dismissed = server.notify(0, sample("dismissed"));
  let mut transient = sample("transient");
  transient.transient = true;
  server.notify(0, transient);
  let kept = server.notify(0, sample("kept"));

  notifications.invoke(picked, "default");
  notifications.dismiss(dismissed);
  assert_eq!(
    *closed.borrow(),
    [
      heard("picked", CloseReason::Dismissed),
      heard("dismissed", CloseReason::Dismissed)
    ]
  );
  wait_for(&mut shell, "the transient one to expire", || {
    closed.borrow().len() == 3
  });
  assert_eq!(closed.borrow()[2], heard("transient", CloseReason::Expired));
  // Its popup expired too, but it stays in the history.
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(closed.borrow().len(), 3);
  assert_eq!(ids(notifications.list()), [kept]);
  notifications.clear();
  assert_eq!(closed.borrow()[3], heard("kept", CloseReason::Dismissed));

  // The oldest goes when the history is full.
  for n in 0..=MAX_KEPT {
    server.notify(0, sample(&n.to_string()));
  }
  assert_eq!(closed.borrow().len(), 5);
  assert_eq!(closed.borrow()[4], heard("0", CloseReason::Expired));
  assert!(
    !shown.get(),
    "a handler runs once the list no longer shows it"
  );
}

#[test]
fn a_panicking_close_handler_is_logged_and_taken_away_and_the_rest_run_on() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let first = server.notify(0, sample("first"));
  let second = server.notify(0, sample("second"));
  let panics = Rc::new(Cell::new(0));
  let counted = Rc::clone(&panics);
  notifications.on_closed(move |gone, _| {
    counted.set(counted.get() + 1);
    panic!("bad close of {}", gone.summary);
  });
  let closed = closes(&notifications);
  let inner = notifications.clone();
  notifications.on_closed(move |gone, _| {
    // Nothing of the service is borrowed while a handler runs.
    if gone.id == first {
      inner.dismiss(second);
    }
  });
  let logs = capture_logs(|| notifications.dismiss(first));
  let at =
    "notifications on_closed (called at crates/crabshell-services/src/notifications/tests.rs:";
  assert!(logs.contains(at), "{logs}");
  assert!(logs.contains("panicked: bad close of first"), "{logs}");
  assert_eq!(
    panics.get(),
    1,
    "a handler that panicked isn't called again"
  );
  assert_eq!(
    *closed.borrow(),
    [
      heard("first", CloseReason::Dismissed),
      heard("second", CloseReason::Dismissed)
    ]
  );
  assert_eq!(ids(notifications.list()), [0u32; 0]);
}

#[test]
fn what_a_close_handler_reads_never_reruns_the_effect_that_closed() {
  let mut shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  server.notify(0, sample("hello"));
  let theme = shell.cx.signal(0);
  notifications.on_closed(move |_, _| {
    theme.get();
  });
  let runs = Rc::new(Cell::new(0));
  let counted = Rc::clone(&runs);
  let closer = notifications.clone();
  shell.cx.effect(move || {
    counted.set(counted.get() + 1);
    closer.clear();
  });
  shell.run_until(Instant::now() + Duration::from_millis(10));
  assert_eq!(ids(notifications.list()), [0u32; 0]);
  theme.set(1);
  shell.run_until(Instant::now() + Duration::from_millis(10));
  assert_eq!(runs.get(), 1);
}

#[test]
fn a_close_handler_goes_with_its_context() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let id = server.notify(0, sample("hello"));
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let from_child = child.notifications();
  let closed = closes(&from_child);
  child.owner().dispose();
  assert_eq!(Runtime::current().node_count(), nodes);
  assert_eq!(Rc::strong_count(&closed), 1, "the handler was dropped");
  notifications.dismiss(id);
  assert_eq!(*closed.borrow(), []);

  let logs = capture_logs(|| from_child.on_closed(|_, _| {}));
  assert!(
    logs.contains("on a context that is gone does nothing"),
    "{logs}"
  );
  let listeners = || {
    server
      .state
      .borrow()
      .as_ref()
      .map_or(0, |state| state.listeners.len())
  };
  assert_eq!(listeners(), 0);

  // A handler that ends another's context first keeps it from running.
  let child = shell.cx.child();
  let owner = child.owner();
  notifications.on_closed(move |_, _| owner.dispose());
  let late = closes(&child.notifications());
  let id = server.notify(0, sample("late"));
  notifications.dismiss(id);
  assert_eq!(*late.borrow(), []);
  assert_eq!(listeners(), 1);
}

#[test]
fn popups_expire_on_one_timer_that_goes_after_the_last() {
  let bus = PrivateBus::start();
  let config = Config {
    popup_timeout: Duration::from_millis(400),
    ..online(&bus)
  };
  let (mut shell, notifications, app) = serve(&bus, config);
  let heard = listen(&mut shell, &bus);
  let server = service(&notifications);
  let usual = notify(&mut shell, &app, note("usual")).expect("an id");
  let mut transient = note("transient");
  transient.hints = vec![("transient", Variant::from(true))];
  transient.timeout = 800;
  let transient = notify(&mut shell, &app, transient).expect("an id");
  let mut never = note("never");
  never.timeout = 0;
  let never = notify(&mut shell, &app, never).expect("an id");
  let mut critical = note("critical");
  critical.hints = vec![("urgency", Variant::from(2u8))];
  critical.timeout = 50;
  let critical = notify(&mut shell, &app, critical).expect("an id");
  let (list, popups) = (notifications.list(), notifications.popups());
  assert_eq!(ids(popups), [critical, never, transient, usual]);

  wait_for(&mut shell, "the usual one to expire", || {
    ids(popups) == [critical, never, transient]
  });
  assert_eq!(
    ids(list),
    [usual, transient, never, critical],
    "it stays kept"
  );
  wait_for(&mut shell, "the transient one to expire", || {
    ids(popups) == [critical, never]
  });
  assert_eq!(ids(list), [usual, never, critical], "a transient one goes");
  wait_for(&mut shell, "its signal", || !heard.borrow().is_empty());

  let timer = || server.state.borrow().as_ref().and_then(|state| state.timer);
  assert!(timer().is_none(), "no popup left to expire");
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);
  assert_eq!(
    *heard.borrow(),
    [format!("NotificationClosed {transient} 1")]
  );
  assert_eq!(ids(popups), [critical, never]);
}

#[test]
fn the_methods_answer_as_the_spec_says() {
  let bus = PrivateBus::start();
  let (mut shell, _notifications, app) = serve(&bus, online(&bus));
  let reply = call(&mut shell, &app, "GetCapabilities", ()).expect("the capabilities");
  let capabilities: Vec<String> = reply.body().deserialize().expect("as");
  assert_eq!(
    capabilities,
    ["actions", "body", "icon-static", "persistence"]
  );
  let reply = call(&mut shell, &app, "GetServerInformation", ()).expect("the server");
  let information: (String, String, String, String) = reply.body().deserialize().expect("ssss");
  let version = env!("CARGO_PKG_VERSION").to_owned();
  assert_eq!(
    information,
    (
      "crabshell".into(),
      "crabshell".into(),
      version,
      "1.2".into()
    )
  );
  let reply = call(&mut shell, &app, "Introspect", ()).expect("the introspection");
  let xml: String = reply.body().deserialize().expect("s");
  assert!(xml.contains(r#"<method name="Notify">"#), "{xml}");

  let error = call(&mut shell, &app, "Notify", "not a notification").expect_err("refused");
  assert!(error.to_string().contains("InvalidArgs"), "{error}");
  let error = call(&mut shell, &app, "Frobnicate", ()).expect_err("refused");
  assert!(error.to_string().contains("UnknownMethod"), "{error}");
}

#[test]
fn the_shell_waits_for_another_daemon_and_steps_back_for_one_that_replaces_it() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let other = shell.block_on(bus.connect(&[NOTIFICATIONS]));
  let mut asked = None;
  let logs = capture_logs(|| {
    asked = Some(shell.cx.notifications());
    shell.run_until(Instant::now() + Duration::from_millis(300));
  });
  let notifications = asked.expect("the notifications");
  assert!(
    logs.contains("another program serves notifications"),
    "{logs}"
  );
  let server = service(&notifications);
  assert!(!server.serving());
  // Called by its own name meanwhile, it refuses.
  let unique = server.state.borrow().as_ref().and_then(|state| {
    let connection = state.connection.as_ref()?;
    Some(connection.unique_name()?.to_string())
  });
  let unique = unique.expect("the shell's name on the bus");
  let app = shell.block_on(bus.connect(&[]));
  let caller = app.clone();
  let refused = shell.block_on(async move {
    let information = "GetServerInformation";
    let path = NOTIFICATIONS_PATH;
    let call = caller.call_method(
      Some(unique.as_str()),
      path,
      Some(NOTIFICATIONS),
      information,
      &(),
    );
    call.await.expect_err("not serving").to_string()
  });
  assert!(refused.contains("not serving"), "{refused}");

  shell
    .block_on(other.close())
    .expect("the other daemon leaves");
  wait_for(&mut shell, "the shell to serve", || server.serving());
  let id = notify(&mut shell, &app, note("hello")).expect("an id");

  let replacer = shell.block_on(bus.connect(&[NOTIFICATIONS]));
  wait_for(&mut shell, "the shell to step back", || !server.serving());
  assert_eq!(ids(notifications.list()), [id], "the history stays");
  shell
    .block_on(replacer.close())
    .expect("the replacer leaves");
  wait_for(&mut shell, "the shell to serve again", || server.serving());
}

#[test]
fn pictures_convert_to_argb_and_bad_ones_are_refused() {
  // Two RGB pixels in a row padded to 8 bytes, then two more unpadded.
  let rgb = [1, 2, 3, 4, 5, 6, 0, 0, 7, 8, 9, 10, 11, 12];
  let pixels = image_pixels(&(2, 2, 8, false, 8, 3, &rgb)).expect("a picture");
  assert_eq!((pixels.width, pixels.height), (2, 2));
  assert_eq!(
    &*pixels.data,
    [255, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12]
  );
  let rgba = [10, 20, 30, 40];
  let pixels = image_pixels(&(1, 1, 4, true, 8, 4, &rgba)).expect("a picture");
  assert_eq!(&*pixels.data, [40, 10, 20, 30]);

  let wide = vec![0; 513 * 4];
  let huge = vec![0; 1024 * 1024 + 4];
  let refused: [RawImage<'_>; 9] = [
    (513, 1, 513 * 4, true, 8, 4, &wide),
    (0, 1, 0, true, 8, 4, &[]),
    (-1, 1, 4, true, 8, 4, &rgba),
    (1, 1, 4, true, 16, 4, &rgba),
    (1, 1, 4, false, 8, 4, &rgba),
    (2, 1, 8, true, 8, 4, &rgba),
    (1, 1, 2, true, 8, 4, &rgba),
    (1, 1, -4, true, 8, 4, &rgba),
    (1, 1, 4, true, 8, 4, &huge),
  ];
  for raw in refused {
    assert_eq!(image_pixels(&raw), None, "{:?}", (raw.0, raw.1, raw.2));
  }
}

#[test]
fn icons_and_paths_name_files_or_theme_icons() {
  assert_eq!(
    icon("mail-unread"),
    Some(IconSource::Name("mail-unread".into()))
  );
  assert_eq!(icon("/a/b.png"), Some(IconSource::File("/a/b.png".into())));
  assert_eq!(
    icon("file:///tmp/a%20b%C3%A9.png"),
    Some(IconSource::File(PathBuf::from("/tmp/a bé.png")))
  );
  for nothing in [
    "",
    "icons/x.png",
    "http://example.com/x.png",
    "file://host/x",
    "file:///a%2",
  ] {
    assert_eq!(icon(nothing), None, "{nothing}");
  }
}

#[test]
fn the_picture_comes_from_the_hints_in_the_specs_order() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let rgba = || vec![10u8, 20, 30, 40];
  let argb = IconSource::Pixels(IconPixels {
    width: 1,
    height: 1,
    data: vec![40, 10, 20, 30].into(),
  });
  // The app's icon, the hints, and the picture they make.
  type Case = (
    &'static str,
    Vec<(&'static str, Variant<'static>)>,
    Option<IconSource>,
  );
  let cases: Vec<Case> = vec![
    (
      "mail-unread",
      vec![
        ("image-data", picture(1, 1, 4, 4, rgba())),
        ("image-path", Variant::from("/a.png")),
      ],
      Some(argb.clone()),
    ),
    (
      "mail-unread",
      vec![("image_data", picture(1, 1, 4, 4, rgba()))],
      Some(argb.clone()),
    ),
    (
      "mail-unread",
      vec![("image-path", Variant::from("file:///a.png"))],
      Some(IconSource::File("/a.png".into())),
    ),
    (
      "mail-unread",
      vec![("image_path", Variant::from("/b.png"))],
      Some(IconSource::File("/b.png".into())),
    ),
    (
      "mail-unread",
      vec![
        ("image-data", picture(513, 1, 513 * 4, 4, vec![0; 513 * 4])),
        ("image-path", Variant::from(7u32)),
        ("icon_data", picture(1, 1, 4, 4, rgba())),
      ],
      Some(IconSource::Name("mail-unread".into())),
    ),
    (
      "",
      vec![("icon_data", picture(1, 1, 4, 4, rgba()))],
      Some(argb),
    ),
    ("", vec![("image-data", Variant::from("a picture"))], None),
  ];
  let expected: Vec<Option<IconSource>> = cases.iter().map(|case| case.2.clone()).collect();
  for (app_icon, hints, _) in cases {
    let mut sent = note("picture");
    sent.app_icon = app_icon;
    sent.hints = hints;
    notify(&mut shell, &app, sent).expect("an id");
  }
  let images: Vec<Option<IconSource>> = notifications
    .list()
    .get_untracked()
    .into_iter()
    .map(|n| n.image)
    .collect();
  assert_eq!(images, expected);
}

#[test]
fn outside_data_is_cut_to_the_caps_and_wrong_types_read_as_defaults() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let heard = listen(&mut shell, &bus);
  let mut big = note(&"€".repeat(400));
  big.body = "x".repeat(20_000);
  big.actions = std::iter::repeat_n(["key", "label"], 20)
    .flatten()
    .chain(["odd"])
    .collect();
  big.hints = vec![
    ("urgency", Variant::from("critical")),
    ("resident", Variant::from("yes")),
    ("transient", Variant::from(1i32)),
    ("desktop-entry", Variant::from(5u32)),
    // Read by no one, and skipped unread.
    ("x-kde-urls", Variant::from(vec!["file:///a", "file:///b"])),
  ];
  big.timeout = -5;
  notify(&mut shell, &app, big).expect("an id");
  let kept = notifications.list().get_untracked()[0].clone();
  assert_eq!(kept.summary.len(), 1023, "cut before a split €");
  assert_eq!(kept.body.len(), 4 * 1024);
  assert_eq!(kept.actions.len(), 16);
  assert_eq!(kept.urgency, Urgency::Normal);
  assert!(!kept.resident);
  assert!(
    kept.transient,
    "an integer flag counts, as notify-send -h int:transient:1 sends"
  );
  assert_eq!(kept.desktop_entry, "");
  assert_eq!(kept.timeout, Timeout::Default);

  let server = service(&notifications);
  for _ in 0..MAX_KEPT {
    server.notify(0, sample("one more"));
  }
  let list = notifications.list();
  assert_eq!(list.get_untracked().len(), MAX_KEPT);
  assert_eq!(ids(list)[0], 2, "the oldest went");
  wait_for(&mut shell, "its signal", || !heard.borrow().is_empty());
  assert_eq!(*heard.borrow(), ["NotificationClosed 1 1"]);
  assert_eq!(notifications.popups().get_untracked().len(), MAX_KEPT);
}

#[test]
fn pictures_past_the_budget_fall_back_to_the_app_icon_oldest_first() {
  let with_pixels = |app_icon: &str, bytes: usize| Notification {
    app_icon: app_icon.to_owned(),
    image: Some(IconSource::Pixels(IconPixels {
      width: 1,
      height: 1,
      data: vec![0; bytes].into(),
    })),
    ..sample("picture")
  };
  let mut history = [
    with_pixels("oldest", 8),
    with_pixels("old", 4),
    sample("none"),
    with_pixels("new", 4),
  ];
  keep_pixels_within(&mut history, 8);
  let images: Vec<_> = history.iter().map(|n| n.image.clone()).collect();
  assert_eq!(images[0], Some(IconSource::Name("oldest".into())));
  assert!(matches!(images[1], Some(IconSource::Pixels(_))));
  assert_eq!(images[2], None);
  assert!(matches!(images[3], Some(IconSource::Pixels(_))));
}

#[test]
fn ids_skip_zero_and_those_kept_when_they_wrap() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let set_next = |id| server.state.borrow_mut().as_mut().expect("started").next_id = id;
  set_next(u32::MAX);
  assert_eq!(server.notify(0, sample("last")), u32::MAX);
  assert_eq!(server.notify(0, sample("wrapped")), 1);
  set_next(u32::MAX);
  assert_eq!(server.notify(0, sample("past both")), 2);
}

#[test]
fn an_idle_server_wakes_nothing_and_no_session_bus_is_quiet() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let mut never = note("stays");
  never.timeout = 0;
  notify(&mut shell, &app, never).expect("an id");
  assert_eq!(ids(notifications.popups()), [1]);
  shell.run_until(Instant::now() + Duration::from_millis(200));
  assert_eq!(shell.run_until(Instant::now() + Duration::from_secs(2)), 1);

  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    let notifications = shell.cx.notifications();
    let woke = shell.run_until(Instant::now() + Duration::from_millis(200));
    assert_eq!(woke, 1);
    assert_eq!(ids(notifications.list()), [0u32; 0]);
  });
  assert!(logs.contains("no session bus"), "{logs}");

  let mut shell = Harness::new(Config {
    session_bus: Some(
      "unix:path=/nonexistent/crabshell-test-bus"
        .parse()
        .expect("an address"),
    ),
    ..offline()
  });
  let logs = capture_logs(|| {
    shell.cx.notifications();
    shell.run_until(Instant::now() + Duration::from_millis(100));
  });
  assert!(logs.contains("can't reach the session bus"), "{logs}");
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_millis(500)),
    1
  );
}

#[test]
fn every_caller_shares_one_server_and_without_provide_it_is_empty() {
  let shell = Harness::new(offline());
  let list = shell.cx.notifications().list();
  let output = shell.cx.child();
  assert_eq!(output.notifications().list(), list);
  output.owner().dispose();
  assert!(list.try_get().is_some());

  let cx = Cx::new(Runtime::current().new_owner());
  let list = cx.notifications().list();
  assert_eq!(list.get_untracked(), []);
  cx.owner().dispose();
  assert_eq!(list.try_get(), None);
}

#[test]
fn disposing_the_root_frees_the_server_and_its_timer() {
  let runtime = Runtime::current();
  let before = runtime.node_count();
  let bus = PrivateBus::start();
  let config = Config {
    popup_timeout: Duration::from_millis(100),
    ..online(&bus)
  };
  let (mut shell, notifications, app) = serve(&bus, config);
  notify(&mut shell, &app, note("hello")).expect("an id");
  let server = service(&notifications);
  shell.cx.owner().dispose();
  assert_eq!(runtime.node_count(), before);
  // The timer finds the history gone and doesn't come back.
  shell.run_until(Instant::now() + Duration::from_millis(300));
  assert!(
    server
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.timer)
      .is_none()
  );
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_millis(500)),
    1
  );
}

#[test]
fn markup_in_a_body_shows_as_its_text() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let mut sent = note("markup");
  sent.body = "<b>Bold</b> &amp; &lt;not a tag&gt; &#169; &#x263A; a<br/>b &unknown; <".to_owned();
  notify(&mut shell, &app, sent).expect("an id");
  let list = notifications.list();
  wait_for(&mut shell, "the notification", || {
    !list.get_untracked().is_empty()
  });
  assert_eq!(
    list.get_untracked()[0].body,
    "Bold & <not a tag> \u{a9} \u{263a} ab &unknown; <"
  );
}

#[test]
fn body_markup_follows_the_setting_in_the_capabilities_and_the_body() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let capabilities = |shell: &mut Harness| -> Vec<String> {
    let reply = call(shell, &app, "GetCapabilities", ()).expect("the capabilities");
    reply.body().deserialize().expect("as")
  };
  let plain = ["actions", "body", "icon-static", "persistence"];
  assert_eq!(capabilities(&mut shell), plain, "off by default");

  notifications.set_body_markup(true);
  assert_eq!(
    capabilities(&mut shell),
    [
      "actions",
      "body",
      "body-hyperlinks",
      "body-markup",
      "icon-static",
      "persistence"
    ]
  );
  let markup = "<b>Bold</b> &amp; <a href=\"https://example.org\">a link</a>";
  let mut sent = note("markup");
  sent.body = markup.to_owned();
  let id = notify(&mut shell, &app, sent).expect("an id");
  let list = notifications.list();
  let body = |id: u32| {
    let list = list.get_untracked();
    list.iter().find(|n| n.id == id).map(|n| n.body.clone())
  };
  wait_for(&mut shell, "the notification", || body(id).is_some());
  assert_eq!(body(id).as_deref(), Some(markup), "kept as sent");
  // Past the 4 KiB cap, the tag the cut split goes with it.
  let long = "a".repeat(4090) + "<a href=\"https://example.org\">link</a>" + &"a".repeat(1000);
  let mut sent = note("long");
  sent.body = long;
  let id = notify(&mut shell, &app, sent).expect("an id");
  wait_for(&mut shell, "the long one", || body(id).is_some());
  assert_eq!(body(id), Some("a".repeat(4090)));

  notifications.set_body_markup(false);
  assert_eq!(capabilities(&mut shell), plain);
  let mut sent = note("plain again");
  sent.body = markup.to_owned();
  let id = notify(&mut shell, &app, sent).expect("an id");
  wait_for(&mut shell, "the notification", || body(id).is_some());
  assert_eq!(body(id).as_deref(), Some("Bold & a link"));
}

#[test]
fn a_markup_body_cut_at_its_cap_never_ends_in_half_a_tag_or_entity() {
  let filler = |n: usize| "a".repeat(n);
  // The cap falls inside the tag, then inside the entity.
  let tag = filler(4090) + "<a href=\"https://example.org\">link</a>" + &filler(1000);
  assert_eq!(cut_markup(&tag, 4096), filler(4090));
  let entity = filler(4094) + "&amp; more" + &filler(1000);
  assert_eq!(cut_markup(&entity, 4096), filler(4094));
  // An entity inside a tag goes with the tag.
  let both = filler(4085) + "<a href=\"x&amp;y\">z</a>" + &filler(1000);
  assert_eq!(cut_markup(&both, 4096), filler(4085));
  // Whole tags and entities stay, and so do a bare `&` and `<`, which no
  // cut split.
  let whole = filler(4080) + "<b>b</b>&amp;" + &filler(1000);
  assert_eq!(cut_markup(&whole, 4096), whole[..4096]);
  let bare = filler(4000) + " & < " + &filler(1000);
  assert_eq!(cut_markup(&bare, 4096), bare[..4096]);
  // A short body is kept whole.
  assert_eq!(cut_markup("<b>hi", 4096), "<b>hi");
}

#[test]
fn a_reload_keeps_the_notifications_and_their_ids_go_on() {
  let bus = PrivateBus::start();
  let (saved, before) = {
    let (mut shell, notifications, app) = serve(&bus, online(&bus));
    let mut pixels = note("pixels");
    pixels.app_icon = "mail-unread";
    pixels.actions = vec!["default", "Open"];
    pixels.hints = vec![
      ("image-data", picture(1, 1, 4, 4, vec![1, 2, 3, 4])),
      ("urgency", Variant::from(2u8)),
      ("category", Variant::from("email.arrived")),
    ];
    let mut transient = note("transient");
    transient.hints = vec![("transient", Variant::from(true))];
    transient.timeout = 600;
    let mut expired = note("expired");
    expired.timeout = 50;
    for note in [pixels, transient, expired] {
      notify(&mut shell, &app, note).expect("an id");
    }
    let popups = notifications.popups();
    wait_for(&mut shell, "the short one to expire", || {
      ids(popups) == [2, 1]
    });
    let saved = crate::save_state(&shell.cx).expect("something to keep");
    // The text it goes through between shells.
    let saved: serde_json::Value = serde_json::from_str(&saved.to_string()).expect("JSON");
    let before = notifications.list().get_untracked();
    shell.cx.owner().dispose();
    (saved, before)
  };

  let mut shell = Harness::new(online(&bus));
  crate::restore_state(&shell.cx, saved);
  let notifications = shell.cx.notifications();
  let (list, popups) = (notifications.list(), notifications.popups());
  let mut kept = before.clone();
  // Pixels aren't kept, so the app's icon shows.
  kept[0].image = Some(IconSource::Name("mail-unread".into()));
  assert_eq!(list.get_untracked(), kept, "shown before the bus answers");
  assert_eq!(ids(popups), [2, 1]);
  let server = service(&notifications);
  wait_for(&mut shell, "the server", || server.serving());
  wait_for(&mut shell, "the transient one to expire", || {
    ids(popups) == [1]
  });
  assert_eq!(ids(list), [1, 3], "a transient one goes");
  let app = shell.block_on(bus.connect(&[]));
  assert_eq!(notify(&mut shell, &app, note("next")).expect("an id"), 4);
  assert!(call(&mut shell, &app, "CloseNotification", 3_u32).is_ok());
  assert_eq!(ids(list), [1, 4], "the app's own ids still work");
}

#[test]
fn a_saved_state_that_doesnt_fit_is_logged_and_starts_empty() {
  let mut shell = Harness::new(offline());
  let logs = capture_logs(|| {
    crate::restore_state(&shell.cx, serde_json::json!({ "notifications": [1, 2] }))
  });
  assert!(logs.contains("the notifications kept"), "{logs}");
  let notifications = shell.cx.notifications();
  assert_eq!(notifications.list().get_untracked(), []);
  shell.run_until(Instant::now() + Duration::from_millis(50));
  assert_eq!(
    crate::save_state(&shell.cx),
    Some(serde_json::json!({
      "notifications": { "next_id": 1, "history": [], "showing": [] }
    }))
  );
}

/// A saved notification as the shell before a reload kept it, showing
/// with `left` ms to go.
fn kept(id: u32, summary: String, transient: bool) -> serde_json::Value {
  serde_json::json!({
    "id": id, "app_name": "test app", "app_icon": "", "summary": summary,
    "body": "the body", "actions": [], "urgency": "Normal", "timeout": "Default",
    "image": null, "desktop_entry": "", "category": "", "resident": false,
    "transient": transient,
    "received": { "secs_since_epoch": 1_700_000_000_u64, "nanos_since_epoch": 0 },
  })
}

#[test]
fn restored_notifications_keep_to_the_caps_of_ones_that_arrive() {
  let mut shell = Harness::new(offline());
  let mut big = kept(1, "s".repeat(2000), false);
  let long = "x".repeat(300);
  big["app_name"] = long.clone().into();
  big["app_icon"] = long.clone().into();
  big["body"] = "b".repeat(5000).into();
  big["desktop_entry"] = long.clone().into();
  big["category"] = long.clone().into();
  big["actions"] = (0..20)
    .map(|n| serde_json::json!([format!("{n}{long}"), long]))
    .collect();
  let saved = serde_json::json!({ "next_id": 2, "history": [big], "showing": [] });
  crate::restore_state(&shell.cx, serde_json::json!({ "notifications": saved }));
  let notifications = shell.cx.notifications();
  shell.run_until(Instant::now() + Duration::from_millis(20));
  let restored = notifications.list().get_untracked()[0].clone();
  assert_eq!(restored.summary.len(), 1024);
  assert_eq!(restored.body.len(), 4096);
  for text in [
    &restored.app_name,
    &restored.app_icon,
    &restored.desktop_entry,
    &restored.category,
  ] {
    assert_eq!(text.len(), 256);
  }
  assert_eq!(restored.actions.len(), 16);
  assert!(
    restored
      .actions
      .iter()
      .all(|(key, label)| key.len() == 256 && label.len() == 256)
  );
}

/// A popup whose time ran out during the swap goes once the server can
/// tell its app, not before.
#[test]
fn a_restored_popup_that_expired_in_the_swap_tells_its_app() {
  let bus = PrivateBus::start();
  let mut shell = Harness::new(online(&bus));
  let heard = listen(&mut shell, &bus);
  let saved = serde_json::json!({
    "next_id": 3,
    "history": [kept(1, "gone".into(), true), kept(2, "stays".into(), false)],
    "showing": [[1, 0], [2, null]],
  });
  crate::restore_state(&shell.cx, serde_json::json!({ "notifications": saved }));
  let notifications = shell.cx.notifications();
  let list = notifications.list();
  wait_for(&mut shell, "the expired one to go", || ids(list) == [2]);
  wait_for(&mut shell, "its signal", || !heard.borrow().is_empty());
  assert_eq!(*heard.borrow(), ["NotificationClosed 1 1"]);
}

/// The list's ids, oldest first.
fn list_ids(notifications: &Notifications) -> Vec<u32> {
  ids(notifications.list())
}

/// Two shells on one bus, as in a reload: the old one saves and runs on,
/// the new one starts from that and takes the name over, while an app
/// on its own thread sends a notification every few milliseconds. Each
/// one sent reaches the new shell's list, with the id the app was given.
#[test]
fn notifications_sent_through_a_reload_all_reach_the_new_shell() {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};

  let bus = PrivateBus::start();
  let (mut old, old_notes, _) = serve(&bus, online(&bus));
  let stop = Arc::new(AtomicBool::new(false));
  let app = std::thread::spawn({
    let (stop, address) = (Arc::clone(&stop), bus.address.clone());
    move || {
      let mut shell = Harness::new(offline());
      let builder = zbus::connection::Builder::address(address.as_str());
      let app = shell
        .block_on(async move { builder.build().await })
        .expect("the app's connection");
      let mut sent = Vec::new();
      while !stop.load(Ordering::Relaxed) {
        let summary = format!("note {}", sent.len());
        sent.push(notify(&mut shell, &app, note(&summary)).expect("every call is answered"));
        std::thread::sleep(Duration::from_millis(3));
      }
      sent
    }
  });
  wait_for(&mut old, "the first notifications", || {
    list_ids(&old_notes).len() >= 5
  });
  let saved = crate::save_state(&old.cx).expect("something to keep");
  let saved: serde_json::Value = serde_json::from_str(&saved.to_string()).expect("JSON");
  let at_save = list_ids(&old_notes).len();
  // The app goes on calling the old shell while the new one starts.
  old.run_until(Instant::now() + Duration::from_millis(50));

  let mut new = Harness::new(online(&bus));
  crate::restore_state(&new.cx, saved);
  let mut logs = String::new();
  let new_notes = new.cx.notifications();
  let give_up = Instant::now() + Duration::from_secs(5);
  // Both loops run, as two shells' do.
  let both = |old: &mut Harness, new: &mut Harness| {
    old.run_until(Instant::now() + Duration::from_millis(3));
    new.run_until(Instant::now() + Duration::from_millis(3));
  };
  logs += &capture_logs(|| {
    while crate::taking_over(&new.cx) {
      assert!(Instant::now() < give_up, "no takeover after 5 s");
      both(&mut old, &mut new);
    }
    for _ in 0..10 {
      both(&mut old, &mut new);
    }
  });
  assert!(
    list_ids(&old_notes).len() > at_save,
    "the old shell got some after its save"
  );
  assert!(
    logs.contains("handing the notifications over to the new shell")
      && logs.contains("took the notifications over from the old shell"),
    "{logs}"
  );
  // The old shell exits.
  old.cx.owner().dispose();
  drop(old);
  new.run_until(Instant::now() + Duration::from_millis(100));
  stop.store(true, Ordering::Relaxed);
  while !app.is_finished() {
    new.run_until(Instant::now() + Duration::from_millis(5));
  }
  let sent = app.join().expect("the app ran");
  assert!(sent.len() < MAX_KEPT, "{} sent", sent.len());
  assert_eq!(list_ids(&new_notes), sent);
}

#[test]
fn only_the_shell_that_took_the_name_can_take_the_notifications() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  notify(&mut shell, &app, note("kept")).expect("an id");
  let unique = service(&notifications)
    .state
    .borrow()
    .as_ref()
    .and_then(|state| {
      let connection = state.connection.as_ref()?;
      Some(connection.unique_name()?.to_string())
    });
  let unique = unique.expect("the shell's name on the bus");
  let take = |shell: &mut Harness, app: &Connection| {
    let (app, unique) = (app.clone(), unique.clone());
    shell.block_on(async move {
      let call = app.call_method(
        Some(unique.as_str()),
        NOTIFICATIONS_PATH,
        Some(super::HANDOFF),
        "Take",
        &(),
      );
      call.await.map(|_| ()).map_err(|error| error.to_string())
    })
  };
  let refused = take(&mut shell, &app).expect_err("not saved for a reload");
  assert!(refused.contains("nothing to hand over"), "{refused}");
  crate::save_state(&shell.cx).expect("something to keep");
  // Saved, but this app doesn't own the name.
  let refused = take(&mut shell, &app).expect_err("not the name's owner");
  assert!(refused.contains("nothing to hand over"), "{refused}");
  let taker = shell.block_on(bus.connect(&[]));
  let replaced = taker.clone();
  shell
    .block_on(async move {
      let flags = zbus::fdo::RequestNameFlags::REPLACE_EXISTING;
      replaced.request_name_with_flags(NOTIFICATIONS, flags).await
    })
    .expect("the name");
  wait_for(&mut shell, "the shell to step back", || {
    !service(&notifications).serving()
  });
  assert_eq!(take(&mut shell, &taker), Ok(()), "the new owner takes them");
  let refused = take(&mut shell, &taker).expect_err("only once");
  assert!(refused.contains("nothing to hand over"), "{refused}");
}

/// A shell that got the name back, as when the new shell of a reload
/// failed, hands nothing over to whoever takes it next.
#[test]
fn a_shell_that_got_the_name_back_hands_nothing_over() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, _) = serve(&bus, online(&bus));
  let server = service(&notifications);
  let unique = server.state.borrow().as_ref().and_then(|state| {
    let connection = state.connection.as_ref()?;
    Some(connection.unique_name()?.to_string())
  });
  let unique = unique.expect("the shell's name on the bus");
  crate::save_state(&shell.cx).expect("something to keep");
  let take_name = |shell: &mut Harness| {
    let taker = shell.block_on(bus.connect(&[]));
    let asked = taker.clone();
    shell
      .block_on(async move {
        let flags = zbus::fdo::RequestNameFlags::REPLACE_EXISTING;
        asked.request_name_with_flags(NOTIFICATIONS, flags).await
      })
      .expect("the name");
    taker
  };
  let failed = take_name(&mut shell);
  wait_for(&mut shell, "the shell to step back", || !server.serving());
  shell.block_on(failed.close()).expect("the new shell fails");
  wait_for(&mut shell, "the name back", || server.serving());
  let next = take_name(&mut shell);
  wait_for(&mut shell, "the shell to step back", || !server.serving());
  let refused = shell.block_on(async move {
    let call = next.call_method(
      Some(unique.as_str()),
      NOTIFICATIONS_PATH,
      Some(super::HANDOFF),
      "Take",
      &(),
    );
    call.await.map(|_| ()).map_err(|error| error.to_string())
  });
  let refused = refused.expect_err("disarmed when the name came back");
  assert!(refused.contains("nothing to hand over"), "{refused}");
}

/// More calls than zbus queues, 64, while the old shell is slow to hand
/// over: they are read off meanwhile, so its answer still gets through,
/// and then each is answered.
#[test]
fn a_burst_while_taking_over_is_held_and_then_answered() {
  let bus = PrivateBus::start();
  let (mut old, old_notes, app) = serve(&bus, online(&bus));
  notify(&mut old, &app, note("before")).expect("an id");
  let saved = crate::save_state(&old.cx).expect("something to keep");
  let mut new = Harness::new(online(&bus));
  crate::restore_state(&new.cx, saved);
  let new_notes = new.cx.notifications();
  let server = service(&new_notes);
  // Only the new shell runs, so the old one doesn't answer yet.
  wait_for(&mut new, "the name", || server.serving());
  let address = bus.address.clone();
  let burst = std::thread::spawn(move || {
    let mut shell = Harness::new(offline());
    let builder = zbus::connection::Builder::address(address.as_str());
    let app = shell
      .block_on(async move { builder.build().await })
      .expect("the app's connection");
    let answered = Rc::new(Cell::new(0));
    for n in 0..100 {
      let (app, answered) = (app.clone(), Rc::clone(&answered));
      shell.spawn(async move {
        let hints: HashMap<&str, Variant<'_>> = HashMap::new();
        let body = (
          "burst",
          0_u32,
          "",
          format!("n{n}"),
          "",
          Vec::<&str>::new(),
          hints,
          -1,
        );
        let call = app.call_method(
          Some(NOTIFICATIONS),
          NOTIFICATIONS_PATH,
          Some(NOTIFICATIONS),
          "Notify",
          &body,
        );
        if call.await.is_ok() {
          answered.set(answered.get() + 1);
        }
      });
    }
    let give_up = Instant::now() + Duration::from_secs(10);
    while answered.get() < 100 && Instant::now() < give_up {
      shell.run_until(Instant::now() + Duration::from_millis(10));
    }
    answered.get()
  });
  new.run_until(Instant::now() + Duration::from_millis(300));
  let logs = capture_logs(|| {
    while crate::taking_over(&new.cx) || !burst.is_finished() {
      old.run_until(Instant::now() + Duration::from_millis(3));
      new.run_until(Instant::now() + Duration::from_millis(3));
    }
  });
  assert_eq!(burst.join().expect("the burst ran"), 100);
  assert!(
    logs.contains("took the notifications over from the old shell"),
    "{logs}"
  );
  assert_eq!(list_ids(&new_notes).len(), 101);
  drop(old_notes);
}

/// The new shell of a reload took the notifications and is replaced; the
/// old one saves again while it still waits for the name back. It names
/// itself as the owner, and the name coming back doesn't disarm that save,
/// so the next shell takes the notifications from it.
#[test]
fn a_save_after_handing_over_still_names_the_shell_for_the_next() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, _) = serve(&bus, online(&bus));
  let server = service(&notifications);
  let unique = server.state.borrow().as_ref().and_then(|state| {
    let connection = state.connection.as_ref()?;
    Some(connection.unique_name()?.to_string())
  });
  let unique = unique.expect("the shell's name on the bus");
  crate::save_state(&shell.cx).expect("something to keep");
  let take_name = |shell: &mut Harness| {
    let taker = shell.block_on(bus.connect(&[]));
    let asked = taker.clone();
    shell
      .block_on(async move {
        let flags = zbus::fdo::RequestNameFlags::REPLACE_EXISTING;
        asked.request_name_with_flags(NOTIFICATIONS, flags).await
      })
      .expect("the name");
    taker
  };
  let take = |shell: &mut Harness, taker: &Connection| {
    let (taker, unique) = (taker.clone(), unique.clone());
    shell.block_on(async move {
      let call = taker.call_method(
        Some(unique.as_str()),
        NOTIFICATIONS_PATH,
        Some(super::HANDOFF),
        "Take",
        &(),
      );
      call.await.map(|_| ()).map_err(|error| error.to_string())
    })
  };
  let replaced = take_name(&mut shell);
  wait_for(&mut shell, "the shell to step back", || !server.serving());
  assert_eq!(take(&mut shell, &replaced), Ok(()));
  let saved = crate::save_state(&shell.cx).expect("something to keep");
  assert_eq!(
    saved["notifications"]["owner"].as_str(),
    Some(unique.as_str()),
    "still the owner, for the next shell"
  );
  shell
    .block_on(replaced.close())
    .expect("the new shell goes");
  wait_for(&mut shell, "the name back", || server.serving());
  let next = take_name(&mut shell);
  wait_for(&mut shell, "the shell to step back", || !server.serving());
  assert_eq!(
    take(&mut shell, &next),
    Ok(()),
    "the second save stays armed"
  );
}

fn timed(summary: &str, ms: u64) -> Notification {
  Notification {
    timeout: Timeout::After(Duration::from_millis(ms)),
    ..sample(summary)
  }
}

fn timer_armed(server: &Service) -> bool {
  let state = server.state.borrow();
  state.as_ref().is_some_and(|state| state.timer.is_some())
}

#[test]
fn the_default_timeout_applies_to_popups_that_arrive_after_it() {
  let mut shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let before = server.notify(0, sample("before"));
  let whole = |id| notifications.popup_time_left(id).map(|(_, whole)| whole);
  assert_eq!(whole(before), Some(Duration::from_secs(5)), "5 s at first");
  notifications.set_default_timeout(Duration::from_millis(100));
  let after = server.notify(0, sample("after"));
  let own = server.notify(0, timed("its own", 3000));
  assert_eq!(whole(after), Some(Duration::from_millis(100)));
  assert_eq!(
    whole(own),
    Some(Duration::from_secs(3)),
    "an app's own stays"
  );
  let popups = notifications.popups();
  wait_for(&mut shell, "the short one to expire", || {
    ids(popups) == [own, before]
  });
  assert_eq!(
    whole(before),
    Some(Duration::from_secs(5)),
    "one showing keeps its"
  );
  // A replaced one counts the new default.
  let mut replaced = sample("replaced");
  replaced.urgency = Urgency::Low;
  server.notify(before, replaced);
  assert_eq!(whole(before), Some(Duration::from_millis(100)));
  // Past what an `Instant` holds, it never expires.
  notifications.set_default_timeout(Duration::MAX);
  let forever = server.notify(0, sample("forever"));
  assert_eq!(notifications.popup_time_left(forever), None);
  let mut critical = sample("critical");
  critical.urgency = Urgency::Critical;
  let critical = server.notify(0, critical);
  assert_eq!(notifications.popup_time_left(critical), None);
  assert_eq!(notifications.popup_time_left(999), None, "an unknown id");
  notifications.set_default_timeout(Duration::ZERO);
  let at_once = server.notify(0, sample("at once"));
  assert_eq!(
    notifications.popup_time_left(at_once),
    None,
    "no zero whole"
  );
}

#[test]
fn a_held_popup_stops_counting_and_resumes_with_the_time_it_had_left() {
  let mut shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let id = server.notify(0, timed("held", 300));
  let other = server.notify(0, timed("other", 5000));
  shell.run_until(Instant::now() + Duration::from_millis(100));
  notifications.hold_popup(id, true);
  notifications.hold_popup(id, true);
  let (left, whole) = notifications.popup_time_left(id).expect("counting");
  assert_eq!(whole, Duration::from_millis(300));
  assert!(left <= Duration::from_millis(200), "{left:?}");
  shell.run_until(Instant::now() + Duration::from_millis(400));
  assert_eq!(ids(notifications.popups()), [other, id], "still showing");
  assert_eq!(
    notifications.popup_time_left(id),
    Some((left, whole)),
    "frozen"
  );
  notifications.hold_popup(999, true);

  notifications.hold_popup(id, false);
  let started = Instant::now();
  let popups = notifications.popups();
  wait_for(&mut shell, "the held one to expire", || {
    ids(popups) == [other]
  });
  let counted = started.elapsed();
  assert!(
    counted + Duration::from_millis(20) >= left,
    "{counted:?} of {left:?}"
  );
  assert_eq!(ids(notifications.list()), [id, other], "it stays kept");
}

#[test]
fn hiding_a_popup_ends_it_as_expiry_does() {
  let bus = PrivateBus::start();
  let (mut shell, notifications, app) = serve(&bus, online(&bus));
  let heard = listen(&mut shell, &bus);
  let closed = closes(&notifications);
  let kept = notify(&mut shell, &app, note("kept")).expect("an id");
  let mut transient = note("transient");
  transient.hints = vec![("transient", Variant::from(true))];
  let transient = notify(&mut shell, &app, transient).expect("an id");
  notifications.hide_popup(kept);
  notifications.hide_popup(999);
  assert_eq!(ids(notifications.popups()), [transient]);
  assert_eq!(ids(notifications.list()), [kept, transient]);
  notifications.hide_popup(transient);
  assert_eq!(ids(notifications.popups()), [0u32; 0]);
  assert_eq!(ids(notifications.list()), [kept], "a transient one goes");
  assert_eq!(
    *closed.borrow(),
    [self::heard("transient", CloseReason::Expired)]
  );
  wait_for(&mut shell, "its signal", || !heard.borrow().is_empty());
  shell.run_until(Instant::now() + Duration::from_millis(100));
  assert_eq!(
    *heard.borrow(),
    [format!("NotificationClosed {transient} 1")]
  );
  assert!(
    !timer_armed(&service(&notifications)),
    "nothing left to expire"
  );
}

#[test]
fn a_handler_hears_each_arrival_and_can_hide_it_before_it_pops_up() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let popups = notifications.popups();
  let ever_shown: Rc<RefCell<Vec<u32>>> = Rc::default();
  let sink = Rc::clone(&ever_shown);
  shell
    .cx
    .effect(move || sink.borrow_mut().extend(popups.get().iter().map(|n| n.id)));
  let heard: Rc<RefCell<Vec<(u32, String)>>> = Rc::default();
  let sink = Rc::clone(&heard);
  let inner = notifications.clone();
  notifications.on_notification(move |arrived| {
    assert!(!ids(inner.popups()).contains(&arrived.id), "not shown yet");
    sink
      .borrow_mut()
      .push((arrived.id, arrived.summary.clone()));
    // Nothing of the service is borrowed while a handler runs.
    if arrived.urgency != Urgency::Critical {
      inner.hide_popup(arrived.id);
    }
  });
  let quiet = server.notify(0, sample("quiet"));
  let mut critical = sample("critical");
  critical.urgency = Urgency::Critical;
  let critical = server.notify(0, critical);
  Runtime::current().flush();
  assert_eq!(ids(popups), [critical]);
  assert_eq!(
    ids(notifications.list()),
    [quiet, critical],
    "kept in the history"
  );
  assert!(
    !ever_shown.borrow().contains(&quiet),
    "{:?}",
    ever_shown.borrow()
  );
  let mut replaced = sample("replaced");
  replaced.urgency = Urgency::Critical;
  server.notify(quiet, replaced);
  assert_eq!(
    *heard.borrow(),
    [
      (quiet, "quiet".to_owned()),
      (critical, "critical".to_owned()),
      (quiet, "replaced".to_owned())
    ]
  );
  assert_eq!(
    ids(popups),
    [quiet, critical],
    "a replaced one pops up again"
  );
}

#[test]
fn a_panicking_arrival_handler_is_logged_and_taken_away_and_the_rest_run_on() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let panics = Rc::new(Cell::new(0));
  let counted = Rc::clone(&panics);
  notifications.on_notification(move |arrived| {
    counted.set(counted.get() + 1);
    panic!("bad arrival of {}", arrived.summary);
  });
  let heard = Rc::new(Cell::new(0));
  let sink = Rc::clone(&heard);
  notifications.on_notification(move |_| sink.set(sink.get() + 1));
  let mut id = 0;
  let logs = capture_logs(|| id = server.notify(0, sample("first")));
  let at = "notifications on_notification (called at crates/crabshell-services/src/notifications/tests.rs:";
  assert!(logs.contains(at), "{logs}");
  assert!(logs.contains("panicked: bad arrival of first"), "{logs}");
  assert_eq!(ids(notifications.popups()), [id], "it still shows");
  server.notify(0, sample("second"));
  assert_eq!(
    panics.get(),
    1,
    "a handler that panicked isn't called again"
  );
  assert_eq!(heard.get(), 2);
}

#[test]
fn an_arrival_handler_goes_with_its_context() {
  let shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let nodes = Runtime::current().node_count();
  let child = shell.cx.child();
  let heard = Rc::new(Cell::new(0));
  let sink = Rc::clone(&heard);
  child
    .notifications()
    .on_notification(move |_| sink.set(sink.get() + 1));
  server.notify(0, sample("heard"));
  child.owner().dispose();
  assert_eq!(Runtime::current().node_count(), nodes);
  assert_eq!(Rc::strong_count(&heard), 1, "the handler was dropped");
  server.notify(0, sample("unheard"));
  assert_eq!(heard.get(), 1);
}

#[test]
fn a_replaced_popup_counts_its_whole_timeout_again() {
  let mut shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let id = server.notify(0, timed("first", 300));
  shell.run_until(Instant::now() + Duration::from_millis(200));
  let (left, _) = notifications.popup_time_left(id).expect("counting");
  assert!(left <= Duration::from_millis(100), "{left:?}");
  server.notify(id, timed("again", 300));
  let (left, _) = notifications.popup_time_left(id).expect("counting");
  assert!(left > Duration::from_millis(250), "{left:?}");
  shell.run_until(Instant::now() + Duration::from_millis(150));
  assert_eq!(
    ids(notifications.popups()),
    [id],
    "past the first one's time"
  );
  // A held one stays held, counting from its whole timeout.
  notifications.hold_popup(id, true);
  server.notify(id, timed("held", 300));
  shell.run_until(Instant::now() + Duration::from_millis(400));
  assert_eq!(
    notifications.popup_time_left(id),
    Some((Duration::from_millis(300), Duration::from_millis(300)))
  );
}

#[test]
fn no_timer_runs_while_every_popup_is_held_or_never_expires() {
  let mut shell = Harness::new(offline());
  let notifications = shell.cx.notifications();
  let server = service(&notifications);
  let mut never = sample("never");
  never.timeout = Timeout::Never;
  server.notify(0, never);
  let held = server.notify(0, timed("held", 100));
  assert!(timer_armed(&server));
  notifications.hold_popup(held, true);
  assert!(!timer_armed(&server), "nothing counts down");
  shell.run_until(Instant::now() + Duration::from_millis(50));
  assert_eq!(
    shell.run_until(Instant::now() + Duration::from_millis(500)),
    1
  );
  notifications.hold_popup(held, false);
  assert!(timer_armed(&server));
}

/// A held popup is saved with the time it had left, and the next shell
/// counts that down, since the window that held it is gone.
#[test]
fn a_reload_keeps_a_held_popup_with_its_time_left_counting() {
  let (saved, id) = {
    let mut shell = Harness::new(offline());
    let notifications = shell.cx.notifications();
    let id = service(&notifications).notify(0, timed("held", 400));
    shell.run_until(Instant::now() + Duration::from_millis(100));
    notifications.hold_popup(id, true);
    shell.run_until(Instant::now() + Duration::from_millis(300));
    let saved = crate::save_state(&shell.cx).expect("something to keep");
    let saved: serde_json::Value = serde_json::from_str(&saved.to_string()).expect("JSON");
    shell.cx.owner().dispose();
    (saved, id)
  };
  let mut shell = Harness::new(offline());
  crate::restore_state(&shell.cx, saved);
  let notifications = shell.cx.notifications();
  let (left, whole) = notifications.popup_time_left(id).expect("counting");
  assert_eq!(whole, Duration::from_millis(400), "its whole timeout kept");
  assert!(
    left > Duration::from_millis(200) && left <= Duration::from_millis(300),
    "{left:?}"
  );
  let popups = notifications.popups();
  wait_for(&mut shell, "it to expire", || ids(popups).is_empty());

  // One saved by a shell before these fields counts its time left as whole.
  let mut shell = Harness::new(offline());
  let saved = serde_json::json!({
    "next_id": 2, "history": [kept(1, "old".into(), false)], "showing": [[1, 900]],
  });
  crate::restore_state(&shell.cx, serde_json::json!({ "notifications": saved }));
  let notifications = shell.cx.notifications();
  let (_, whole) = notifications.popup_time_left(1).expect("counting");
  assert_eq!(whole, Duration::from_millis(900));
  shell.run_until(Instant::now() + Duration::from_millis(10));
}
