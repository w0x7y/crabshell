//! A notification server, as the Desktop Notifications spec 1.2 has it:
//! apps call `org.freedesktop.Notifications` on the session bus, and the
//! shell keeps what they send.
//!
//! The shell queues for the name, so it serves while no other daemon,
//! such as mako or dunst, does, and takes over when that one leaves. On a
//! reload the new shell takes the name from the old one and asks it, by
//! `Take` on [`HANDOFF`], for what it kept, so nothing sent during the
//! swap is lost.
//! zbus has no object server here, so the calls are answered by hand on
//! the main thread, in [`server`]. One stream reads every message the bus
//! sends this connection, and one loop timer runs only while a popup
//! counts down to expire, so a quiet bus leaves the shell asleep.

mod server;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::panic::{self, AssertUnwindSafe, Location};
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::sync::Once;
use std::task::Poll;
use std::time::{Duration, Instant, SystemTime};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{Owner, ReadSignal, Signal, untrack};
use crabshell_ui::{Cx, IconSource};
use serde::{Deserialize, Serialize};
use zbus::export::futures_core::Stream;
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Type as Kind;
use zbus::{Connection, Message, MessageStream};

use crate::Shared;
use crate::dbus::set_changed;
use crate::dbus::{BUS, BUS_PATH};
use crate::lines::panic_message;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
/// The interface the shell before a reload hands its notifications over
/// on.
const HANDOFF: &str = "dev.crabshell.Handoff";

/// How long the new shell waits for the old one's notifications.
const HANDOFF_LIMIT: Duration = Duration::from_secs(2);

/// The most calls held while the old shell hands its notifications over;
/// past it they wait in zbus's queue, and the takeover may time out.
const MAX_HELD: usize = 256;

/// The most bytes of notifications the old shell may hand over: past the
/// history's own caps, so a full history fits.
const MAX_HANDOFF: usize = 16 * 1024 * 1024;

/// How long a popup shows when its app leaves that to the server, until
/// the config sets its own.
pub(crate) const POPUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The most notifications kept. Past it the oldest goes, as if it
/// expired.
const MAX_KEPT: usize = 256;

/// The most pixel bytes the history keeps: 128 pictures of 128 px.
const MAX_PIXELS_KEPT: usize = 8 * 1024 * 1024;

/// Why a notification closed, from [`Notifications::on_closed`]. Its app
/// hears the same in `NotificationClosed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
  /// It went by itself: a transient one's popup timed out, or it was the
  /// oldest when the history was full.
  Expired,
  /// The user dismissed it, cleared them all, or picked one of its
  /// actions.
  Dismissed,
  /// Its app took it back.
  CloseRequested,
}

impl CloseReason {
  /// Its number in `NotificationClosed`.
  fn code(self) -> u32 {
    match self {
      CloseReason::Expired => 1,
      CloseReason::Dismissed => 2,
      CloseReason::CloseRequested => 3,
    }
  }
}

/// How urgent a notification is, from its `urgency` hint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Urgency {
  /// It can wait, such as a finished download.
  Low,
  /// The usual.
  #[default]
  Normal,
  /// It needs the user now, such as a battery about to run out. Its
  /// popup stays until dismissed.
  Critical,
}

/// How long a notification's popup shows, as its app asks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Timeout {
  /// As long as the server likes: 5 s, or what
  /// [`Notifications::set_default_timeout`] set.
  #[default]
  Default,
  /// Until dismissed.
  Never,
  /// This long.
  After(Duration),
}

/// One notification an app sent.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
  /// Never 0, and unique among those kept. The key to give
  /// `children_keyed`, and what [`Notifications::dismiss`] and
  /// [`Notifications::invoke`] take.
  pub id: u32,
  /// The app's name for itself, such as `Firefox`, or empty.
  pub app_name: String,
  /// The app's icon as it sent it: an icon name, a `file://` URI, or
  /// empty.
  pub app_icon: String,
  /// One line about it, at most 1 KiB.
  pub summary: String,
  /// The text under that, at most 4 KiB, as plain text: tags an app sent
  /// are taken out and entities decoded. While
  /// [`set_body_markup`](Notifications::set_body_markup) is on, it is the
  /// markup as sent, for `parse_markup`.
  pub body: String,
  /// The actions the user can pick, as (key, label), at most 16. The
  /// app means `default` for a click on the notification itself.
  pub actions: Vec<(String, String)>,
  /// How urgent it is.
  pub urgency: Urgency,
  /// How long its popup shows.
  pub timeout: Timeout,
  /// The picture to show: the app's pixels, an image file, or else its
  /// icon. `None` when it has none.
  pub image: Option<IconSource>,
  /// The app's desktop entry, such as `org.mozilla.firefox`, or empty.
  pub desktop_entry: String,
  /// What kind it is, such as `email.arrived`, or empty.
  pub category: String,
  /// Whether it stays after one of its actions is picked.
  pub resident: bool,
  /// Whether it goes once its popup expires, instead of staying in the
  /// history.
  pub transient: bool,
  /// When it arrived, or was last replaced.
  pub received: SystemTime,
}

/// The notifications, from `cx.notifications()`. A clone is cheap and
/// shares the one server.
#[derive(Clone)]
pub struct Notifications {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  list: ReadSignal<Vec<Notification>>,
  popups: ReadSignal<Vec<Notification>>,
  /// The asking context's owner, which [`on_closed`](Self::on_closed)'s
  /// handlers live under.
  owner: Owner,
}

impl Notifications {
  /// Every notification kept, oldest first: the history, from arrival
  /// until it is dismissed, picked or closed by its app. A replaced one
  /// keeps its place.
  pub fn list(&self) -> ReadSignal<Vec<Notification>> {
    self.list
  }

