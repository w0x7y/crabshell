//! Tray menus, over `com.canonical.dbusmenu`: the layout of the one menu
//! that is open, kept up to date while it stays open.

use std::rc::Rc;

use serde::Deserialize;
use zbus::{Connection, DeserializeDict, Type, as_variant};

use super::{Service, bus_call};

pub(super) const DBUSMENU: &str = "com.canonical.dbusmenu";

/// The properties asked for. Leaving out `icon-data` keeps PNG bytes the
/// menu doesn't draw off the bus.
const PROPERTY_NAMES: &[&str] = &[
  "type",
  "label",
  "enabled",
  "visible",
  "toggle-type",
  "toggle-state",
  "children-display",
];

/// The most entries a menu keeps, all levels together. Past it, the rest
/// are left out.
const MAX_ENTRIES: usize = 1024;

/// A tray item's open menu, from [`TrayHost::menu`](super::TrayHost::menu).
#[derive(Clone, Debug, PartialEq)]
pub struct TrayMenu {
  /// The item's id, as [`TrayItem::id`](super::TrayItem::id).
  pub item: String,
  /// The top-level entries, in order.
  pub entries: Vec<MenuEntry>,
}

/// One entry of a tray menu. Hidden entries are left out.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuEntry {
  /// The app's id for it, to give [`TrayHost::menu_event`](super::TrayHost::menu_event).
  pub id: i32,
  /// What it says, with the underscores that mark access keys removed.
  pub label: String,
  /// Whether it is a separator line rather than something to click.
  pub separator: bool,
  /// Whether it can be clicked.
  pub enabled: bool,
  /// Whether it shows a checkmark or a radio button.
  pub toggle: Option<MenuToggle>,
  /// Whether that checkmark or radio button is on.
  pub checked: bool,
  /// Whether it opens a submenu. Some apps fill a submenu only when it
  /// opens, so ask with [`TrayHost::open_submenu`](super::TrayHost::open_submenu).
  pub submenu: bool,
  /// Its submenu's entries, empty for none or until they are asked for.
  pub children: Vec<MenuEntry>,
}

/// What a toggling menu entry shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuToggle {
  /// A checkmark, on or off on its own.
  Checkmark,
  /// A radio button, one of a group.
  Radio,
}

/// The menu that is open: whose it is and where its signals come from.
pub(super) struct OpenMenu {
  item: String,
  bus: String,
  owner: String,
  path: String,
  /// Which opening this is, so a task left from an earlier one stops.
  opened: u64,
  /// The newest layout read asked for, so an older reply is dropped.
  read: u64,
}

impl OpenMenu {
  /// The match rule for this menu's own signals, added while it is open
  /// so other apps' menus never wake the shell.
  fn rule(&self) -> String {
    let OpenMenu { owner, path, .. } = self;
    format!("type='signal',sender='{owner}',path='{path}',interface='{DBUSMENU}'")
  }
}

