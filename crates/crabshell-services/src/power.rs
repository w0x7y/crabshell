//! Batteries and power supplies from UPower, and the power profile from
//! power-profiles-daemon, both on the system bus.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Once;
use std::time::Duration;

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::{Array, Connection, Message, OwnedObjectPath, OwnedVariant};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, Props};

const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DEVICE: &str = "org.freedesktop.UPower.Device";
/// The daemon's first name, which every version since still owns.
const PROFILES: &str = "net.hadess.PowerProfiles";
const PROFILES_PATH: &str = "/net/hadess/PowerProfiles";

/// The most devices kept: UPower lists one per battery, charger and
/// wireless gadget.
const MAX_DEVICES: usize = 64;

/// What a power device is, from UPower's `Type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceKind {
  /// Not known, or a kind this list leaves out.
  #[default]
  Unknown,
  /// A charger or other mains supply.
  LinePower,
  /// A laptop's battery.
  Battery,
  /// An uninterruptible power supply.
  Ups,
  /// A monitor.
  Monitor,
  /// A wireless mouse.
  Mouse,
  /// A wireless keyboard.
  Keyboard,
  /// A phone.
  Phone,
  /// A tablet.
  Tablet,
  /// A game controller.
  GamingInput,
  /// A headset.
  Headset,
  /// Headphones.
  Headphones,
  /// Speakers.
  Speakers,
}

/// How a battery is doing, from UPower's `State`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BatteryState {
  /// Not known, or not a battery.
  #[default]
  Unknown,
  /// Charging.
  Charging,
  /// Running the machine.
  Discharging,
  /// Empty.
  Empty,
  /// Full, and on the charger.
  FullyCharged,
  /// Waiting to charge.
  PendingCharge,
  /// Waiting to run the machine.
  PendingDischarge,
}

/// A battery or power supply UPower knows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PowerDevice {
  /// UPower's object path for it, which names it while it exists.
  pub path: String,
  /// What it is.
  pub kind: DeviceKind,
  /// Its model name, or `""`.
  pub model: String,
  /// The icon UPower suggests, such as `battery-good-charging-symbolic`.
  pub icon_name: String,
  /// Whether a battery is in its bay.
  pub present: bool,
  /// Whether it powers this machine, rather than a gadget.
  pub power_supply: bool,
  /// How it is doing.
  pub state: BatteryState,
  /// How full it is, from 0 to 100.
  pub percent: f64,
  /// How long until it runs out, if UPower knows.
  pub time_to_empty: Option<Duration>,
  /// How long until it is full, if UPower knows.
  pub time_to_full: Option<Duration>,
  /// The power going in or out, in watts.
  pub energy_rate: f64,
}

impl PowerDevice {
  fn from_props(path: String, props: &Props) -> PowerDevice {
    let seconds = |name| {
      dbus::get::<i64>(props, name)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Duration::from_secs(seconds as u64))
    };
    PowerDevice {
      path,
      kind: match dbus::get::<u32>(props, "Type") {
        Some(1) => DeviceKind::LinePower,
        Some(2) => DeviceKind::Battery,
        Some(3) => DeviceKind::Ups,
        Some(4) => DeviceKind::Monitor,
        Some(5) => DeviceKind::Mouse,
        Some(6) => DeviceKind::Keyboard,
        Some(8) => DeviceKind::Phone,
        Some(10) => DeviceKind::Tablet,
        Some(12) => DeviceKind::GamingInput,
        Some(17) => DeviceKind::Headset,
        Some(18) => DeviceKind::Speakers,
        Some(19) => DeviceKind::Headphones,
        _ => DeviceKind::Unknown,
      },
      model: dbus::text(props, "Model"),
      icon_name: dbus::text(props, "IconName"),
      present: dbus::get(props, "IsPresent").unwrap_or(false),
      power_supply: dbus::get(props, "PowerSupply").unwrap_or(false),
      state: match dbus::get::<u32>(props, "State") {
        Some(1) => BatteryState::Charging,
        Some(2) => BatteryState::Discharging,
        Some(3) => BatteryState::Empty,
        Some(4) => BatteryState::FullyCharged,
        Some(5) => BatteryState::PendingCharge,
        Some(6) => BatteryState::PendingDischarge,
        _ => BatteryState::Unknown,
      },
      percent: dbus::get::<f64>(props, "Percentage")
        .filter(|percent| percent.is_finite())
        .map_or(0.0, |percent| percent.clamp(0.0, 100.0)),
      time_to_empty: seconds("TimeToEmpty"),
      time_to_full: seconds("TimeToFull"),
      energy_rate: dbus::get::<f64>(props, "EnergyRate")
        .filter(|rate| rate.is_finite())
        .unwrap_or(0.0),
    }
  }
}