  /// The notifications still showing as popups, newest first. Each stays
  /// for its timeout, and until dismissed when that is
  /// [`Timeout::Never`] or it is [`Urgency::Critical`]. A replaced one
  /// counts its whole timeout again. An expired one stays in
  /// [`list`](Self::list), unless it is transient.
  pub fn popups(&self) -> ReadSignal<Vec<Notification>> {
    self.popups
  }

  /// Sets what [`Timeout::Default`] means for the popups that arrive or
  /// are replaced from now on; those showing keep the countdown they
  /// have. It is 5 s until set. A timeout too long for the clock to hold
  /// shows until dismissed.
  pub fn set_default_timeout(&self, timeout: Duration) {
    if let Some(service) = &self.service {
      service.default_timeout.set(timeout);
    }
  }

  /// While `held`, popup `id`'s countdown stops, as for the pointer over
  /// it; `false` resumes it with the time it had left. A popup replaced
  /// while held stays held, with its whole timeout left. A reload resumes
  /// every held popup, since the window that held it is gone. An unknown
  /// `id` is ignored.
  pub fn hold_popup(&self, id: u32, held: bool) {
    if let Some(service) = &self.service {
      service.hold(id, held);
    }
  }

  /// Ends popup `id` now, as if its time ran out: it leaves
  /// [`popups`](Self::popups) and stays in [`list`](Self::list), and its
  /// app isn't told. A transient one goes from the list too, and its app
  /// and [`on_closed`](Self::on_closed) hear [`CloseReason::Expired`]. An
  /// unknown `id` is ignored.
  pub fn hide_popup(&self, id: u32) {
    if let Some(service) = &self.service {
      service.hide(&[id]);
    }
  }

  /// Popup `id`'s time left and its whole timeout, to draw a countdown
  /// bar of `left / whole`. The time left stops while
  /// [`hold_popup`](Self::hold_popup) holds it. `None` for a popup that
  /// shows until dismissed or expires at once, or one not showing. It
  /// isn't a signal, so read it in a binding that `cx.frames` ticks while
  /// the popup counts.
  ///
  /// ```no_run
  /// use crabshell_reactive::{Memo, Signal};
  /// use crabshell_services::ServicesExt;
  /// use crabshell_ui::Cx;
  ///
  /// /// How full popup `id`'s countdown bar is, from 1 down to 0. It stops
  /// /// while `hovered`, which the toast's button writes, and nothing
  /// /// ticks then, or for a popup that never expires.
  /// fn countdown(cx: &Cx, id: u32, hovered: Signal<bool>) -> Memo<f32> {
  ///   let notifications = cx.notifications();
  ///   let held = notifications.clone();
  ///   cx.effect(move || held.hold_popup(id, hovered.get()));
  ///   let (counting, popups) = (notifications.clone(), notifications.popups());
  ///   // Read again when a replacement gives the popup a new timeout.
  ///   let time = cx.frames(move || {
  ///     !hovered.get() && popups.with(|_| counting.popup_time_left(id).is_some())
  ///   });
  ///   cx.memo(move || {
  ///     time.get();
  ///     let left = notifications.popup_time_left(id);
  ///     left.map_or(1.0, |(left, whole)| left.as_secs_f32() / whole.as_secs_f32())
  ///   })
  /// }
  /// ```
  pub fn popup_time_left(&self, id: u32) -> Option<(Duration, Duration)> {
    self.service.as_ref()?.time_left(id)
  }

  /// Takes notification `id` away, as the user's close button does, and
  /// tells its app. An unknown `id` is ignored.
  pub fn dismiss(&self, id: u32) {
    if let Some(service) = &self.service {
      service.close(&[id], CloseReason::Dismissed);
    }
  }

  /// Tells notification `id`'s app that the user picked its action
  /// `key`, then takes it away unless it is resident. An unknown `id` or
  /// `key` is logged.
  pub fn invoke(&self, id: u32, key: &str) {
    if let Some(service) = &self.service {
      service.invoke(id, key);
    }
  }

  /// When on, the server tells apps it shows `body-markup` and
  /// `body-hyperlinks`, so they send the spec's markup, and keeps each body
  /// as sent, for `Text::rich(parse_markup(&body))`. Off by default, when
  /// tags are taken out. Notifications already kept stay as they came.
  pub fn set_body_markup(&self, on: bool) {
    if let Some(service) = &self.service {
      service.body_markup.set(on);
    }
  }

  /// Dismisses every notification.
  pub fn clear(&self) {
    if let Some(service) = &self.service {
      let state = service.state.borrow();
      let history = state.as_ref().map(|state| state.history.as_slice());
      let ids: Vec<u32> = history.unwrap_or_default().iter().map(|n| n.id).collect();
      drop(state);
      service.close(&ids, CloseReason::Dismissed);
    }
  }

  /// Calls `handler` with each notification that leaves the history and
  /// why, as Quickshell's `Notification.closed`, for as long as the
  /// context `cx.notifications()` was called on lives. It runs on the
  /// main thread once the lists no longer show it. A popup that times out
  /// stays in the history, so it hasn't closed, unless it is transient.
  ///
  /// If it panics, that is logged with where `on_closed` was called, and
  /// it isn't called again. Called on a handle whose context went, it
  /// logs that and does nothing.
  #[track_caller]
  pub fn on_closed(&self, handler: impl Fn(&Notification, CloseReason) + 'static) {
    self.listen(Handler::Closed(Rc::new(handler)), Location::caller());
  }

