//! A notification server, as the Desktop Notifications spec 1.2 has it:
//! apps call `org.freedesktop.Notifications` on the session bus, and the
//! shell keeps what they send.
//!
//! The shell queues for the name, so it serves while no other daemon,
//! such as mako or dunst, does, and takes over when that one leaves.
//! zbus has no object server here, so the calls are answered by hand on
//! the main thread, in [`server`]. One stream reads every message the bus
//! sends this connection, and one loop timer runs only while a popup
//! waits to expire, so a quiet bus leaves the shell asleep.

mod server;

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::panic::{self, AssertUnwindSafe, Location};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Once;
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
use crate::dbus::BUS;
use crate::dbus::set_changed;
use crate::lines::panic_message;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

/// How long a popup shows when its app leaves that to the server.
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
  /// As long as the server likes: 5 s.
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
  /// [`Timeout::Never`] or it is [`Urgency::Critical`]. An expired one
  /// stays in [`list`](Self::list), unless it is transient.
  pub fn popups(&self) -> ReadSignal<Vec<Notification>> {
    self.popups
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
    let at = Location::caller();
    if self.owner.is_disposed() {
      return tracing::warn!(
        "notifications on_closed (called at {at}) on a context that is gone does nothing"
      );
    }
    if let Some(service) = &self.service {
      let id = service.listen(Rc::new(handler), at);
      // Dropped with the asking context's owner, which takes the handler away.
      self.owner.signal(Unlisten(Rc::clone(service), id));
    }
  }
}

/// What `on_closed` calls.
type CloseHandler = Rc<dyn Fn(&Notification, CloseReason)>;

