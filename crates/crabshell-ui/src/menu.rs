//! [`Menu`]: a vertical list of buttons built from menu entries, such as a
//! tray item's DBusMenu, with separators, checkmarks, radio buttons and
//! submenus that open as nested popups.

use std::rc::Rc;

use crabshell_reactive::Memo;

use crate::cx::Cx;
use crate::popup::PopupWindow;
use crate::prop::{IntoProp, Prop};
use crate::style::{Color, Style};
use crate::widgets::{
  Align, Button, Column, Common, Element, ElementKind, Row, Spacer, Stack, Text, Widget,
  visibility_methods,
};

/// One entry of a [`Menu`].
#[derive(Clone, Debug, PartialEq)]
pub struct MenuEntry {
  /// The app's id for it, which the menu's handlers get.
  pub id: i32,
  /// What it says.
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
  /// opens, so its `children` may still be empty.
  pub submenu: bool,
  /// Its submenu's entries.
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

/// Room around each entry's text.
const ITEM: Style = Style::new().padding(4, 12);
/// Laid over an entry under the pointer.
const HOVER: Style = Style::new().background(Color::rgba(128, 128, 128, 64));
/// A separator's line: 2 px of gray.
const SEPARATOR: Style = Style::new()
  .background(Color::rgba(128, 128, 128, 128))
  .padding(1, 0);
/// Laid over an entry that can't be clicked.
const DISABLED: Style = Style::new().opacity(0.5);
/// A checkmark or radio button that is off keeps its room.
const HIDDEN: Style = Style::new().opacity(0.0);

/// A vertical list of buttons, one per entry, as a popup menu shows them.
///
/// A click on an entry calls [`on_activate`](Self::on_activate) with its
/// id, then closes the popups it is in. A click on an entry with a
/// submenu calls [`on_open_submenu`](Self::on_open_submenu) and opens the
/// submenu as a popup next to it, with the same styles and handlers.
/// Disabled entries ignore clicks. Each style takes a value or a closure.
///
/// ```
/// use crabshell_ui::{Menu, MenuEntry, PopupWindow};
///
/// let quit = MenuEntry {
///     id: 1,
///     label: "Quit".into(),
///     separator: false,
///     enabled: true,
///     toggle: None,
///     checked: false,
///     submenu: false,
///     children: Vec::new(),
/// };
/// let popup = PopupWindow::new().child(
///     Menu::new(vec![quit]).on_activate(|_cx, id| println!("clicked {id}")),
/// );
/// ```
pub struct Menu {
  entries: Prop<Vec<MenuEntry>>,
  style: Option<Prop<Style>>,
  item_style: Option<Prop<Style>>,
  hover_style: Option<Prop<Style>>,
  separator_style: Option<Prop<Style>>,
  on_activate: Option<EntryHandler>,
  on_open_submenu: Option<EntryHandler>,
  common: Common,
}

impl Menu {
  /// A menu of `entries`: a list, or a closure that returns one.
  pub fn new(entries: impl IntoProp<Vec<MenuEntry>>) -> Menu {
    Menu {
      entries: entries.into_prop(),
      style: None,
      item_style: None,
      hover_style: None,
      separator_style: None,
      on_activate: None,
      on_open_submenu: None,
      common: Common::default(),
    }
  }

  /// Sets the style of the menu's box: its background, border and padding.
  pub fn style(mut self, style: impl IntoProp<Style>) -> Menu {
    self.style = Some(style.into_prop());
    self
  }

  /// Sets each entry's style. The default pads the text 4 px above and
  /// below and 12 px left and right.
  pub fn item_style(mut self, style: impl IntoProp<Style>) -> Menu {
    self.item_style = Some(style.into_prop());
    self
  }

  /// Sets the style laid over an entry under the pointer. The default is
  /// a faint gray background.
  pub fn hover_style(mut self, style: impl IntoProp<Style>) -> Menu {
    self.hover_style = Some(style.into_prop());
    self
  }

  /// Sets a separator's style. A separator is as tall as the style's
  /// vertical padding twice; the default is a 2 px gray line.
  pub fn separator_style(mut self, style: impl IntoProp<Style>) -> Menu {
    self.separator_style = Some(style.into_prop());
    self
  }

  /// Runs `handler` with an entry's id when that entry is clicked.
  pub fn on_activate(mut self, handler: impl Fn(&Cx, i32) + 'static) -> Menu {
    self.on_activate = Some(Rc::new(handler));
    self
  }

  /// Runs `handler` with an entry's id when its submenu opens, as a tray
  /// menu tells its app with `open_submenu`.
  pub fn on_open_submenu(mut self, handler: impl Fn(&Cx, i32) + 'static) -> Menu {
    self.on_open_submenu = Some(Rc::new(handler));
    self
  }