impl Service {
  /// Opens item `id`'s menu: tells the app, then reads the layout.
  pub(super) fn open_menu(self: &Rc<Self>, id: &str) {
    let open = self.with_entry(id, |entry| {
      let path = entry.shown.as_ref()?.menu.clone();
      Some(OpenMenu {
        item: entry.id.clone(),
        bus: entry.bus.clone(),
        owner: entry.owner.clone()?,
        path: (!path.is_empty()).then_some(path)?,
        opened: 0,
        read: 0,
      })
    });
    let Some(mut open) = open.flatten() else {
      return tracing::warn!(id, "this tray item has no menu");
    };
    self.close_menu();
    let (rule, bus, path) = (open.rule(), open.bus.clone(), open.path.clone());
    let (connection, opened) = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.menu_count += 1;
      open.opened = state.menu_count;
      state.open = Some(open);
      (state.connection.clone(), state.menu_count)
    };
    let Some(connection) = connection else {
      return;
    };
    let service = Rc::clone(self);
    self.shared.spawn(async move {
      if let Err(error) = bus_call(&connection, "AddMatch", &rule).await {
        tracing::warn!(%error, "can't follow a tray menu's changes");
      }
      let about = connection.call_method(
        Some(bus.as_str()),
        path.as_str(),
        Some(DBUSMENU),
        "AboutToShow",
        &0,
      );
      // Some apps don't have it; the layout is read either way.
      if let Err(error) = about.await {
        tracing::debug!(%error, "a tray menu refused AboutToShow");
      }
      if service.is_open(|open| open.opened == opened) {
        service.read_menu(&connection);
      }
    });
  }

  /// Tells the open menu's app that submenu `entry` opens, and reads the
  /// layout again if the app says the submenu changed.
  pub(super) fn open_submenu(self: &Rc<Self>, entry: i32) {
    let asked = {
      let state = self.state.borrow();
      let state = state.as_ref();
      let open = state.and_then(|state| state.open.as_ref());
      let connection = state.and_then(|state| state.connection.clone());
      open
        .zip(connection)
        .map(|(open, connection)| (open.bus.clone(), open.path.clone(), open.opened, connection))
    };
    let Some((bus, path, opened, connection)) = asked else {
      return tracing::warn!(entry, "no tray menu is open");
    };
    let service = Rc::clone(self);
    self.shared.spawn(async move {
      let about = connection.call_method(
        Some(bus.as_str()),
        path.as_str(),
        Some(DBUSMENU),
        "AboutToShow",
        &entry,
      );
      let changed = about
        .await
        .and_then(|reply| reply.body().deserialize::<bool>());
      match changed {
        Ok(true) if service.is_open(|open| open.opened == opened) => service.read_menu(&connection),
        Ok(_) => {}
        Err(error) => tracing::debug!(entry, %error, "a tray menu refused AboutToShow"),
      }
    });
  }

  /// Whether a menu is open and `test` holds for it.
  fn is_open(&self, test: impl FnOnce(&OpenMenu) -> bool) -> bool {
    let state = self.state.borrow();
    let open = state.as_ref().and_then(|state| state.open.as_ref());
    open.is_some_and(test)
  }

  /// Closes the open menu, if any, and stops following it.
  pub(super) fn close_menu(&self) {
    let (open, connection) = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      if state.menu.try_with(Option::is_some) == Some(true) {
        state.menu.set(None);
      }
      (state.open.take(), state.connection.clone())
    };
    if let (Some(open), Some(connection)) = (open, connection) {
      let rule = open.rule();
      self.shared.spawn(async move {
        if let Err(error) = bus_call(&connection, "RemoveMatch", &rule).await {
          tracing::debug!(%error, "a tray menu's match was already gone");
        }
      });
    }
  }

  /// Closes the menu if it is item `id`'s, as when the item goes.
  pub(super) fn close_menu_of(&self, id: &str) {
    if self.is_open(|open| open.item == id) {
      self.close_menu();
    }
  }

  /// Reads the open menu again when it says it changed.
  pub(super) fn menu_changed(self: &Rc<Self>, connection: &Connection, sender: &str, path: &str) {
    if self.is_open(|open| open.owner == sender && open.path == path) {
      self.read_menu(connection);
    }
  }

  /// Reads the open menu's whole layout and shows it, unless the menu
  /// closed or a newer read was asked for meanwhile. A failed read is
  /// logged and closes the menu, so a broken one stops waking the shell.
  fn read_menu(self: &Rc<Self>, connection: &Connection) {
    let asked = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.menu_count += 1;
      let read = state.menu_count;
      let Some(open) = state.open.as_mut() else {
        return;
      };
      open.read = read;
      (open.item.clone(), open.bus.clone(), open.path.clone(), read)
    };
    let (item, bus, path, read) = asked;
    let service = Rc::clone(self);
    let connection = connection.clone();
    self.shared.spawn(async move {
      let args = (0, -1, PROPERTY_NAMES);
      let call = connection.call_method(
        Some(bus.as_str()),
        path.as_str(),
        Some(DBUSMENU),
        "GetLayout",
        &args,
      );
      let layout = call
        .await
        .and_then(|reply| reply.body().deserialize::<(u32, Layout)>());
      let newest = |open: &OpenMenu| open.read == read;
      let children = match layout {
        Ok((_, Layout(_, _, children))) => children,
        Err(error) => {
          tracing::warn!(item, %error, "can't read a tray item's menu; closing it");
          if service.is_open(newest) {
            service.close_menu();
          }
          return;
        }
      };
      let mut left = MAX_ENTRIES;
      let entries = entries(children, &mut left);
      if left == 0 {
        tracing::debug!(
          item,
          "a tray menu kept only its first {MAX_ENTRIES} entries"
        );
      }
      let menu = Some(TrayMenu { item, entries });
      let state = service.state.borrow();
      let Some(state) = state.as_ref() else {
        return;
      };
      // An unchanged layout isn't set again, so nothing repaints.
      let changed = state.menu.try_with(|shown| *shown != menu) == Some(true);
      if state.open.as_ref().is_some_and(newest) && changed {
        state.menu.set(menu);
      }
    });
  }
}