/// A config's handler of closed notifications, from `on_closed`.
#[derive(Clone)]
struct Listener {
  id: u64,
  handler: CloseHandler,
  /// Where `on_closed` was called, for the log if it panics.
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
  /// What the shell before a reload kept, until the server starts.
  saved: RefCell<Option<Saved>>,
  /// Whether expiries wait for the bus to answer the name request, so a
  /// popup that expired during a reload is told to its app.
  held: Cell<bool>,
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
  /// The ids showing as popups.
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

/// The ids showing as popups, newest first, each with when it expires,
/// `None` for never.
type Showing = Vec<(u32, Option<Instant>)>;

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
      shared,
      state: RefCell::new(None),
      body_markup: Cell::new(false),
      saved: RefCell::new(None),
      held: Cell::new(false),
    }
  }

  /// Lets expiries run, now that the name request was answered or never
  /// will be.
  fn release(self: &Rc<Self>) {
    if self.held.replace(false) {
      self.schedule();
    }
  }

  /// What a reload keeps, or `None` when the server never started.
  pub(crate) fn save(&self) -> Option<serde_json::Value> {
    let state = self.state.borrow();
    let state = state.as_ref()?;
    let now = Instant::now();
    let showing = state.showing.iter().map(|(id, expires)| {
      let left = expires.map(|at| at.saturating_duration_since(now).as_millis());
      (
        *id,
        left.map(|left| u64::try_from(left).unwrap_or(u64::MAX)),
      )
    });
    let saved = Saved {
      next_id: state.next_id,
      history: state.history.iter().map(Kept::new).collect(),
      showing: showing.collect(),
    };
    serde_json::to_value(saved)
      .inspect_err(|error| tracing::warn!(%error, "the notifications couldn't be kept"))
      .ok()
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
    let (history, showing, next_id) = self.saved.take().map_or_else(
      || (Vec::new(), Vec::new(), 1),
      |saved| restored(saved, Instant::now()),
    );
    let popups_shown = showing.iter().filter_map(|(id, _)| {
      history
        .iter()
        .find(|kept: &&Notification| kept.id == *id)
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
    let expires = match (notification.urgency, notification.timeout) {
      (Urgency::Critical, _) | (_, Timeout::Never) => None,
      (_, Timeout::Default) => Some(now + self.shared.config.popup_timeout),
      (_, Timeout::After(after)) => Some(now + after),
    };
    let (id, dropped) = {
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
      match state.history.iter().position(|kept| kept.id == id) {
        Some(at) => state.history[at] = notification,
        None => state.history.push(notification),
      }
      match state.showing.iter_mut().find(|(shown, _)| *shown == id) {
        Some(shown) => shown.1 = expires,
        None => state.showing.insert(0, (id, expires)),
      }
      let excess = state.history.len().saturating_sub(MAX_KEPT);
      let dropped: Vec<Notification> = state.history.drain(..excess).collect();
      let history = &state.history;
      state
        .showing
        .retain(|(id, _)| history.iter().any(|kept| kept.id == *id));
      keep_pixels_within(&mut state.history, MAX_PIXELS_KEPT);
      (id, dropped)
    };
    self.closed(Vec::new(), dropped, CloseReason::Expired);
    id
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
    state.showing.retain(|(id, _)| !ids.contains(id));
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

  /// Adds a config's close handler. Its id.
  fn listen(&self, handler: CloseHandler, at: &'static Location<'static>) -> u64 {
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

  /// Hands each of `closed` to the config's handlers, with nothing
  /// borrowed, since a handler may call the service, and untracked, so
  /// what one reads never reruns an effect that closed a notification. A
  /// handler that panics is logged with where `on_closed` was called, and
  /// taken away.
  fn deliver(&self, closed: &[Notification], reason: CloseReason) {
    let listeners = match self.state.borrow().as_ref() {
      Some(state) if !closed.is_empty() => state.listeners.clone(),
      _ => return,
    };
    for gone in closed {
      for Listener { id, handler, at } in &listeners {
        // A handler before this one may have taken this one's context.
        if !self.listening(*id) {
          continue;
        }
        let run = AssertUnwindSafe(|| untrack(|| handler(gone, reason)));
        if let Err(payload) = panic::catch_unwind(run) {
          let message = panic_message(payload.as_ref());
          tracing::error!("notifications on_closed (called at {at}) panicked: {message}");
          drop(self.unlisten(*id));
        }
      }
    }
  }

  /// The timer's tick: takes the expired popups away, and the transient
  /// ones out of the history too.
  fn expire(self: &Rc<Self>) -> TimeoutAction {
    if let Some(state) = self.state.borrow_mut().as_mut() {
      // This timer goes once the tick returns.
      state.timer = None;
    }
    if !self.alive() {
      return TimeoutAction::Drop;
    }
    let now = Instant::now();
    let transient = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return TimeoutAction::Drop;
      };
      let (expired, showing) = std::mem::take(&mut state.showing)
        .into_iter()
        .partition::<Vec<_>, _>(|(_, expires)| expires.is_some_and(|at| at <= now));
      state.showing = showing;
      let history = state.history.iter();
      let transient = history.filter(|kept| kept.transient);
      transient
        .map(|kept| kept.id)
        .filter(|id| expired.iter().any(|(gone, _)| gone == id))
        .collect::<Vec<_>>()
    };
    let taken = self.take(&transient);
    self.closed(Vec::new(), taken, CloseReason::Expired);
    TimeoutAction::Drop
  }

  /// Shows the history and popups as they are now, and sets the timer
  /// for the next popup to expire.
  fn changed(self: &Rc<Self>) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let popups = state.showing.iter().filter_map(|(id, _)| {
        let kept = state.history.iter().find(|kept| kept.id == *id);
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
    let Some(next) = state.showing.iter().filter_map(|(_, at)| *at).min() else {
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
        true
      }
      Some("NameLost") => {
        tracing::info!("another program took over notifications; waiting for it to leave");
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
  // daemon that has it leaves. NameAcquired says so, in `on_signal`.
  let flags = RequestNameFlags::ALLOW_REPLACEMENT;
  let requested = connection
    .request_name_with_flags(NOTIFICATIONS, flags)
    .await;
  if let Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) = requested
    && let Some(state) = service.state.borrow_mut().as_mut()
  {
    // As NameAcquired says too, so expiries held till now reach the apps.
    state.serving = true;
  }
  service.release();
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
    if known && !showing.iter().any(|(shown, _)| *shown == id) {
      let left = left.map(Duration::from_millis);
      showing.push((id, left.map(|left| now.checked_add(left).unwrap_or(now))));
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
