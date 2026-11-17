//! logind on the system bus: when the machine goes to sleep, holding
//! sleep off until a config is ready for it, and the power actions.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::os::fd::OwnedFd;
use std::panic::Location;
use std::rc::Rc;
use std::sync::Once;

use crabshell_reactive::{Owner, ReadSignal, Signal};
use crabshell_ui::{Cx, IntoProp};
use zbus::{Connection, Message};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, PROPERTIES, set_changed};

pub(crate) const LOGIND: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
/// The shell's own session, whichever it is.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION: &str = "org.freedesktop.login1.Session";

/// logind, from `cx.logind()`. Cloning it is cheap.
#[derive(Clone)]
pub struct Logind {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  sleeping: ReadSignal<bool>,
  /// The asking context's owner, which a delay lives as long as.
  owner: Owner,
}

impl Logind {
  /// Whether the machine is about to sleep: `true` from logind's
  /// `PrepareForSleep(true)` until its `PrepareForSleep(false)` after
  /// the resume. `false` while logind isn't running.
  pub fn preparing_for_sleep(&self) -> ReadSignal<bool> {
    self.sleeping
  }

  /// Holds sleep off until `ready` reads `true`, as a lock screen locks
  /// before the machine sleeps. While the asking context lives, the
  /// shell holds a logind delay inhibitor. When the machine is about to
  /// sleep, it lets go as soon as `ready` reads `true`, and takes a new
  /// one after the resume. logind's `InhibitDelayMaxSec`, 5 s by
  /// default, bounds the wait. Each call holds its own. An inhibitor
  /// logind refuses is logged and asked for again only after the next
  /// resume or when logind starts again.
  ///
  /// `ready` is read only while the machine is about to sleep. If it
  /// panics, that is logged and the inhibitor goes.
  #[track_caller]
  pub fn delay_sleep_until(&self, ready: impl IntoProp<bool>) {
    let at = Location::caller();
    if self.owner.is_disposed() {
      return tracing::warn!(
        "Logind::delay_sleep_until (called at {at}) on a context that is gone does nothing"
      );
    }
    let Some(service) = &self.service else {
      return;
    };
    let Some((sleeping, connected)) = service.signals() else {
      return;
    };
    let ready = ready.into_prop().shared();
    let hold = Rc::new(Hold {
      fd: RefCell::new(None),
      asking: Cell::new(None),
      arrived: self.owner.signal(0),
    });
    let service = Rc::clone(service);
    // The effect owns the hold, so the inhibitor goes with the context,
    // or when a panic in `ready` disables the effect.
    self
      .owner
      .effect(move || {
        if !connected.get() {
          hold.release();
        } else if !sleeping.get() {
          hold.take(&service);
        } else {
          // Read too when an inhibitor asked for before arrives late.
          hold.arrived.get();
          if ready() {
            hold.release();
          }
        }
      })
      .named(format!("Logind::delay_sleep_until (called at {at})"));
  }

  /// Turns the machine off, as logind's `PowerOff`. polkit may ask the
  /// user first. A refusal is logged, and so is a call made before
  /// logind is reached, which is dropped; the same holds for the other
  /// power actions.
  pub fn power_off(&self) {
    self.manager("PowerOff");
  }

  /// Restarts the machine, as logind's `Reboot`.
  pub fn reboot(&self) {
    self.manager("Reboot");
  }

  /// Sends the machine to sleep, as logind's `Suspend`.
  pub fn suspend(&self) {
    self.manager("Suspend");
  }

  /// Hibernates the machine, as logind's `Hibernate`.
  pub fn hibernate(&self) {
    self.manager("Hibernate");
  }

  /// Calls the manager's `method` with `interactive` true.
  fn manager(&self, method: &'static str) {
    if let Some(service) = &self.service {
      service.call(PATH, MANAGER, method, true);
    }
  }
}

/// One `delay_sleep_until`'s inhibitor.
struct Hold {
  /// The descriptor logind watches: closing it lets go.
  fd: RefCell<Option<OwnedFd>>,
  /// The generation of logind an `Inhibit` call under way went to.
  asking: Cell<Option<u64>>,
  /// Bumped when an inhibitor arrives, so one that arrives while the
  /// machine is about to sleep is let go as `ready` says.
  arrived: Signal<u32>,
}

impl Hold {
  /// Asks logind for an inhibitor, unless one is held or asked of the
  /// logind running now.
  fn take(self: &Rc<Self>, service: &Rc<Service>) {
    let Some((connection, generation)) = service.connection() else {
      return;
    };
    if self.fd.borrow().is_some() || self.asking.get() == Some(generation) {
      return;
    }
    self.asking.set(Some(generation));
    let (hold, service) = (Rc::downgrade(self), Rc::clone(service));
    let shared = Rc::clone(&service.shared);
    shared.spawn(async move {
      let why = "The shell gets ready for sleep";
      let body = ("sleep", "crabshell", why, "delay");
      let call = connection.call_method(Some(LOGIND), PATH, Some(MANAGER), "Inhibit", &body);
      let fd = service
        .shared
        .timeout(CALL_TIMEOUT, call)
        .await
        .and_then(|reply| reply.body().deserialize::<zbus::OwnedFd>());
      // Gone with its context, asked again of a logind that restarted,
      // or from one that went: the descriptor closes here.
      let Some(hold) = hold.upgrade() else {
        return;
      };
      if hold.asking.get() != Some(generation) {
        return;
      }
      hold.asking.set(None);
      if service.connection().map(|(_, now)| now) != Some(generation) {
        return;
      }
      match fd {
        Ok(fd) => {
          hold.fd.replace(Some(fd.into()));
          hold
            .arrived
            .try_update(|count| *count = count.wrapping_add(1));
        }
        Err(error) => tracing::warn!(%error, "logind gave no inhibitor, so sleep won't wait"),
      }
    });
  }