  /// Calls `handler` with each notification as it arrives or is replaced,
  /// as Quickshell's `onNotification`, for as long as the context
  /// `cx.notifications()` was called on lives. It runs on the main thread
  /// before [`popups`](Self::popups) shows it, so a
  /// [`hide_popup`](Self::hide_popup) inside it, for do not disturb, means
  /// it never pops up and only joins [`list`](Self::list).
  ///
  /// If it panics, that is logged with where `on_notification` was
  /// called, and it isn't called again. Called on a handle whose context
  /// went, it logs that and does nothing.
  #[track_caller]
  pub fn on_notification(&self, handler: impl Fn(&Notification) + 'static) {
    self.listen(Handler::Arrived(Rc::new(handler)), Location::caller());
  }

  /// Adds `handler` under the asking context's owner.
  fn listen(&self, handler: Handler, at: &'static Location<'static>) {
    if self.owner.is_disposed() {
      let name = handler.name();
      return tracing::warn!(
        "notifications {name} (called at {at}) on a context that is gone does nothing"
      );
    }
    if let Some(service) = &self.service {
      let id = service.listen(handler, at);
      // Dropped with the asking context's owner, which takes the handler away.
      self.owner.signal(Unlisten(Rc::clone(service), id));
    }
  }
}

/// What `on_closed` calls.
type CloseHandler = Rc<dyn Fn(&Notification, CloseReason)>;

/// What `on_closed` or `on_notification` calls.
#[derive(Clone)]
enum Handler {
  Closed(CloseHandler),
  Arrived(Rc<dyn Fn(&Notification)>),
}

impl Handler {
  /// The method that gave it, for the log.
  fn name(&self) -> &'static str {
    match self {
      Handler::Closed(_) => "on_closed",
      Handler::Arrived(_) => "on_notification",
    }
  }
}

/// A config's handler, from `on_closed` or `on_notification`.
#[derive(Clone)]
struct Listener {
  id: u64,
  handler: Handler,
  /// Where it was given, for the log if it panics.
  at: &'static Location<'static>,
}

/// Takes a listener away when its context's owner drops this.
struct Unlisten(Rc<Service>, u64);

impl Drop for Unlisten {
  fn drop(&mut self) {
    // Dropped outside the borrow: the handler may hold a `Notifications`.
    drop(self.0.unlisten(self.1));
  }
}

/// `cx.notifications()`.
pub(crate) fn notifications(cx: &Cx) -> Notifications {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.notifications() needs the services the shell provides; none arrive");
    });
    return Notifications {
      service: None,
      list: cx.signal(Vec::new()).read_only(),
      popups: cx.signal(Vec::new()).read_only(),
      owner: cx.owner(),
    };
  };
  let (list, popups) = service.start();
  Notifications {
    service: Some(service),
    list,
    popups,
    owner: cx.owner(),
  }
}

/// The notification server of one shell, started on first use.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
  /// Whether bodies are kept as markup, from `set_body_markup`.
  body_markup: Cell<bool>,
  /// What `Timeout::Default` means, from `set_default_timeout`.
  default_timeout: Cell<Duration>,
  /// What the shell before a reload kept, until the server starts.
  saved: RefCell<Option<Saved>>,
  /// Whether expiries wait for the bus to answer the name request, so a
  /// popup that expired during a reload is told to its app.
  held: Cell<bool>,
  /// Saved for a reload while serving: the next shell may `Take` what
  /// this one kept, once.
  handing_off: Cell<bool>,
  /// It answered a `Take` since the name last moved: the shell that took
  /// over may have failed, and the bus may hand the name back before
  /// this one hears so, so a save then still names this shell.
  handed_over: Cell<bool>,
  /// It saved since it last lost the name: the save came after a new
  /// shell failed, for the next one, which `NameAcquired` mustn't disarm.
  saved_since_lost: Cell<bool>,
  /// The old shell's name on the bus, until this one has taken the
  /// notifications over from it.
  taking_from: RefCell<Option<String>>,
}

/// What a reload keeps of the server: the history, the popups showing and
/// the next id, so apps' ids go on.
#[derive(Serialize, Deserialize)]
struct Saved {
  next_id: u32,
  history: Vec<Kept>,
  /// The ids showing as popups, newest first, each with the milliseconds
  /// left until it expires, `None` for never.
  showing: Vec<(u32, Option<u64>)>,
  /// The whole timeout of each popup that expires, in milliseconds, for
  /// its countdown bar. A shell from 0.8.2 or before kept none, and then
  /// the time left counts as whole.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  whole: Vec<(u32, u64)>,
  /// The shell's name on the bus while it served, so the next one takes
  /// the name over and asks it for what came after the save.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  owner: Option<String>,
}

/// A notification as a reload keeps it: all but pixels, which fall back
/// to the app's icon.
#[derive(Serialize, Deserialize)]
struct Kept {
  id: u32,
  app_name: String,
  app_icon: String,
  summary: String,
  body: String,
  actions: Vec<(String, String)>,
  urgency: Urgency,
  timeout: Timeout,
  image: Option<Image>,
  desktop_entry: String,
  category: String,
  resident: bool,
  transient: bool,
  received: SystemTime,
}

/// A picture a reload keeps.
#[derive(Serialize, Deserialize)]
enum Image {
  Name(String),
  File(std::path::PathBuf),
}

