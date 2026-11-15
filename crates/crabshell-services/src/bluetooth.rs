//! Bluetooth adapters and devices from BlueZ, on the system bus.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Once;

use crabshell_reactive::{ReadSignal, Signal};
use crabshell_ui::Cx;
use zbus::{Connection, Message, ObjectPath, OwnedObjectPath, Variant};

use crate::Shared;
use crate::dbus::{self, CALL_TIMEOUT, Daemon, OBJECT_MANAGER, PROPERTIES, Props, set_changed};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";

/// The most adapters shown: one per radio.
const MAX_ADAPTERS: usize = 16;
/// The most devices shown: discovery adds every device in range.
const MAX_DEVICES: usize = 256;
/// The most objects kept, so a flood can't take the memory.
const MAX_OBJECTS: usize = 4096;
/// How long `Pair` and `Connect` may take: BlueZ answers a pairing only
/// once the user confirmed it in the agent's prompt.
const SLOW_CALL: std::time::Duration = std::time::Duration::from_secs(90);

/// The properties this service reads, by interface: a change to any
/// other, such as a device's signal strength during discovery, shows
/// nothing new.
fn read(interface: &str, name: &str) -> bool {
  match interface {
    ADAPTER => matches!(
      name,
      "Alias" | "Address" | "Powered" | "PowerState" | "Discovering"
    ),
    DEVICE => matches!(
      name,
      "Address" | "Alias" | "Name" | "Icon" | "Connected" | "Paired" | "Trusted" | "Adapter"
    ),
    BATTERY => name == "Percentage",
    _ => false,
  }
}

/// Whether an adapter is on, from BlueZ's `PowerState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothAdapterState {
  /// On.
  Enabled,
  /// Off.
  Disabled,
  /// Turning on.
  Enabling,
  /// Turning off.
  Disabling,
  /// Off, and held off by rfkill, as by a laptop's airplane mode.
  Blocked,
}

/// Whether a device is connected, from BlueZ's `Connected` and the calls
/// made through [`Bluetooth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BluetoothDeviceState {
  /// Not connected.
  Disconnected,
  /// A [`Bluetooth::connect`] call is out.
  Connecting,
  /// Connected.
  Connected,
  /// A [`Bluetooth::disconnect`] call is out.
  Disconnecting,
}

/// A Bluetooth radio BlueZ knows.
#[derive(Clone, Debug, PartialEq)]
pub struct BluetoothAdapter {
  /// BlueZ's object path for it, such as `/org/bluez/hci0`.
  pub path: String,
  /// Its name, BlueZ's `Alias`.
  pub name: String,
  /// Its address, such as `00:1A:7D:DA:71:13`.
  pub address: String,
  /// Whether it is on.
  pub powered: bool,
  /// Whether it is on, off or on the way. An older BlueZ, without
  /// `PowerState`, says only `Enabled` or `Disabled`, from
  /// [`powered`](Self::powered).
  pub state: BluetoothAdapterState,
  /// Whether it is looking for devices.
  pub discovering: bool,
}

impl BluetoothAdapter {
  fn from_props(path: String, props: &Props) -> BluetoothAdapter {
    let powered = dbus::get(props, "Powered").unwrap_or(false);
    let state = match dbus::get::<&str>(props, "PowerState") {
      Some("on") => BluetoothAdapterState::Enabled,
      Some("off") => BluetoothAdapterState::Disabled,
      Some("off-enabling") => BluetoothAdapterState::Enabling,
      Some("on-disabling") => BluetoothAdapterState::Disabling,
      Some("off-blocked") => BluetoothAdapterState::Blocked,
      unknown => {
        if let Some(unknown) = unknown {
          tracing::debug!(
            path,
            state = unknown,
            "an unknown power state; read from Powered"
          );
        }
        if powered {
          BluetoothAdapterState::Enabled
        } else {
          BluetoothAdapterState::Disabled
        }
      }
    };
    BluetoothAdapter {
      name: dbus::text(props, "Alias"),
      address: dbus::text(props, "Address"),
      powered,
      state,
      discovering: dbus::get(props, "Discovering").unwrap_or(false),
      path,
    }
  }
}