  visibility_methods!();
}

/// What a menu and its submenus share: the styles as closures any of them
/// can call, and the handlers.
struct Look {
  menu: Shared<Style>,
  item: Shared<Style>,
  hover: Shared<Style>,
  separator: Shared<Style>,
  on_activate: Option<EntryHandler>,
  on_open_submenu: Option<EntryHandler>,
}

type Shared<T> = Rc<dyn Fn() -> T>;

/// A handler that gets an entry's id.
type EntryHandler = Rc<dyn Fn(&Cx, i32)>;

/// A property as a closure that submenus can share, or `default`.
fn shared<T: Clone + 'static>(prop: Option<Prop<T>>, default: T) -> Shared<T> {
  prop.map_or_else(|| Rc::new(move || default.clone()), Prop::shared)
}

impl Widget for Menu {
  fn into_element(self: Box<Self>) -> Element {
    let look = Rc::new(Look {
      menu: shared(self.style, Style::new()),
      item: shared(self.item_style, ITEM),
      hover: shared(self.hover_style, HOVER),
      separator: shared(self.separator_style, SEPARATOR),
      on_activate: self.on_activate,
      on_open_submenu: self.on_open_submenu,
    });
    let mut element = menu(look, shared(Some(self.entries), Vec::new()));
    element.common.visible = self.common.visible;
    element.common.geometry = self.common.geometry;
    element.common.mapped_geometry = self.common.mapped_geometry;
    element
  }
}

/// An element that is already built, so a function can return either
/// kind of entry.
struct Built(Element);

impl Widget for Built {
  fn into_element(self: Box<Self>) -> Element {
    self.0
  }
}

/// A menu of `entries` in `look`: a column with one child per entry id.
fn menu(look: Rc<Look>, entries: Shared<Vec<MenuEntry>>) -> Element {
  let style = Rc::clone(&look.menu);
  let column = Column::new().style(move || style()).children_keyed(
    move || entries(),
    // A separator and an entry differ in kind, so an id that changes
    // between them gets a new child.
    |entry: &MenuEntry| (entry.id, entry.separator),
    move |_cx, entry| entry_view(&look, entry),
  );
  let mut element = Box::new(column).into_element();
  element.name = "Menu";
  element
}

/// One entry: a separator line, or a button with its checkmark, label and
/// submenu arrow. Which of the two is fixed when the entry first appears.
fn entry_view(look: &Rc<Look>, entry: Memo<MenuEntry>) -> Built {
  if entry.with_untracked(|entry| entry.separator) {
    let separator = Rc::clone(&look.separator);
    let line = Stack::new().style(move || separator());
    return Built(Box::new(Column::new().padding(3, 0).child(line)).into_element());
  }
  let mark = move || match entry.get().toggle {
    Some(MenuToggle::Radio) => "●".to_owned(),
    _ => "✓".to_owned(),
  };
  let shown = move || {
    let entry = entry.get();
    if entry.toggle.is_some() && entry.checked {
      Style::new()
    } else {
      HIDDEN
    }
  };
  let arrow = move || if entry.get().submenu { "▸" } else { "" }.to_owned();
  // The mark's room fits either symbol, so every label lines up.
  let marks = Stack::new()
    .child(Text::new("✓").style(HIDDEN))
    .child(Text::new("●").style(HIDDEN))
    .child(Text::new(mark).style(shown));
  let row = Row::new()
    .gap(8)
    .align(Align::Center)
    .child(marks)
    .child(Text::new(move || entry.get().label))
    .child(Spacer::new())
    .child(Text::new(arrow));
  // A button centers its child; this row fills it instead, so labels
  // start at the left and arrows sit at the right.
  let mut row = Box::new(row).into_element();
  if let ElementKind::Flex(flex) = &mut row.kind {
    flex.grow = true;
  }
  let (item, hover) = (Rc::clone(&look.item), Rc::clone(&look.hover));
  let clicked = Rc::clone(look);
  let button = Button::new(Built(row))
    .style(move || {
      let style = item();
      if entry.get().enabled {
        style
      } else {
        style.merge(DISABLED)
      }
    })
    .hover_style(move || {
      if entry.get().enabled {
        hover()
      } else {
        Style::new()
      }
    })
    .on_click(move |cx| click(&clicked, cx, entry));
  Built(Box::new(button).into_element())
}

/// What a click on `entry` does: opens its submenu, or activates it and
/// closes the menu.
fn click(look: &Rc<Look>, cx: &Cx, entry: Memo<MenuEntry>) {
  let (id, enabled, submenu) =
    entry.with_untracked(|entry| (entry.id, entry.enabled, entry.submenu));
  if !enabled {
    return;
  }
  if submenu {
    if let Some(open) = &look.on_open_submenu {
      open(cx, id);
    }
    let look = Rc::clone(look);
    let children: Shared<Vec<MenuEntry>> = Rc::new(move || {
      let children = entry.try_with(|entry| entry.children.clone());
      children.unwrap_or_default()
    });
    cx.open_popup(move |_| PopupWindow::new().child(Built(menu(look, children))));
    return;
  }
  if let Some(activate) = &look.on_activate {
    activate(cx, id);
  }
  // A menu in a panel rather than a popup has nothing to close.
  cx.close_popups_if_any();
}