impl Kept {
  fn new(notification: &Notification) -> Kept {
    let image = match &notification.image {
      Some(IconSource::Name(name)) => Some(Image::Name(name.clone())),
      Some(IconSource::File(path)) => Some(Image::File(path.clone())),
      Some(IconSource::Pixels(_)) | None => None,
    };
    Kept {
      id: notification.id,
      app_name: notification.app_name.clone(),
      app_icon: notification.app_icon.clone(),
      summary: notification.summary.clone(),
      body: notification.body.clone(),
      actions: notification.actions.clone(),
      urgency: notification.urgency,
      timeout: notification.timeout,
      image,
      desktop_entry: notification.desktop_entry.clone(),
      category: notification.category.clone(),
      resident: notification.resident,
      transient: notification.transient,
      received: notification.received,
    }
  }

  /// The notification, within the caps of one that arrives, since the
  /// file it came from is outside data.
  fn notification(self) -> Notification {
    use server::{MAX_ACTIONS, MAX_BODY, MAX_SUMMARY, MAX_TEXT, cut, cut_markup};

    let text = |text: &str| cut(text, MAX_TEXT);
    let actions = self.actions.iter().take(MAX_ACTIONS);
    let image = match self.image {
      Some(Image::Name(name)) => Some(IconSource::Name(name)),
      Some(Image::File(path)) => Some(IconSource::File(path)),
      None => server::icon(&self.app_icon),
    };
    Notification {
      id: self.id,
      app_name: text(&self.app_name),
      app_icon: text(&self.app_icon),
      summary: cut(&self.summary, MAX_SUMMARY),
      body: cut_markup(&self.body, MAX_BODY),
      actions: actions
        .map(|(key, label)| (text(key), text(label)))
        .collect(),
      urgency: self.urgency,
      timeout: self.timeout,
      image,
      desktop_entry: text(&self.desktop_entry),
      category: text(&self.category),
      resident: self.resident,
      transient: self.transient,
      received: self.received,
    }
  }
}

struct State {
  list: Signal<Vec<Notification>>,
  popups: Signal<Vec<Notification>>,
  /// The session bus, once connected.
  connection: Option<Connection>,
  /// Whether this shell owns the name now, so it answers and signals.
  serving: bool,
  /// Every notification kept, oldest first.
  history: Vec<Notification>,
  /// The popups showing.
  showing: Showing,
  /// The id the next notification gets, unless one kept has it.
  next_id: u32,
  /// The expiry timer, while a popup waits to expire.
  timer: Option<RegistrationToken>,
  /// The config's close handlers, in the order given.
  listeners: Vec<Listener>,
  /// The id the next listener gets.
  next_listener: u64,
}

impl State {
  /// A new id: the count goes on from the last, skipping 0 when it wraps
  /// and any id still kept.
  fn new_id(&mut self) -> u32 {
    loop {
      let id = self.next_id;
      self.next_id = id.checked_add(1).unwrap_or(1);
      if !self.history.iter().any(|kept| kept.id == id) {
        return id;
      }
    }
  }
}

/// The popups showing, newest first.
type Showing = Vec<Popup>;

/// A notification showing as a popup, and its countdown.
#[derive(Clone, Copy)]
struct Popup {
  id: u32,
  /// How long it shows in all, `None` for until dismissed.
  timeout: Option<Duration>,
  /// The time it had left at `since`.
  left: Duration,
  /// When it last started counting or was held.
  since: Instant,
  /// Whether its countdown stopped, from `hold_popup`.
  held: bool,
}

impl Popup {
  fn new(id: u32, timeout: Option<Duration>, now: Instant) -> Popup {
    Popup {
      id,
      timeout,
      left: timeout.unwrap_or_default(),
      since: now,
      held: false,
    }
  }

  /// The time it has left at `now`, `None` for never.
  fn left(&self, now: Instant) -> Option<Duration> {
    self.timeout?;
    match self.held {
      true => Some(self.left),
      false => Some(
        self
          .left
          .saturating_sub(now.saturating_duration_since(self.since)),
      ),
    }
  }

  /// When it expires, `None` while held or for never.
  fn expires(&self) -> Option<Instant> {
    self.timeout.filter(|_| !self.held)?;
    self.since.checked_add(self.left)
  }

  fn hold(&mut self, held: bool, now: Instant) {
    if held != self.held {
      self.left = self.left(now).unwrap_or(self.left);
      self.since = now;
      self.held = held;
    }
  }
}