/// A device BlueZ knows: one paired before, or one discovery found.
#[derive(Clone, Debug, PartialEq)]
pub struct BluetoothDevice {
  /// BlueZ's object path for it, which names it while it exists.
  pub path: String,
  /// The path of the adapter it is reached through.
  pub adapter: String,
  /// Its address, such as `AA:BB:CC:DD:EE:01`.
  pub address: String,
  /// Its `Alias`, else its `Name`, else its address.
  pub name: String,
  /// The icon BlueZ suggests, such as `audio-headset`, or `""`.
  pub icon: String,
  /// Whether it is connected.
  pub connected: bool,
  /// Whether it is connected, or on the way while a call made through
  /// [`Bluetooth`] is out.
  pub state: BluetoothDeviceState,
  /// Whether it is paired.
  pub paired: bool,
  /// Whether it may connect without asking.
  pub trusted: bool,
  /// How full its battery is, from 0 to 100, if it says.
  pub battery: Option<u8>,
}

impl BluetoothDevice {
  fn from_props(
    path: String,
    props: &Props,
    battery: Option<&Props>,
    call: Option<BluetoothDeviceState>,
  ) -> BluetoothDevice {
    let address = dbus::text(props, "Address");
    let connected = dbus::get(props, "Connected").unwrap_or(false);
    let state = match (call, connected) {
      (Some(call), _) => call,
      (None, true) => BluetoothDeviceState::Connected,
      (None, false) => BluetoothDeviceState::Disconnected,
    };
    let name = [dbus::text(props, "Alias"), dbus::text(props, "Name")]
      .into_iter()
      .find(|name| !name.is_empty())
      .unwrap_or_else(|| address.clone());
    BluetoothDevice {
      path,
      adapter: dbus::get::<&ObjectPath>(props, "Adapter")
        .map_or(String::new(), |at| at.to_string()),
      address,
      name,
      icon: dbus::text(props, "Icon"),
      connected,
      state,
      paired: dbus::get(props, "Paired").unwrap_or(false),
      trusted: dbus::get(props, "Trusted").unwrap_or(false),
      battery: battery
        .and_then(|props| dbus::get::<u8>(props, "Percentage"))
        .map(|percent| percent.min(100)),
    }
  }
}

/// Bluetooth from BlueZ, from `cx.bluetooth()`. Cloning it is cheap.
#[derive(Clone)]
pub struct Bluetooth {
  service: Option<Rc<BluetoothService>>,
  adapters: ReadSignal<Vec<BluetoothAdapter>>,
  devices: ReadSignal<Vec<BluetoothDevice>>,
}

impl Bluetooth {
  /// Every adapter, in path order. Empty while BlueZ isn't running.
  pub fn adapters(&self) -> ReadSignal<Vec<BluetoothAdapter>> {
    self.adapters
  }

  /// Every device BlueZ knows, in path order. Empty while BlueZ isn't
  /// running.
  pub fn devices(&self) -> ReadSignal<Vec<BluetoothDevice>> {
    self.devices
  }

  /// Turns the adapter at `adapter`, its path, on or off.
  pub fn set_powered(&self, adapter: &str, powered: bool) {
    let body = (ADAPTER, "Powered", Variant::from(powered));
    self.call(adapter, PROPERTIES, "Set", body);
  }

  /// Starts looking for devices. Found ones join [`devices`] until BlueZ
  /// forgets them, a while after [`stop_discovery`].
  ///
  /// [`devices`]: Self::devices
  /// [`stop_discovery`]: Self::stop_discovery
  pub fn start_discovery(&self, adapter: &str) {
    self.call(adapter, ADAPTER, "StartDiscovery", ());
  }

  /// Stops looking for devices.
  pub fn stop_discovery(&self, adapter: &str) {
    self.call(adapter, ADAPTER, "StopDiscovery", ());
  }

  /// Connects the device at `device`, its path. Its state is
  /// [`Connecting`](BluetoothDeviceState::Connecting) until BlueZ answers
  /// or says it connected or disconnected.
  pub fn connect(&self, device: &str) {
    self.call(device, DEVICE, "Connect", ());
  }

  /// Disconnects the device at `device`, which is
  /// [`Disconnecting`](BluetoothDeviceState::Disconnecting) as
  /// [`connect`](Self::connect) is connecting.
  pub fn disconnect(&self, device: &str) {
    self.call(device, DEVICE, "Disconnect", ());
  }

