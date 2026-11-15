//! [`Tray`]: one button per tray item, each opening its menu in a popup.
//! It lives here rather than in `crabshell-ui` because it needs the tray
//! service, and it is built only from `crabshell-ui`'s widgets.

use std::rc::Rc;

use crabshell_reactive::{Memo, Signal};
use crabshell_ui::{
  Align, Button, Color, Cx, Element, Icon, IconSource, IntoProp, Menu, PopupWindow, Prop, Rect,
  Row, Style, Widget,
};

use super::{TrayHost, TrayItem};
use crate::ServicesExt;

/// A menu's popup unless [`Tray::menu_style`] sets another: light text on
/// dark gray.
const MENU: Style = Style::new()
  .background(Color::rgb(0x2b, 0x2b, 0x2b))
  .color(Color::rgb(0xe6, 0xe6, 0xe6))
  .radius(6.0)
  .padding(4, 0);

/// Room around each icon.
const BUTTON: Style = Style::new().padding(2, 4).radius(4.0);

/// The icons' size unless [`Tray::icon_size`] sets another.
const ICON_SIZE: u32 = 16;

/// Shown for an item that gives no icon at all.
const NO_ICON: &str = "image-missing";

type Shared<T> = Rc<dyn Fn() -> T>;

/// The system tray: one icon button per tray item, in the order the items
/// registered. A left click activates an item, or opens its menu when the
/// item is only a menu. A right click opens the menu, a middle click is the
/// item's secondary activation, and scrolling over an icon scrolls it.
///
/// A menu opens as a [`PopupWindow`] holding a [`Menu`], next to its icon.
/// Clicking an entry tells the app and closes the menu.
///
/// ```
/// use crabshell_services::Tray;
///
/// let tray = Tray::new().icon_size(18);
/// ```
#[derive(Default)]
pub struct Tray {
  icon_size: Option<Prop<u32>>,
  menu_style: Option<Prop<Style>>,
  visible: Option<Prop<bool>>,
  geometry: Option<Signal<Rect>>,
}

impl Tray {
  /// A tray with 16 px icons.
  pub fn new() -> Tray {
    Tray::default()
  }

  /// Sets the icons' size in logical pixels.
  pub fn icon_size(mut self, size: impl IntoProp<u32>) -> Tray {
    self.icon_size = Some(size.into_prop());
    self
  }

  /// Sets the style of each menu's popup: its background, border, padding,
  /// and the text's color and font. The default is light text on dark
  /// gray.
  pub fn menu_style(mut self, style: impl IntoProp<Style>) -> Tray {
    self.menu_style = Some(style.into_prop());
    self
  }

  /// Shows or hides the tray, as [`Row::visible`] does for a row.
  pub fn visible(mut self, visible: impl IntoProp<bool>) -> Tray {
    self.visible = Some(visible.into_prop());
    self
  }

  /// Shares where the tray is after each layout that moves it, as
  /// [`Row::geometry`] does for a row.
  pub fn geometry(mut self, geometry: Signal<Rect>) -> Tray {
    self.geometry = Some(geometry);
    self
  }
}

impl Widget for Tray {
  fn into_element(self: Box<Self>) -> Element {
    let size: Shared<u32> = self
      .icon_size
      .map_or_else(|| Rc::new(|| ICON_SIZE), Prop::shared);
    let style: Shared<Style> = self
      .menu_style
      .map_or_else(|| Rc::new(|| MENU), Prop::shared);
    // `cx.tray()` needs a context, which a keyed child's view gets, so the
    // tray is one such child: a row of the items.
    let mut tray = Row::new().children_keyed(
      || vec![()],
      |_| (),
      move |cx, _| {
        let host = cx.tray();
        let items = host.items();
        let (size, style) = (Rc::clone(&size), Rc::clone(&style));
        Row::new().align(Align::Center).children_keyed(
          move || items.get(),
          |item| item.id.clone(),
          move |_, item| item_button(&host, item, &size, &style),
        )
      },
    );
    if let Some(visible) = self.visible {
      tray = tray.visible(visible);
    }
    if let Some(geometry) = self.geometry {
      tray = tray.geometry(geometry);
    }
    Box::new(tray).into_element()
  }
}

