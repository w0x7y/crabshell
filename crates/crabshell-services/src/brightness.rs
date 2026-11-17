//! Screen brightness: the backlights in sysfs, set through logind so no
//! root is needed.

#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::Once;

use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use inotify::WatchMask;

use crate::Shared;
use crate::dbus::set_changed;
use crate::logind;
use crate::watch::{self, Watch};

/// The most backlights kept.
const MAX_DEVICES: usize = 32;
/// The most bytes read from one sysfs file: a number and a newline.
const MAX_READ: u64 = 64;

/// A backlight in `/sys/class/backlight`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backlight {
  /// Its sysfs name, such as `intel_backlight`, which
  /// [`Brightness::set`] takes.
  pub name: String,
  /// How bright it is, from 0 to `max_brightness`.
  pub brightness: u32,
  /// The brightest it goes.
  pub max_brightness: u32,
}

/// Screen brightness, from `cx.brightness()`. Cloning it is cheap.
#[derive(Clone)]
pub struct Brightness {
  /// `None` when the shell provided no services.
  service: Option<Rc<Service>>,
  devices: ReadSignal<Vec<Backlight>>,
}

impl Brightness {
  /// Every backlight, by name. Empty on a machine with none.
  pub fn devices(&self) -> ReadSignal<Vec<Backlight>> {
    self.devices
  }

  /// Asks logind to set the backlight `name` to `value`, held to its
  /// `max_brightness`. It shows in [`devices`](Self::devices) once the
  /// kernel has it. An unknown name or a refusal is logged.
  pub fn set(&self, name: &str, value: u32) {
    let Some(service) = &self.service else {
      return;
    };
    let max = untrack(|| {
      self.devices.try_with(|devices| {
        let device = devices.iter().find(|device| device.name == name)?;
        Some(device.max_brightness)
      })
    });
    match max.flatten() {
      Some(max) => service.logind.set_brightness(name, value.min(max)),
      None => tracing::warn!(name, "no such backlight, so its brightness wasn't set"),
    }
  }
}

/// `cx.brightness()`.
pub(crate) fn brightness(cx: &Cx) -> Brightness {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.brightness() needs the services the shell provides; it stays empty");
    });
    return Brightness {
      service: None,
      devices: cx.signal(Vec::new()).read_only(),
    };
  };
  Brightness {
    devices: service.start(),
    service: Some(service),
  }
}

/// The backlights for one shell, read from the first ask.
pub(crate) struct Service {
  shared: Rc<Shared>,
  logind: Rc<logind::Service>,
  state: RefCell<Option<State>>,
}

struct State {
  devices: Signal<Vec<Backlight>>,
  /// The backlights' names, as listed at the start.
  names: Vec<String>,
  /// Those whose files were logged as unreadable, so each logs once.
  logged: HashSet<String>,
  _watches: Vec<Watch>,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>, logind: Rc<logind::Service>) -> Service {
    Service {
      shared,
      logind,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> ReadSignal<Vec<Backlight>> {
    if let Some(state) = &*self.state.borrow() {
      return state.devices.read_only();
    }
    // For `set`, so logind is reached before a config asks.
    self.logind.start();
    let root = &self.shared.config.backlight;
    let names = names(root);
    let watches = names
      .iter()
      .filter_map(|name| {
        // The kernel says a brightness changed here, whether a program
        // wrote `brightness` or the firmware moved it on a key.
        let path = root.join(name).join("actual_brightness");
        let service = Rc::downgrade(self);
        let on_change = move |_: &watch::Change| {
          if let Some(service) = Weak::upgrade(&service) {
            service.read();
          }
        };
        watch::watch(&self.shared, &path, WatchMask::MODIFY, on_change)
          .inspect_err(|error| {
            tracing::warn!(%error, path = %path.display(), "can't follow this backlight");
          })
          .ok()
      })
      .collect();
    let devices = self.shared.root.signal(Vec::new());
    *self.state.borrow_mut() = Some(State {
      devices,
      names,
      logged: HashSet::new(),
      _watches: watches,
    });
    self.read();
    devices.read_only()
  }

  /// Reads every backlight again and shows what changed.
  fn read(&self) {
    let root = &self.shared.config.backlight;
    let read = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      let mut devices = Vec::new();
      for name in &state.names {
        let dir = root.join(name);
        let read = read_value(&dir.join("brightness"))
          .and_then(|brightness| Ok((brightness, read_value(&dir.join("max_brightness"))?)));
        match read {
          Ok((brightness, max_brightness)) => devices.push(Backlight {
            name: name.clone(),
            brightness: brightness.min(max_brightness),
            max_brightness,
          }),
          Err(error) => {
            if state.logged.insert(name.clone()) {
              tracing::warn!(name, %error, "can't read this backlight, so it isn't listed");
            }
          }
        }
      }
      (state.devices, devices)
    };
    set_changed(read.0, read.1);
  }
}

/// The backlights' names under `root`, sorted, at most [`MAX_DEVICES`].
fn names(root: &Path) -> Vec<String> {
  let entries = match std::fs::read_dir(root) {
    Ok(entries) => entries,
    Err(error) => {
      tracing::info!(%error, root = %root.display(), "no backlights");
      return Vec::new();
    }
  };
  let mut names: Vec<String> = entries
    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
    .collect();
  names.sort();
  if names.len() > MAX_DEVICES {
    tracing::warn!(
      count = names.len(),
      "more backlights than {MAX_DEVICES}; the rest aren't listed"
    );
    names.truncate(MAX_DEVICES);
  }
  names
}

/// The number in the sysfs file at `path`.
fn read_value(path: &Path) -> std::io::Result<u32> {
  let mut text = String::new();
  std::fs::File::open(path)?
    .take(MAX_READ + 1)
    .read_to_string(&mut text)?;
  if text.len() as u64 > MAX_READ {
    return Err(std::io::Error::other(format!(
      "{} is over {MAX_READ} bytes",
      path.display()
    )));
  }
  text
    .trim()
    .parse()
    .map_err(|error| std::io::Error::other(format!("{} holds {text:?}: {error}", path.display())))
}