  /// Lets go of the inhibitor, if one is held.
  fn release(&self) {
    drop(self.fd.take());
  }
}

/// `cx.logind()`.
pub(crate) fn logind(cx: &Cx) -> Logind {
  match cx.service::<Service>() {
    Some(service) => Logind {
      sleeping: service.start(),
      service: Some(service),
      owner: cx.owner(),
    },
    None => {
      static LOGGED: Once = Once::new();
      LOGGED.call_once(|| {
        tracing::warn!("cx.logind() needs the services the shell provides; it does nothing");
      });
      Logind {
        service: None,
        sleeping: cx.signal(false).read_only(),
        owner: cx.owner(),
      }
    }
  }
}

/// logind for one shell, followed from the first ask.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  sleeping: Signal<bool>,
  /// The bus, while logind runs, for its methods.
  connection: Option<Connection>,
  /// Whether `connection` is there, for the delays waiting on it.
  connected: Signal<bool>,
  generation: u64,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  /// Starts following logind, once, and returns `preparing_for_sleep`.
  pub(crate) fn start(self: &Rc<Self>) -> ReadSignal<bool> {
    if let Some(state) = &*self.state.borrow() {
      return state.sleeping.read_only();
    }
    let sleeping = self.shared.root.signal(false);
    *self.state.borrow_mut() = Some(State {
      sleeping,
      connection: None,
      connected: self.shared.root.signal(false),
      generation: 0,
    });
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so logind can't be reached"),
    }
    sleeping.read_only()
  }

  /// Whether logind is reached, for what waits to call it.
  pub(crate) fn connected(&self) -> Option<Signal<bool>> {
    Some(self.signals()?.1)
  }

  fn signals(&self) -> Option<(Signal<bool>, Signal<bool>)> {
    let state = self.state.borrow();
    state
      .as_ref()
      .map(|state| (state.sleeping, state.connected))
  }

  /// The bus while logind runs, with the generation it runs in.
  pub(crate) fn connection(&self) -> Option<(Connection, u64)> {
    let state = self.state.borrow();
    let state = state.as_ref()?;
    Some((state.connection.clone()?, state.generation))
  }

  /// Calls logind's `method` on `interface` at `path` with `body`, and
  /// logs a failure.
  pub(crate) fn call<B>(&self, path: &'static str, interface: &'static str, method: &str, body: B)
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let Some((connection, _)) = self.connection() else {
      return tracing::info!(method, "logind isn't running, so nothing was asked of it");
    };
    let (shared, method) = (Rc::clone(&self.shared), method.to_owned());
    self.shared.spawn(async move {
      let call = connection.call_method(Some(LOGIND), path, Some(interface), &*method, &body);
      if let Err(error) = shared.timeout(CALL_TIMEOUT, call).await {
        tracing::warn!(%error, method, "logind refused");
      }
    });
  }

  /// Sets the backlight `name` to `value`, through the shell's session.
  pub(crate) fn set_brightness(&self, name: &str, value: u32) {
    let body = ("backlight".to_owned(), name.to_owned(), value);
    self.call(SESSION_PATH, SESSION, "SetBrightness", body);
  }

  fn publish(&self, sleeping: bool, connected: bool) {
    if let Some((sleeping_now, connected_now)) = self.signals() {
      set_changed(sleeping_now, sleeping);
      set_changed(connected_now, connected);
    }
  }
}

impl Daemon for Service {
  const NAME: &'static str = LOGIND;

  fn alive(&self) -> bool {
    let state = self.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.sleeping.try_with(|_| ()).is_some())
  }

  fn clear(&self) -> u64 {
    let generation = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      state.connection = None;
      state.generation += 1;
      state.generation
    };
    self.publish(false, false);
    generation
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let body = (MANAGER, "PreparingForSleep");
    let call = connection.call_method(Some(LOGIND), PATH, Some(PROPERTIES), "Get", &body);
    let sleeping = self
      .shared
      .timeout(CALL_TIMEOUT, call)
      .await
      .and_then(|reply| bool::try_from(&reply.body().deserialize::<zbus::OwnedVariant>()?));
    let sleeping = sleeping.unwrap_or_else(|error| {
      tracing::warn!(%error, "couldn't read whether logind prepares for sleep");
      false
    });
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state
        .as_mut()
        .filter(|state| state.generation == generation)
      else {
        return;
      };
      state.connection = Some(connection);
    }
    self.publish(sleeping, true);
  }

  fn signal(self: &Rc<Self>, _: &Connection, message: &Message) {
    if message.header().member().map(|member| member.as_str()) != Some("PrepareForSleep") {
      return;
    }
    match message.body().deserialize::<bool>() {
      Ok(sleeping) => {
        if let Some((signal, _)) = self.signals() {
          set_changed(signal, sleeping);
        }
      }
      Err(error) => tracing::warn!(%error, "a bad PrepareForSleep from logind"),
    }
  }

  /// `PrepareForSleep` only: logind broadcasts every session and user
  /// that comes and goes.
  fn rules() -> Vec<String> {
    vec![format!(
      "type='signal',sender='{LOGIND}',path='{PATH}',interface='{MANAGER}',member='PrepareForSleep'"
    )]
  }
}