  /// Pairs with the device at `device`. The shell answers no pairing
  /// questions, so a device that asks for a PIN or a confirmation fails,
  /// and that is logged, unless another program on the desktop answers
  /// for BlueZ.
  pub fn pair(&self, device: &str) {
    self.call(device, DEVICE, "Pair", ());
  }

  /// Lets the device at `device` connect without asking, or not.
  pub fn set_trusted(&self, device: &str, trusted: bool) {
    let body = (DEVICE, "Trusted", Variant::from(trusted));
    self.call(device, PROPERTIES, "Set", body);
  }

  /// Unpairs the device at `device` and has its adapter forget it.
  pub fn forget(&self, device: &str) {
    let adapter = self.service.as_ref().and_then(|service| {
      let state = service.state.borrow();
      let object = state.as_ref()?.objects.get(device)?;
      dbus::get::<&ObjectPath>(object.device.as_ref()?, "Adapter").map(|at| at.to_string())
    });
    let (Some(adapter), Ok(path)) = (adapter, ObjectPath::try_from(device.to_owned())) else {
      return tracing::info!(device, "no such Bluetooth device; nothing forgotten");
    };
    self.call(&adapter, ADAPTER, "RemoveDevice", path);
  }

  /// Calls `method` on BlueZ's object at `path`, and logs a refusal.
  fn call<B>(&self, path: &str, interface: &'static str, method: &'static str, body: B)
  where
    B: serde::Serialize + zbus::DynamicType + 'static,
  {
    let Some(service) = &self.service else {
      return;
    };
    let Some(connection) = service
      .state
      .borrow()
      .as_ref()
      .and_then(|state| state.connection.clone())
    else {
      return tracing::info!(method, "BlueZ isn't running; nothing sent");
    };
    let marked = match method {
      "Connect" => Some(service.mark(path, BluetoothDeviceState::Connecting)),
      "Disconnect" => Some(service.mark(path, BluetoothDeviceState::Disconnecting)),
      _ => None,
    };
    let (shared, service, path) = (&service.shared, Rc::clone(service), path.to_owned());
    shared.spawn(async move {
      let call = connection.call_method(Some(BLUEZ), path.as_str(), Some(interface), method, &body);
      let limit = if matches!(method, "Pair" | "Connect") {
        SLOW_CALL
      } else {
        CALL_TIMEOUT
      };
      let answer = service.shared.timeout(limit, call).await;
      if let Some(number) = marked {
        service.unmark(&path, number);
      }
      if let Err(error) = answer {
        tracing::warn!(%error, path, method, "BlueZ refused it");
      }
    });
  }
}

/// `cx.bluetooth()`.
pub(crate) fn bluetooth(cx: &Cx) -> Bluetooth {
  let Some(service) = cx.service::<BluetoothService>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.bluetooth() needs the services the shell provides; it stays empty");
    });
    return Bluetooth {
      service: None,
      adapters: cx.signal(Vec::new()).read_only(),
      devices: cx.signal(Vec::new()).read_only(),
    };
  };
  service.start()
}

/// BlueZ for one shell, followed from the first ask.
pub(crate) struct BluetoothService {
  shared: Rc<Shared>,
  state: RefCell<Option<BluetoothState>>,
}

struct BluetoothState {
  adapters: Signal<Vec<BluetoothAdapter>>,
  devices: Signal<Vec<BluetoothDevice>>,
  /// BlueZ's adapters and devices, by path.
  objects: BTreeMap<String, Object>,
  /// The bus, while BlueZ runs, for the methods.
  connection: Option<Connection>,
  generation: u64,
  /// The devices a connect or disconnect call is out for, by path, with
  /// the call's number, so the answer to one that another replaced
  /// changes nothing.
  calls: HashMap<String, (BluetoothDeviceState, u64)>,
  /// The number the next such call gets.
  next_call: u64,
}

/// The interfaces this service reads on one of BlueZ's objects. An
/// object is kept while it is an adapter or a device.
#[derive(Default)]
struct Object {
  adapter: Option<Props>,
  device: Option<Props>,
  battery: Option<Props>,
}

impl Object {
  /// Where `interface`'s properties go, or `None` for one this service
  /// doesn't read.
  fn slot(&mut self, interface: &str) -> Option<&mut Option<Props>> {
    match interface {
      ADAPTER => Some(&mut self.adapter),
      DEVICE => Some(&mut self.device),
      BATTERY => Some(&mut self.battery),
      _ => None,
    }
  }
}