/// UPower's batteries and power supplies, from `cx.upower()`. Cloning it
/// is cheap.
#[derive(Clone)]
pub struct UPower {
  devices: ReadSignal<Vec<PowerDevice>>,
  display: ReadSignal<Option<PowerDevice>>,
  on_battery: ReadSignal<bool>,
}

impl UPower {
  /// Every device, in the order UPower lists them. Empty while UPower
  /// isn't running.
  pub fn devices(&self) -> ReadSignal<Vec<PowerDevice>> {
    self.devices
  }

  /// The one device that sums up the machine's batteries, as a battery
  /// icon in a bar shows. On a machine with none, it isn't `present`.
  /// `None` while UPower isn't running.
  pub fn display_device(&self) -> ReadSignal<Option<PowerDevice>> {
    self.display
  }

  /// Whether the machine runs on battery.
  pub fn on_battery(&self) -> ReadSignal<bool> {
    self.on_battery
  }
}

/// `cx.upower()`.
pub(crate) fn upower(cx: &Cx) -> UPower {
  let Some(service) = cx.service::<UPowerService>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.upower() needs the services the shell provides; it stays empty");
    });
    return UPower {
      devices: cx.signal(Vec::new()).read_only(),
      display: cx.signal(None).read_only(),
      on_battery: cx.signal(false).read_only(),
    };
  };
  service.start()
}

/// UPower for one shell, followed from the first ask.
pub(crate) struct UPowerService {
  shared: Rc<Shared>,
  state: RefCell<Option<UPowerState>>,
}

struct UPowerState {
  devices: Signal<Vec<PowerDevice>>,
  display: Signal<Option<PowerDevice>>,
  on_battery: Signal<bool>,
  /// Each device's properties, in UPower's order.
  objects: Vec<(String, Props)>,
  /// The display device's path and properties.
  display_device: Option<(String, Props)>,
  generation: u64,
}

impl UPowerService {
  pub(crate) fn new(shared: Rc<Shared>) -> UPowerService {
    UPowerService {
      shared,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> UPower {
    if let Some(state) = &*self.state.borrow() {
      return UPower {
        devices: state.devices.read_only(),
        display: state.display.read_only(),
        on_battery: state.on_battery.read_only(),
      };
    }
    let root = &self.shared.root;
    let state = UPowerState {
      devices: root.signal(Vec::new()),
      display: root.signal(None),
      on_battery: root.signal(false),
      objects: Vec::new(),
      display_device: None,
      generation: 0,
    };
    let upower = UPower {
      devices: state.devices.read_only(),
      display: state.display.read_only(),
      on_battery: state.on_battery.read_only(),
    };
    *self.state.borrow_mut() = Some(state);
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so UPower's devices stay empty"),
    }
    upower
  }

  /// Shows the devices as they are now.
  fn publish(&self) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let devices = state
        .objects
        .iter()
        .map(|(path, props)| PowerDevice::from_props(path.clone(), props))
        .collect::<Vec<_>>();
      let display = state
        .display_device
        .as_ref()
        .map(|(path, props)| PowerDevice::from_props(path.clone(), props));
      (state.devices, devices, state.display, display)
    });
    if let Some((devices_signal, devices, display_signal, display)) = shown {
      set_changed(devices_signal, devices);
      set_changed(display_signal, display);
    }
  }
}

impl Daemon for UPowerService {
  const NAME: &'static str = UPOWER;