/// One node of `GetLayout`'s tree: id, properties and children.
#[derive(Deserialize, Type)]
#[cfg_attr(test, derive(Clone, serde::Serialize))]
pub(super) struct Layout(pub(super) i32, pub(super) Props, pub(super) Vec<Child>);

/// A child node, which the spec wraps in a variant.
#[derive(Deserialize, Type)]
#[cfg_attr(test, derive(Clone, serde::Serialize))]
#[zbus(signature = "v")]
pub(super) struct Child(#[serde(with = "as_variant")] pub(super) Layout);

/// The properties the menu reads. One of the wrong type fails the read.
#[derive(Default, DeserializeDict, Type)]
#[cfg_attr(test, derive(Clone, zbus::SerializeDict))]
#[zbus(signature = "a{sv}", rename_all = "kebab-case")]
pub(super) struct Props {
  #[zbus(rename = "type")]
  pub(super) kind: Option<String>,
  pub(super) label: Option<String>,
  pub(super) enabled: Option<bool>,
  pub(super) visible: Option<bool>,
  pub(super) toggle_type: Option<String>,
  pub(super) toggle_state: Option<i32>,
  pub(super) children_display: Option<String>,
}

/// The visible entries of `children`, with the spec's defaults, until
/// `left` entries have been taken.
pub(super) fn entries(children: Vec<Child>, left: &mut usize) -> Vec<MenuEntry> {
  let mut shown = Vec::new();
  for Child(Layout(id, props, children)) in children {
    if props.visible == Some(false) {
      continue;
    }
    let Some(fewer) = left.checked_sub(1) else {
      break;
    };
    *left = fewer;
    let children = entries(children, left);
    shown.push(MenuEntry {
      id,
      label: without_access_keys(props.label.as_deref().unwrap_or_default()),
      separator: props.kind.as_deref() == Some("separator"),
      enabled: props.enabled.unwrap_or(true),
      toggle: match props.toggle_type.as_deref() {
        Some("checkmark") => Some(MenuToggle::Checkmark),
        Some("radio") => Some(MenuToggle::Radio),
        _ => None,
      },
      checked: props.toggle_state == Some(1),
      submenu: props.children_display.as_deref() == Some("submenu") || !children.is_empty(),
      children,
    });
  }
  shown
}

/// `label` without the underscore before an access key; `__` is one
/// underscore. The bar takes no keyboard input, so the keys go unused.
pub(super) fn without_access_keys(label: &str) -> String {
  let mut shown = String::with_capacity(label.len());
  let mut chars = label.chars();
  while let Some(char) = chars.next() {
    match char {
      '_' => shown.extend(chars.next()),
      char => shown.push(char),
    }
  }
  shown
}