/// Gives the object at `path` `interfaces`. Past [`MAX_OBJECTS`] a new
/// one is left out; which are shown is decided as they are.
fn add(objects: &mut BTreeMap<String, Object>, path: String, interfaces: HashMap<String, Props>) {
  if objects.len() >= MAX_OBJECTS && !objects.contains_key(&path) {
    return tracing::debug!(path, "too many Bluetooth objects; leaving one out");
  }
  let mut object = objects.remove(&path).unwrap_or_default();
  for (interface, props) in interfaces {
    if let Some(slot) = object.slot(&interface) {
      *slot = Some(props);
    }
  }
  if object.adapter.is_some() || object.device.is_some() {
    objects.insert(path, object);
  }
}

impl BluetoothService {
  pub(crate) fn new(shared: Rc<Shared>) -> BluetoothService {
    BluetoothService {
      shared,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> Bluetooth {
    if let Some(state) = &*self.state.borrow() {
      return Bluetooth {
        service: Some(Rc::clone(self)),
        adapters: state.adapters.read_only(),
        devices: state.devices.read_only(),
      };
    }
    let root = &self.shared.root;
    let (adapters, devices) = (root.signal(Vec::new()), root.signal(Vec::new()));
    *self.state.borrow_mut() = Some(BluetoothState {
      adapters,
      devices,
      objects: BTreeMap::new(),
      connection: None,
      generation: 0,
      calls: HashMap::new(),
      next_call: 0,
    });
    match self.shared.config.system_bus.clone() {
      Some(address) => {
        let follow = dbus::follow(Rc::clone(&self.shared), Rc::clone(self), address);
        self.shared.spawn(follow);
      }
      None => tracing::info!("no system bus, so there is no Bluetooth"),
    }
    Bluetooth {
      service: Some(Rc::clone(self)),
      adapters: adapters.read_only(),
      devices: devices.read_only(),
    }
  }

  /// Shows the adapters and devices as they are now.
  fn publish(&self) {
    // Set apart from the borrow, since effects may ask for more.
    let shown = self.state.borrow().as_ref().map(|state| {
      let objects = || {
        state
          .objects
          .iter()
          .map(|(path, object)| (path.clone(), object))
      };
      let adapters = objects()
        .filter_map(|(path, object)| {
          Some(BluetoothAdapter::from_props(path, object.adapter.as_ref()?))
        })
        .take(MAX_ADAPTERS)
        .collect::<Vec<_>>();
      let mut devices = objects()
        .filter_map(|(path, object)| {
          let device = object.device.as_ref()?;
          let call = state.calls.get(&path).map(|(call, _)| *call);
          Some(BluetoothDevice::from_props(
            path,
            device,
            object.battery.as_ref(),
            call,
          ))
        })
        .collect::<Vec<_>>();
      // Past the cap, connected and paired ones first, so the devices a
      // person uses never hide behind strangers discovery found.
      if devices.len() > MAX_DEVICES {
        devices.sort_by_key(|device| (!device.connected, !device.paired));
        devices.truncate(MAX_DEVICES);
        devices.sort_by(|a, b| a.path.cmp(&b.path));
      }
      (state.adapters, adapters, state.devices, devices)
    });
    if let Some((adapters_signal, adapters, devices_signal, devices)) = shown {
      set_changed(adapters_signal, adapters);
      set_changed(devices_signal, devices);
    }
  }

  /// Shows `device` in `state` while a call is out for it. The call's
  /// number.
  fn mark(&self, device: &str, state: BluetoothDeviceState) -> u64 {
    let number = {
      let mut held = self.state.borrow_mut();
      let Some(held) = held.as_mut() else {
        return 0;
      };
      held.next_call += 1;
      held
        .calls
        .insert(device.to_owned(), (state, held.next_call));
      held.next_call
    };
    self.publish();
    number
  }

  /// Ends call `number` for `device`, which BlueZ answered, unless another
  /// call or a change of `Connected` ended it already.
  fn unmark(&self, device: &str, number: u64) {
    let ended = {
      let mut state = self.state.borrow_mut();
      let Some(calls) = state.as_mut().map(|state| &mut state.calls) else {
        return;
      };
      let current = calls.get(device).is_some_and(|(_, at)| *at == number);
      current && calls.remove(device).is_some()
    };
    if ended {
      self.publish();
    }
  }
}

impl Daemon for BluetoothService {
  const NAME: &'static str = BLUEZ;

  /// Only what adapters and devices are: BlueZ also broadcasts media,
  /// GATT values and other programs' notifications, which would each
  /// wake the shell for nothing.
  fn rules() -> Vec<String> {
    let mut rules = vec![format!(
      "type='signal',sender='{BLUEZ}',interface='{OBJECT_MANAGER}'"
    )];
    rules.extend([ADAPTER, DEVICE, BATTERY].map(|interface| {
      format!(
        "type='signal',sender='{BLUEZ}',interface='{PROPERTIES}',member='PropertiesChanged',\
         arg0='{interface}'"
      )
    }));
    rules
  }

  fn alive(&self) -> bool {
    let state = self.state.borrow();
    state
      .as_ref()
      .is_some_and(|state| state.adapters.try_with(|_| ()).is_some())
  }

  fn clear(&self) -> u64 {
    let generation = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return 0;
      };
      state.objects.clear();
      state.calls.clear();
      state.connection = None;
      state.generation += 1;
      state.generation
    };
    self.publish();
    generation
  }