/// A signal for the apps, sent in order.
enum Event {
  /// `NotificationClosed`: the id and why.
  Closed(u32, CloseReason),
  /// `ActionInvoked`: the id and the action's key.
  Invoked(u32, String),
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      default_timeout: Cell::new(shared.config.popup_timeout),
      shared,
      state: RefCell::new(None),
      body_markup: Cell::new(false),
      saved: RefCell::new(None),
      held: Cell::new(false),
      handing_off: Cell::new(false),
      handed_over: Cell::new(false),
      saved_since_lost: Cell::new(false),
      taking_from: RefCell::new(None),
    }
  }

  /// Lets expiries run, now that the name request was answered and any
  /// takeover done, or never will be.
  fn release(self: &Rc<Self>) {
    self.taking_from.take();
    if self.held.replace(false) {
      self.schedule();
    }
  }

  /// Whether this shell is still taking the notifications over from the
  /// one before a reload.
  pub(crate) fn taking_over(&self) -> bool {
    self.taking_from.borrow().is_some()
  }

  /// What a reload keeps, or `None` when the server never started. While
  /// serving, the next shell may then `Take` the notifications that come
  /// after this.
  pub(crate) fn save(&self) -> Option<serde_json::Value> {
    let saved = self.kept()?;
    self.handing_off.set(saved.owner.is_some());
    self.saved_since_lost.set(true);
    serde_json::to_value(saved)
      .inspect_err(|error| tracing::warn!(%error, "the notifications couldn't be kept"))
      .ok()
  }

  /// The history, popups and next id as a reload keeps them, with this
  /// shell's name on the bus while it serves.
  fn kept(&self) -> Option<Saved> {
    let state = self.state.borrow();
    let state = state.as_ref()?;
    let now = Instant::now();
    let ms = |time: Duration| u64::try_from(time.as_millis()).unwrap_or(u64::MAX);
    let showing = (state.showing.iter()).map(|popup| (popup.id, popup.left(now).map(ms)));
    let whole = (state.showing.iter()).filter_map(|popup| Some((popup.id, ms(popup.timeout?))));
    let owner = (state.connection.as_ref()).filter(|_| state.serving || self.handed_over.get());
    let owner = owner.and_then(|connection| connection.unique_name());
    Some(Saved {
      next_id: state.next_id,
      history: state.history.iter().map(Kept::new).collect(),
      showing: showing.collect(),
      whole: whole.collect(),
      owner: owner.map(|name| name.to_string()),
    })
  }

  /// The old shell's answer to `Take`: what it kept, as JSON, once, and
  /// only to the shell that took the name from it. `None` refuses.
  async fn hand_over(&self, connection: &Connection, caller: Option<&str>) -> Option<String> {
    if !self.handing_off.get() {
      return None;
    }
    let ask = connection.call_method(
      Some(BUS),
      BUS_PATH,
      Some(BUS),
      "GetNameOwner",
      &NOTIFICATIONS,
    );
    let owner = self.shared.timeout(HANDOFF_LIMIT, ask).await.ok()?;
    let owner = owner.body().deserialize::<String>().ok()?;
    if caller != Some(owner.as_str()) || !self.handing_off.replace(false) {
      return None;
    }
    tracing::info!("handing the notifications over to the new shell");
    self.handed_over.set(true);
    serde_json::to_string(&self.kept()?).ok()
  }

  /// The new shell's side: takes what the old shell `from` kept, in place
  /// of what it saved earlier, so what came during the swap is there.
  async fn take_over(self: &Rc<Self>, connection: &Connection, from: &str) {
    let ask = connection.call_method(Some(from), NOTIFICATIONS_PATH, Some(HANDOFF), "Take", &());
    let answer = self.shared.timeout(HANDOFF_LIMIT, ask).await;
    let text = match answer.and_then(|answer| answer.body().deserialize::<String>()) {
      Ok(text) if text.len() <= MAX_HANDOFF => text,
      Ok(text) => {
        return tracing::warn!(
          bytes = text.len(),
          "the old shell's notifications are too big; keeping what it saved"
        );
      }
      Err(error) => {
        return tracing::warn!(%error, "the old shell didn't hand its notifications over; keeping what it saved");
      }
    };
    let saved = match serde_json::from_str::<Saved>(&text) {
      Ok(saved) => saved,
      Err(error) => {
        return tracing::warn!(%error, "the old shell's notifications don't fit; keeping what it saved");
      }
    };
    let (history, showing, next_id) = restored(saved, Instant::now());
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.history = history;
      state.showing = showing;
      state.next_id = next_id;
    }
    tracing::info!("took the notifications over from the old shell");
    self.changed();
  }

  /// Starts from what the shell before a reload kept, once a config asks
  /// for the notifications. One that doesn't fit is logged and dropped.
  pub(crate) fn restore(&self, saved: serde_json::Value) {
    match serde_json::from_value::<Saved>(saved) {
      Ok(saved) => *self.saved.borrow_mut() = Some(saved),
      Err(error) => tracing::warn!(%error, "the notifications kept don't fit; starting empty"),
    }
  }

  /// The history and the popups, joining the bus if this is the first
  /// ask.
  fn start(self: &Rc<Self>) -> (ReadSignal<Vec<Notification>>, ReadSignal<Vec<Notification>>) {
    if let Some(state) = &*self.state.borrow() {
      return (state.list.read_only(), state.popups.read_only());
    }
    let saved = self.saved.take();
    let from = saved.as_ref().and_then(|saved| saved.owner.clone());
    let (history, showing, next_id) = saved.map_or_else(
      || (Vec::new(), Vec::new(), 1),
      |saved| restored(saved, Instant::now()),
    );
    let popups_shown = showing.iter().filter_map(|popup| {
      history
        .iter()
        .find(|kept: &&Notification| kept.id == popup.id)
        .cloned()
    });
    let list = self.shared.root.signal(history.clone());
    let popups = self.shared.root.signal(popups_shown.collect());
    *self.state.borrow_mut() = Some(State {
      list,
      popups,
      connection: None,
      serving: false,
      history,
      showing,
      next_id,
      timer: None,
      listeners: Vec::new(),
      next_listener: 0,
    });
    self.held.set(self.shared.config.session_bus.is_some());
    if self.held.get() {
      *self.taking_from.borrow_mut() = from;
    }
    self.schedule();
    match self.shared.config.session_bus.clone() {
      Some(address) => self.shared.spawn(run(Rc::clone(self), address)),
      None => tracing::info!("no session bus, so no notifications arrive"),
    }
    (list.read_only(), popups.read_only())
  }

  /// Whether the history is still there: its owner, the root, isn't
  /// disposed.
  fn alive(&self) -> bool {
    let state = self.state.borrow();
    let alive = state.as_ref().and_then(|state| state.list.try_with(|_| ()));
    alive.is_some()
  }

  fn serving(&self) -> bool {
    let state = self.state.borrow();
    state.as_ref().is_some_and(|state| state.serving)
  }

  /// Keeps `notification`, in place of `replaces` if that one is kept,
  /// and shows it as a popup. Returns its id.
  fn notify(self: &Rc<Self>, replaces: u32, mut notification: Notification) -> u32 {
    let now = Instant::now();
    let timeout = match (notification.urgency, notification.timeout) {
      (Urgency::Critical, _) | (_, Timeout::Never) => None,
      (_, Timeout::Default) => Some(self.default_timeout.get()),
      (_, Timeout::After(after)) => Some(after),
    };
    let timeout = timeout.filter(|timeout| now.checked_add(*timeout).is_some());
    let (id, arrived, dropped) = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      // The spec returns `replaces` even when it is gone, so it names
      // the new one then.
      let id = match replaces {
        0 => state.new_id(),
        id => id,
      };
      notification.id = id;
      let arrived = notification.clone();
      match state.history.iter().position(|kept| kept.id == id) {
        Some(at) => state.history[at] = notification,
        None => state.history.push(notification),
      }
      let popup = Popup::new(id, timeout, now);
      match state.showing.iter_mut().find(|shown| shown.id == id) {
        Some(shown) => {
          *shown = Popup {
            held: shown.held,
            ..popup
          }
        }
        None => state.showing.insert(0, popup),
      }
      let excess = state.history.len().saturating_sub(MAX_KEPT);
      let dropped: Vec<Notification> = state.history.drain(..excess).collect();
      let history = &state.history;
      state
        .showing
        .retain(|popup| history.iter().any(|kept| kept.id == popup.id));
      keep_pixels_within(&mut state.history, MAX_PIXELS_KEPT);
      (id, arrived, dropped)
    };
    // Before `closed` shows the lists, so a handler can hide its popup.
    self.each_handler(|handler| {
      if let Handler::Arrived(handler) = handler {
        handler(&arrived);
      }
    });
    self.closed(Vec::new(), dropped, CloseReason::Expired);
    id
  }

  /// Holds popup `id`'s countdown, or resumes it, as
  /// [`Notifications::hold_popup`] does.
  fn hold(self: &Rc<Self>, id: u32, held: bool) {
    if let Some(state) = self.state.borrow_mut().as_mut()
      && let Some(popup) = state.showing.iter_mut().find(|popup| popup.id == id)
    {
      popup.hold(held, Instant::now());
    }
    self.schedule();
  }

  /// Popup `id`'s time left and whole timeout, as
  /// [`Notifications::popup_time_left`] gives them.
  fn time_left(&self, id: u32) -> Option<(Duration, Duration)> {
    let state = self.state.borrow();
    let popup = state
      .as_ref()?
      .showing
      .iter()
      .find(|popup| popup.id == id)?;
    // Never a zero whole, which a bar would divide by.
    let whole = popup.timeout.filter(|whole| !whole.is_zero())?;
    Some((popup.left(Instant::now())?, whole))
  }

  /// Ends the popups `ids` as their expiry does: each stays in the
  /// history, unless it is transient, when its app hears that it expired.
  fn hide(self: &Rc<Self>, ids: &[u32]) {
    let transient = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.showing.retain(|popup| !ids.contains(&popup.id));
      let history = state.history.iter();
      let transient = history.filter(|kept| kept.transient && ids.contains(&kept.id));
      transient.map(|kept| kept.id).collect::<Vec<_>>()
    };
    let taken = self.take(&transient);
    self.closed(Vec::new(), taken, CloseReason::Expired);
  }

  /// Takes `ids` away and tells their apps `reason`. Returns whether any
  /// was kept.
  fn close(self: &Rc<Self>, ids: &[u32], reason: CloseReason) -> bool {
    let taken = self.take(ids);
    if taken.is_empty() {
      return false;
    }
    self.closed(Vec::new(), taken, reason);
    true
  }

  /// Picks action `key` of notification `id`, as [`Notifications::invoke`]
  /// does.
  fn invoke(self: &Rc<Self>, id: u32, key: &str) {
    let resident = self.state.borrow().as_ref().and_then(|state| {
      let kept = state.history.iter().find(|kept| kept.id == id)?;
      let offered = kept.actions.iter().any(|(action, _)| action == key);
      offered.then_some(kept.resident)
    });
    let Some(resident) = resident else {
      return tracing::warn!(id, key, "no such notification action");
    };
    let events = vec![Event::Invoked(id, key.to_owned())];
    let taken = if resident {
      Vec::new()
    } else {
      self.take(&[id])
    };
    self.closed(events, taken, CloseReason::Dismissed);
  }

  /// Takes `ids` out of the history and the popups, and returns those
  /// that were kept, oldest first.
  fn take(&self, ids: &[u32]) -> Vec<Notification> {
    let mut state = self.state.borrow_mut();
    let Some(state) = state.as_mut() else {
      return Vec::new();
    };
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut state.history)
      .into_iter()
      .partition(|kept| ids.contains(&kept.id));
    state.history = kept;
    state.showing.retain(|popup| !ids.contains(&popup.id));
    taken
  }

  /// Tells the apps `events`, then that `closed` went for `reason`, shows
  /// the lists without them, and then calls the config's handlers.
  fn closed(
    self: &Rc<Self>,
    mut events: Vec<Event>,
    closed: Vec<Notification>,
    reason: CloseReason,
  ) {
    events.extend(closed.iter().map(|gone| Event::Closed(gone.id, reason)));
    self.emit(events);
    self.changed();
    self.deliver(&closed, reason);
  }

  /// Adds a config's handler. Its id.
  fn listen(&self, handler: Handler, at: &'static Location<'static>) -> u64 {
    let mut state = self.state.borrow_mut();
    let state = state
      .as_mut()
      .expect("invariant: started by cx.notifications()");
    state.next_listener += 1;
    let id = state.next_listener;
    state.listeners.push(Listener { id, handler, at });
    id
  }

  /// Takes a handler away, handing it back to drop outside the borrow.
  fn unlisten(&self, id: u64) -> Option<Listener> {
    let mut state = self.state.borrow_mut();
    let listeners = &mut state.as_mut()?.listeners;
    let index = listeners.iter().position(|listener| listener.id == id)?;
    Some(listeners.remove(index))
  }

  fn listening(&self, id: u64) -> bool {
    let state = self.state.borrow();
    let mut listeners = state.iter().flat_map(|state| &state.listeners);
    listeners.any(|listener| listener.id == id)
  }

  /// Hands each of `closed` to the config's close handlers.
  fn deliver(&self, closed: &[Notification], reason: CloseReason) {
    for gone in closed {
      self.each_handler(|handler| {
        if let Handler::Closed(handler) = handler {
          handler(gone, reason);
        }
      });
    }
  }

  /// Runs `call` on each of the config's handlers, with nothing borrowed,
  /// since a handler may call the service, and untracked, so what one
  /// reads never reruns an effect that changed the notifications. A
  /// handler that panics is logged with where it was given, and taken
  /// away.
  fn each_handler(&self, call: impl Fn(&Handler)) {
    let listeners = match self.state.borrow().as_ref() {
      Some(state) => state.listeners.clone(),
      None => return,
    };
    for Listener { id, handler, at } in &listeners {
      // A handler before this one may have taken this one's context.
      if !self.listening(*id) {
        continue;
      }
      let run = AssertUnwindSafe(|| untrack(|| call(handler)));
      if let Err(payload) = panic::catch_unwind(run) {
        let (name, message) = (handler.name(), panic_message(payload.as_ref()));
        tracing::error!("notifications {name} (called at {at}) panicked: {message}");
        drop(self.unlisten(*id));
      }
    }
  }

  /// The timer's tick: ends the popups whose time ran out.
  fn expire(self: &Rc<Self>) -> TimeoutAction {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      // This timer goes once the tick returns.
      state.timer = None;
    }
    if !self.alive() {
      return TimeoutAction::Drop;
    }
    let now = Instant::now();
    let expired = self.state.borrow().as_ref().map(|state| {
      let showing = state.showing.iter();
      let expired = showing.filter(|popup| popup.expires().is_some_and(|at| at <= now));
      expired.map(|popup| popup.id).collect::<Vec<_>>()
    });
    if let Some(expired) = expired {
      self.hide(&expired);
    }
    TimeoutAction::Drop
  }

  /// Shows the history and popups as they are now, and sets the timer
  /// for the next popup to expire.
  fn changed(self: &Rc<Self>) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let popups = state.showing.iter().filter_map(|popup| {
        let kept = state.history.iter().find(|kept| kept.id == popup.id);
        kept.cloned()
      });
      let popups = popups.collect::<Vec<_>>();
      (state.list, state.history.clone(), state.popups, popups)
    });
    if let Some((list_signal, list, popups_signal, popups)) = shown {
      set_changed(list_signal, list);
      set_changed(popups_signal, popups);
    }
    self.schedule();
  }

  /// Sets the one timer afresh, at the soonest expiry, or none while no
  /// popup expires.
  fn schedule(self: &Rc<Self>) {
    if self.held.get() {
      return;
    }
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let mut state = self.state.borrow_mut();
    let Some(state) = state.as_mut() else {
      return;
    };
    if let Some(token) = state.timer.take() {
      event_loop.remove(token);
    }
    let Some(next) = state.showing.iter().filter_map(Popup::expires).min() else {
      return;
    };
    let service = Rc::clone(self);
    let tick = Box::new(move || service.expire());
    state.timer = event_loop.insert_timer(next, tick);
  }

  /// Sends `events` in order, while this shell serves.
  fn emit(&self, events: Vec<Event>) {
    let connection = {
      let state = self.state.borrow();
      let serving = state.as_ref().filter(|state| state.serving);
      serving.and_then(|state| state.connection.clone())
    };
    let Some(connection) = connection.filter(|_| !events.is_empty()) else {
      return;
    };
    self.shared.spawn(async move {
      for event in events {
        let (path, interface) = (NOTIFICATIONS_PATH, NOTIFICATIONS);
        let sent = match &event {
          Event::Closed(id, reason) => {
            let body = (*id, reason.code());
            let signal = "NotificationClosed";
            let emit = connection.emit_signal(None::<&str>, path, interface, signal, &body);
            emit.await
          }
          Event::Invoked(id, key) => {
            let body = (*id, key.as_str());
            let signal = "ActionInvoked";
            let emit = connection.emit_signal(None::<&str>, path, interface, signal, &body);
            emit.await
          }
        };
        if let Err(error) = sent {
          tracing::warn!(%error, "a notification signal wasn't sent");
        }
      }
    });
  }

  /// Acts on one message from the bus.
  async fn on_message(self: &Rc<Self>, connection: &Connection, message: &Message) {
    match message.header().message_type() {
      Kind::MethodCall => server::answer(self, connection, message).await,
      Kind::Signal => self.on_signal(message),
      _ => {}
    }
  }

  /// The bus handing this shell the name, or taking it away.
  fn on_signal(&self, message: &Message) {
    let header = message.header();
    let from_bus = header.sender().is_some_and(|sender| sender.as_str() == BUS);
    let ours = message.body().deserialize::<&str>().ok() == Some(NOTIFICATIONS);
    if !from_bus || !ours {
      return;
    }
    let serving = match header.member().map(|member| member.as_str()) {
      Some("NameAcquired") => {
        tracing::info!("serving notifications");
        // Back from a new shell that failed: no one is to take them now,
        // unless this shell saved again since, for the next one.
        if !self.saved_since_lost.get() {
          self.handing_off.set(false);
        }
        self.handed_over.set(false);
        true
      }
      Some("NameLost") => {
        tracing::info!("another program took over notifications; waiting for it to leave");
        self.handed_over.set(false);
        self.saved_since_lost.set(false);
        false
      }
      _ => return,
    };
    if let Some(state) = self.state.borrow_mut().as_mut() {
      state.serving = serving;
    }
  }
}