  fn alive(&self) -> bool {
    let state = self.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.devices.try_with(|_| ()).is_some())
  }

  fn clear(&self) -> u64 {
    let (devices, display, on_battery, generation) = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      state.objects.clear();
      state.display_device = None;
      state.generation += 1;
      (
        state.devices,
        state.display,
        state.on_battery,
        state.generation,
      )
    };
    set_changed(devices, Vec::new());
    set_changed(display, None);
    set_changed(on_battery, false);
    generation
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let shared = &self.shared;
    let read = async {
      let root = dbus::get_all(shared, &connection, UPOWER, UPOWER_PATH, UPOWER).await?;
      let call = connection.call_method(
        Some(UPOWER),
        UPOWER_PATH,
        Some(UPOWER),
        "EnumerateDevices",
        &(),
      );
      let paths: Vec<OwnedObjectPath> = shared
        .timeout(CALL_TIMEOUT, call)
        .await?
        .body()
        .deserialize()?;
      let call = connection.call_method(
        Some(UPOWER),
        UPOWER_PATH,
        Some(UPOWER),
        "GetDisplayDevice",
        &(),
      );
      let display: OwnedObjectPath = shared
        .timeout(CALL_TIMEOUT, call)
        .await?
        .body()
        .deserialize()?;
      let mut objects = Vec::new();
      for path in paths.into_iter().take(MAX_DEVICES) {
        let path = path.as_str().to_owned();
        match dbus::get_all(shared, &connection, UPOWER, &path, DEVICE).await {
          Ok(props) => objects.push((path, props)),
          // One device gone between the list and its read.
          Err(error) => tracing::info!(%error, path, "couldn't read a power device"),
        }
      }
      let display_path = display.as_str().to_owned();
      let display = dbus::get_all(shared, &connection, UPOWER, &display_path, DEVICE)
        .await
        .ok()
        .map(|props| (display_path, props));
      zbus::Result::Ok((root, objects, display))
    };
    let (root, objects, display) = match read.await {
      Ok(read) => read,
      Err(error) => return tracing::warn!(%error, "couldn't read UPower"),
    };
    let on_battery = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state
        .as_mut()
        .filter(|state| state.generation == generation)
      else {
        return;
      };
      state.objects = objects;
      state.display_device = display;
      state.on_battery
    };
    set_changed(on_battery, dbus::get(&root, "OnBattery").unwrap_or(false));
    self.publish();
  }

  /// A device added or removed: UPower names it without its properties.
  fn reads_again(&self, message: &Message) -> bool {
    let header = message.header();
    header.interface().map(|interface| interface.as_str()) == Some(UPOWER)
      && matches!(
        header.member().map(|member| member.as_str()),
        Some("DeviceAdded" | "DeviceRemoved")
      )
  }

  fn signal(self: &Rc<Self>, _: &Connection, message: &Message) {
    let Some((path, interface, changed, dropped)) = dbus::properties_changed(message) else {
      return;
    };
    {
      if path == UPOWER_PATH && interface == UPOWER {
        let on_battery = self.state.borrow().as_ref().map(|state| state.on_battery);
        if let (Some(signal), Some(value)) = (on_battery, changed.get("OnBattery")) {
          set_changed(signal, bool::try_from(value).unwrap_or(false));
        }
        return;
      }
      if interface != DEVICE {
        return;
      }
      {
        let mut state = self.state.borrow_mut();
        let Some(state) = state.as_mut() else {
          return;
        };
        let display = state.display_device.iter_mut();
        let object = state
          .objects
          .iter_mut()
          .chain(display)
          .find(|(at, _)| *at == path);
        let Some((_, props)) = object else {
          return;
        };
        dbus::apply(props, changed, dropped);
      }
    }
    self.publish();
  }
}

/// A power profile power-profiles-daemon can set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerProfile {
  /// Saves battery at the cost of speed.
  PowerSaver,
  /// The default.
  Balanced,
  /// Fast, at the cost of battery and heat.
  Performance,
}

impl PowerProfile {
  fn parse(name: &str) -> Option<PowerProfile> {
    match name {
      "power-saver" => Some(PowerProfile::PowerSaver),
      "balanced" => Some(PowerProfile::Balanced),
      "performance" => Some(PowerProfile::Performance),
      _ => None,
    }
  }

  fn name(self) -> &'static str {
    match self {
      PowerProfile::PowerSaver => "power-saver",
      PowerProfile::Balanced => "balanced",
      PowerProfile::Performance => "performance",
    }
  }
}

/// The power profile, from `cx.power_profiles()`. Cloning it is cheap.
#[derive(Clone)]
pub struct PowerProfiles {
  service: Option<Rc<ProfilesService>>,
  active: ReadSignal<Option<PowerProfile>>,
  available: ReadSignal<Vec<PowerProfile>>,
}

impl PowerProfiles {
  /// The profile in effect. `None` while power-profiles-daemon isn't
  /// running, or in a profile this list doesn't know.
  pub fn active(&self) -> ReadSignal<Option<PowerProfile>> {
    self.active
  }

  /// The profiles this machine offers, in the daemon's order. Laptops
  /// without a performance mode leave it out.
  pub fn available(&self) -> ReadSignal<Vec<PowerProfile>> {
    self.available
  }

  /// Asks the daemon to switch to `profile`. It shows in [`active`]
  /// once the daemon says it did. A refusal, such as one polkit makes,
  /// is logged.
  ///
  /// [`active`]: Self::active
  pub fn set(&self, profile: PowerProfile) {
    let Some(service) = &self.service else {
      return;
    };
    let Some(connection) = service
      .state
      .borrow()
      .as_ref()
      .and_then(|s| s.connection.clone())
    else {
      return tracing::info!("power-profiles-daemon isn't running; no profile set");
    };
    let shared = Rc::clone(&service.shared);
    service.shared.spawn(async move {
      let value = zbus::Variant::from(profile.name());
      let at = (PROFILES_PATH, PROFILES);
      let set = dbus::set(&shared, &connection, PROFILES, at, "ActiveProfile", value);
      if let Err(error) = set.await {
        tracing::warn!(%error, profile = profile.name(), "the power profile wasn't set");
      }
    });
  }
}