/// The button for one item.
fn item_button(
  host: &TrayHost,
  item: Memo<TrayItem>,
  size: &Shared<u32>,
  style: &Shared<Style>,
) -> Button {
  // The id is the key, so it stays for the button's life.
  let id: Rc<str> = item.with_untracked(|item| item.id.as_str().into());
  let (icon_size, pick) = (Rc::clone(size), Rc::clone(size));
  let icon = Icon::new(move || {
    // shortcut: a widget doesn't know its output's scale, so pixmaps are
    // picked for twice the size, which a 1x output scales down well; pick
    // for the real scale once a context knows it.
    let pixels = pick().saturating_mul(2);
    let source = item.with(|item| item.icon(pixels));
    source.unwrap_or_else(|| IconSource::Name(NO_ICON.into()))
  })
  .size(move || icon_size());
  let click = {
    let (host, id, style) = (host.clone(), Rc::clone(&id), Rc::clone(style));
    move |cx: &Cx| {
      if item.with_untracked(|item| item.item_is_menu) {
        open_menu(cx, &host, &id, &style);
      } else {
        host.activate(&id);
      }
    }
  };
  let right_click = {
    let (host, id, style) = (host.clone(), Rc::clone(&id), Rc::clone(style));
    move |cx: &Cx| open_menu(cx, &host, &id, &style)
  };
  let (middle, scroll) = (host.clone(), host.clone());
  let middle_id = Rc::clone(&id);
  Button::new(icon)
    .style(BUTTON)
    .on_click(click)
    .on_right_click(right_click)
    .on_middle_click(move |_| middle.secondary_activate(&middle_id))
    .on_scroll(move |_, steps| scroll.scroll(&id, steps))
}

/// Opens item `id`'s menu in a popup next to the button `cx` belongs to.
/// The popup opens at once, so it grabs the pointer with this click's
/// serial, and stays empty and clear until the app's layout arrives.
fn open_menu(cx: &Cx, host: &TrayHost, id: &Rc<str>, style: &Shared<Style>) {
  let Some(service) = host.service.clone() else {
    return;
  };
  let Some(opened) = service.open_menu(id) else {
    return;
  };
  let (host, id, style) = (host.clone(), Rc::clone(id), Rc::clone(style));
  cx.open_popup(move |cx| {
    let _following = cx.signal(Following {
      service: Rc::clone(&service),
      opened,
    });
    let menu = host.menu();
    // The popup closes once the tray stops following this opening: the
    // app's menu went, its first read failed, or the app left.
    let closer = cx.clone();
    cx.effect(move || {
      menu.with(|_| ());
      if !service.follows(opened) {
        closer.close_popup();
      }
    });
    let shown = {
      let id = Rc::clone(&id);
      move || menu.with(|menu| menu.as_ref().is_some_and(|menu| *menu.item == *id))
    };
    let entries = {
      let id = Rc::clone(&id);
      move || {
        menu.with(|menu| {
          let ours = menu.as_ref().filter(|menu| *menu.item == *id);
          ours.map(|menu| menu.entries.clone()).unwrap_or_default()
        })
      }
    };
    let (activate, submenu) = (host.clone(), host);
    // The style is the menu's, not the popup's, so submenus get it too.
    let menu = Menu::new(entries)
      .style(move || if shown() { style() } else { Style::new() })
      .on_activate(move |_, entry| activate.menu_event(&id, entry))
      .on_open_submenu(move |_, entry| submenu.open_submenu(entry));
    PopupWindow::new().child(menu)
  });
}

/// Kept under a menu's popup. When the popup goes, the tray stops following
/// the app's menu, unless another opening has taken over since.
struct Following {
  service: Rc<super::Service>,
  opened: u64,
}

impl Drop for Following {
  fn drop(&mut self) {
    self.service.close_menu_opened(self.opened);
  }
}