  async fn read(self: Rc<Self>, connection: Connection, generation: u64) {
    let call = connection.call_method(
      Some(BLUEZ),
      "/",
      Some(OBJECT_MANAGER),
      "GetManagedObjects",
      &(),
    );
    let read = async {
      let reply = self.shared.timeout(CALL_TIMEOUT, call).await?;
      reply
        .body()
        .deserialize::<HashMap<OwnedObjectPath, HashMap<String, Props>>>()
    };
    let managed = match read.await {
      Ok(managed) => managed,
      Err(error) => return tracing::warn!(%error, "couldn't read BlueZ"),
    };
    // In path order, so the caps keep the same objects every time.
    let managed = managed
      .into_iter()
      .map(|(path, interfaces)| (path.as_str().to_owned(), interfaces))
      .collect::<BTreeMap<_, _>>();
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state
        .as_mut()
        .filter(|state| state.generation == generation)
      else {
        return;
      };
      state.objects.clear();
      for (path, interfaces) in managed {
        add(&mut state.objects, path, interfaces);
      }
      state.connection = Some(connection);
    }
    self.publish();
  }

  fn signal(self: &Rc<Self>, _: &Connection, message: &Message) {
    if let Some((path, interface, changed, dropped)) = dbus::properties_changed(message) {
      {
        let mut state = self.state.borrow_mut();
        let Some(state) = state.as_mut() else {
          return;
        };
        // It can come before the answer, and ends the call either way.
        if interface == DEVICE && changed.contains_key("Connected") {
          state.calls.remove(&path);
        }
        let props = state
          .objects
          .get_mut(&path)
          .and_then(|object| object.slot(&interface)?.as_mut());
        let Some(props) = props else {
          return;
        };
        let shown = changed
          .keys()
          .chain(&dropped)
          .any(|name| read(&interface, name));
        dbus::apply(props, changed, dropped);
        if !shown {
          return;
        }
      }
      return self.publish();
    }
    let header = message.header();
    if header.interface().map(|interface| interface.as_str()) != Some(OBJECT_MANAGER) {
      return;
    }
    let body = message.body();
    {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      match header.member().map(|member| member.as_str()) {
        Some("InterfacesAdded") => {
          let Ok((path, interfaces)) =
            body.deserialize::<(OwnedObjectPath, HashMap<String, Props>)>()
          else {
            return;
          };
          add(&mut state.objects, path.as_str().to_owned(), interfaces);
        }
        Some("InterfacesRemoved") => {
          let Ok((path, interfaces)) = body.deserialize::<(OwnedObjectPath, Vec<String>)>() else {
            return;
          };
          let Some(object) = state.objects.get_mut(path.as_str()) else {
            return;
          };
          for interface in interfaces {
            if let Some(slot) = object.slot(&interface) {
              *slot = None;
            }
          }
          if object.adapter.is_none() && object.device.is_none() {
            state.objects.remove(path.as_str());
          }
        }
        _ => return,
      }
    }
    self.publish();
  }
}