/// `cx.power_profiles()`.
pub(crate) fn power_profiles(cx: &Cx) -> PowerProfiles {
  let Some(service) = cx.service::<ProfilesService>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.power_profiles() needs the services the shell provides; it stays empty");
    });
    return PowerProfiles {
      service: None,
      active: cx.signal(None).read_only(),
      available: cx.signal(Vec::new()).read_only(),
    };
  };
  service.start()
}

/// power-profiles-daemon for one shell, followed from the first ask.
pub(crate) struct ProfilesService {
  shared: Rc<Shared>,
  state: RefCell<Option<ProfilesState>>,
}

struct ProfilesState {
  active: Signal<Option<PowerProfile>>,
  available: Signal<Vec<PowerProfile>>,
  props: Props,
  /// The bus, while the daemon runs, for `set`.
  connection: Option<Connection>,
  generation: u64,
}

impl ProfilesService {
  pub(crate) fn new(shared: Rc<Shared>) -> ProfilesService {
    ProfilesService {
      shared,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> PowerProfiles {
    if let Some(state) = &*self.state.borrow() {
      return PowerProfiles {
        service: Some(Rc::clone(self)),
        active: state.active.read_only(),
        available: state.available.read_only(),
      };
    }
    let root = &self.shared.root;
    let (active, available) = (root.signal(None), root.signal(Vec::new()));
    *self.state.borrow_mut() = Some(ProfilesState {
      active,
      available,
      props: Props::new(),
      connection: None,
      generation: 0,
    });
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so there is no power profile"),
    }
    PowerProfiles {
      service: Some(Rc::clone(self)),
      active: active.read_only(),
      available: available.read_only(),
    }
  }

  fn publish(&self) {
    let shown = self.state.borrow().as_ref().map(|state| {
      let active = dbus::get::<&str>(&state.props, "ActiveProfile").and_then(PowerProfile::parse);
      let available = state.props.get("Profiles").map(profiles);
      (
        state.active,
        active,
        state.available,
        available.unwrap_or_default(),
      )
    });
    if let Some((active_signal, active, available_signal, available)) = shown {
      set_changed(active_signal, active);
      set_changed(available_signal, available);
    }
  }
}

/// The profiles in the daemon's `Profiles`, an `aa{sv}` whose dicts each
/// name one in `Profile`.
fn profiles(value: &OwnedVariant) -> Vec<PowerProfile> {
  let Ok(list) = <&Array>::try_from(value) else {
    return Vec::new();
  };
  list
    .inner()
    .iter()
    .filter_map(|entry| {
      let zbus::Variant::Dict(dict) = entry else {
        return None;
      };
      let name = dict.iter().find_map(|(key, value)| {
        if <&str>::try_from(key).ok()? != "Profile" {
          return None;
        }
        let value = match value {
          zbus::Variant::Variant(inner) => &**inner,
          other => other,
        };
        <&str>::try_from(value).ok()
      })?;
      PowerProfile::parse(name)
    })
    .collect()
}

impl Daemon for ProfilesService {
  const NAME: &'static str = PROFILES;

  fn alive(&self) -> bool {
    let state = self.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.active.try_with(|_| ()).is_some())
  }

  fn clear(&self) -> u64 {
    let generation = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      state.props.clear();
      state.connection = None;
      state.generation += 1;
      state.generation
    };
    self.publish();
    generation
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let read = dbus::get_all(&self.shared, &connection, PROFILES, PROFILES_PATH, PROFILES).await;
    let props = match read {
      Ok(props) => props,
      Err(error) => return tracing::warn!(%error, "couldn't read power-profiles-daemon"),
    };
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state
        .as_mut()
        .filter(|state| state.generation == generation)
      else {
        return;
      };
      state.props = props;
      state.connection = Some(connection);
    }
    self.publish();
  }

  fn signal(self: &Rc<Self>, _: &Connection, message: &Message) {
    let Some((path, interface, changed, dropped)) = dbus::properties_changed(message) else {
      return;
    };
    if path != PROFILES_PATH || interface != PROFILES {
      return;
    }
    if let Some(state) = self.state.borrow_mut().as_mut() {
      dbus::apply(&mut state.props, changed, dropped);
    }
    self.publish();
  }
}

/// Sets `signal` to `value` unless it already holds it, so nothing that
/// reads it runs for nothing. A disposed signal is left alone.
fn set_changed<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if signal.try_with(|old| *old != value) == Some(true) {
    signal.try_set(value);
  }
}