/// Joins the session bus at `address`, queues for the name, and acts on
/// what arrives, until the bus closes or the history is disposed.
async fn run(service: Rc<Service>, address: zbus::Address) {
  let builder = zbus::connection::Builder::address(address);
  let connection = match builder.build().await {
    Ok(connection) => connection,
    Err(error) => {
      service.release();
      return tracing::info!(%error, "can't reach the session bus, so no notifications arrive");
    }
  };
  // Made before anything is asked, so no call or NameAcquired is missed.
  let mut messages = MessageStream::from(&connection);
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.connection = Some(connection.clone());
  }
  // Queued rather than refused, so the bus hands the name over when the
  // daemon that has it leaves. NameAcquired says so, in `on_signal`. On a
  // reload the name is taken from the old shell, which allows that.
  let from = service.taking_from.borrow().clone();
  let mut flags = RequestNameFlags::ALLOW_REPLACEMENT;
  if from.is_some() {
    flags |= RequestNameFlags::REPLACE_EXISTING;
  }
  let requested = connection
    .request_name_with_flags(NOTIFICATIONS, flags)
    .await;
  let owner = matches!(
    requested,
    Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner)
  );
  if owner && let Some(state) = service.state.borrow_mut().as_mut() {
    // As NameAcquired says too, so expiries held till now reach the apps.
    state.serving = true;
  }
  // Calls that arrive meanwhile are held, so ids don't clash. They are
  // read off the connection meanwhile: past zbus's queue of 64 its reader
  // would wait, and the old shell's answer with it.
  let mut held = Vec::new();
  if owner && let Some(from) = from {
    let mut take = pin!(service.take_over(&connection, &from));
    poll_fn(|cx| {
      while held.len() < MAX_HELD
        && let Poll::Ready(Some(message)) = Pin::new(&mut messages).poll_next(cx)
      {
        held.push(message);
      }
      take.as_mut().poll(cx)
    })
    .await;
  }
  service.release();
  for message in held {
    match message {
      Ok(message) => service.on_message(&connection, &message).await,
      Err(error) => tracing::warn!(%error, "a bad message on the session bus"),
    }
  }
  match requested {
    Ok(RequestNameReply::InQueue) => {
      tracing::info!("another program serves notifications; waiting for it to leave");
    }
    Ok(_) => {}
    Err(error) => return tracing::warn!(%error, "couldn't ask for the notifications name"),
  }
  loop {
    let next = poll_fn(|cx| Pin::new(&mut messages).poll_next(cx)).await;
    if !service.alive() {
      return;
    }
    match next {
      Some(Ok(message)) => service.on_message(&connection, &message).await,
      Some(Err(error)) => tracing::warn!(%error, "a bad message on the session bus"),
      None => break,
    }
  }
  tracing::warn!("the session bus closed, so no more notifications arrive");
  if let Some(state) = service.state.borrow_mut().as_mut() {
    state.serving = false;
    state.connection = None;
  }
}

/// The history, popups and next id `saved` holds, as of `now`, within the
/// caps: ids unique and never 0, and popups only of those kept.
fn restored(saved: Saved, now: Instant) -> (Vec<Notification>, Showing, u32) {
  let mut history: Vec<Notification> = Vec::new();
  for kept in saved.history {
    if kept.id != 0 && !history.iter().any(|seen| seen.id == kept.id) {
      history.push(kept.notification());
    }
  }
  let excess = history.len().saturating_sub(MAX_KEPT);
  history.drain(..excess);
  let mut showing = Showing::new();
  for (id, left) in saved.showing {
    let known = history.iter().any(|kept| kept.id == id);
    if known && !showing.iter().any(|shown| shown.id == id) {
      // Past what the clock holds, it expires at once.
      let left = left.map(Duration::from_millis);
      let left = left.map(|left| match now.checked_add(left) {
        Some(_) => left,
        None => Duration::ZERO,
      });
      let whole = saved.whole.iter().find(|(whole, _)| *whole == id);
      let whole = whole.map(|(_, ms)| Duration::from_millis(*ms));
      let timeout = left.map(|left| whole.unwrap_or(left).max(left));
      showing.push(Popup {
        left: left.unwrap_or_default(),
        ..Popup::new(id, timeout, now)
      });
    }
  }
  (history, showing, saved.next_id.max(1))
}

/// Drops the pixels of the oldest notifications past `budget` bytes in
/// all, each falling back to its app's icon.
fn keep_pixels_within(history: &mut [Notification], budget: usize) {
  let mut total = 0;
  for kept in history.iter_mut().rev() {
    let Some(IconSource::Pixels(pixels)) = &kept.image else {
      continue;
    };
    let bytes = pixels.data.len();
    if total + bytes <= budget {
      total += bytes;
    } else {
      tracing::debug!(
        id = kept.id,
        "past the pictures' budget; showing the app icon"
      );
      kept.image = server::icon(&kept.app_icon);
    }
  }
}
