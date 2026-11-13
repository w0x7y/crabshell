//! `Menu`: entries as buttons, separators, marks and submenus, and what a
//! click does.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Harness, capture_logs, node_at, rect_at};
use crate::{
  Button, Content, Icon, Menu, MenuEntry, MenuToggle, Point, PointerButton, PopupWindow, Window,
};

fn entry(id: i32, label: &str) -> MenuEntry {
  MenuEntry {
    id,
    label: label.into(),
    separator: false,
    enabled: true,
    toggle: None,
    checked: false,
    submenu: false,
    children: Vec::new(),
  }
}

/// An entry, a separator, a checked checkmark, an unchecked radio button,
/// a submenu and a disabled entry.
fn entries() -> Vec<MenuEntry> {
  vec![
    entry(1, "Open"),
    MenuEntry {
      separator: true,
      ..entry(2, "")
    },
    MenuEntry {
      toggle: Some(MenuToggle::Checkmark),
      checked: true,
      ..entry(3, "Wi-Fi")
    },
    MenuEntry {
      toggle: Some(MenuToggle::Radio),
      ..entry(4, "B")
    },
    MenuEntry {
      submenu: true,
      children: vec![entry(6, "Inner")],
      ..entry(5, "More")
    },
    MenuEntry {
      enabled: false,
      ..entry(7, "Off")
    },
  ]
}

fn text_at(window: &Window, path: &[usize]) -> String {
  match node_at(window.view().root(), path).content() {
    Content::Text(text) => text.content().to_owned(),
    _ => panic!("no text at {path:?}"),
  }
}

fn opacity_at(window: &Window, path: &[usize]) -> Option<f32> {
  node_at(window.view().root(), path).style().opacity
}

/// Clicks the middle of the node at `path`.
fn click(window: &Window, path: &[usize]) {
  let rect = rect_at(window, path);
  window.pointer_motion(Point::new(
    rect.x + rect.width / 2.0,
    rect.y + rect.height / 2.0,
  ));
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
}

/// Every id a handler got, in order.
#[derive(Clone, Default)]
struct Ids(Rc<RefCell<Vec<i32>>>);

impl Ids {
  fn push(&self, id: i32) {
    self.0.borrow_mut().push(id);
  }

  fn get(&self) -> Vec<i32> {
    self.0.borrow().clone()
  }
}

/// A bar whose button opens a popup holding `menu`, already open: the
/// bar and the popup.
fn opened(h: &mut Harness, menu: impl Fn() -> Menu + 'static) -> (Window, Window) {
  let button = Button::new(Icon::new("x").size(20))
    .on_click(move |cx| cx.open_popup(|_| PopupWindow::new().child(menu())));
  let window = h.window(h.panel().child(button), 20.0, 20.0);
  click(&window, &[0]);
  let mut popups = window.take_popups();
  assert_eq!(popups.len(), 1);
  let popup = popups.remove(0);
  h.frame(&popup);
  (window, popup)
}

#[test]
fn entries_become_buttons_separators_marks_and_arrows() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(Menu::new(entries())), 200.0, 300.0);
  let view = window.view();
  let menu = node_at(view.root(), &[0]);
  assert_eq!(menu.path(), "PanelWindow[DP-1]/Menu");
  let names: Vec<&str> = menu.children().map(|child| child.name()).collect();
  assert_eq!(
    names,
    ["Button", "Column", "Button", "Button", "Button", "Button"]
  );
  drop(view);

  // Each button holds a row: mark, label, spacer, arrow.
  assert_eq!(text_at(&window, &[0, 0, 0, 1]), "Open");
  assert_eq!(opacity_at(&window, &[0, 0, 0, 0, 2]), Some(0.0), "no mark");
  assert_eq!(text_at(&window, &[0, 0, 0, 3]), "");
  assert_eq!(text_at(&window, &[0, 2, 0, 0, 2]), "✓");
  assert_eq!(opacity_at(&window, &[0, 2, 0, 0, 2]), None, "checked");
  assert_eq!(text_at(&window, &[0, 3, 0, 0, 2]), "●");
  assert_eq!(
    opacity_at(&window, &[0, 3, 0, 0, 2]),
    Some(0.0),
    "unchecked"
  );
  // Every label starts at the same x, and every row fills its button.
  let label = |entry| rect_at(&window, &[0, entry, 0, 1]).x;
  assert_eq!(label(0), label(2));
  assert_eq!(label(2), label(3));
  let button = rect_at(&window, &[0, 4]);
  let arrow = rect_at(&window, &[0, 4, 0, 3]);
  assert_eq!(
    arrow.x + arrow.width,
    button.x + button.width - 12.0,
    "arrow at the right"
  );
  assert_eq!(text_at(&window, &[0, 4, 0, 3]), "▸");
  assert_eq!(opacity_at(&window, &[0, 5]), Some(0.5), "disabled");
  // The separator is a 2 px line across the menu.
  let line = rect_at(&window, &[0, 1, 0]);
  assert_eq!((line.width, line.height), (200.0, 2.0));
}

#[test]
fn a_click_activates_an_entry_and_closes_the_popup() {
  let mut h = Harness::new();
  let activated = Ids::default();
  let ids = activated.clone();
  let (_bar, popup) = opened(&mut h, move || {
    let ids = ids.clone();
    Menu::new(entries()).on_activate(move |_cx, id| ids.push(id))
  });
  click(&popup, &[0, 5]);
  assert_eq!(activated.get(), [], "a disabled entry ignores clicks");
  assert!(!popup.wants_close());
  click(&popup, &[0, 0]);
  assert_eq!(activated.get(), [1]);
  assert!(popup.wants_close());
}

#[test]
fn a_submenu_opens_next_to_its_entry_with_the_same_handlers() {
  let mut h = Harness::new();
  let (activated, opened_ids) = (Ids::default(), Ids::default());
  let (on_activate, on_open) = (activated.clone(), opened_ids.clone());
  let (_bar, popup) = opened(&mut h, move || {
    let (activate, open) = (on_activate.clone(), on_open.clone());
    Menu::new(entries())
      .on_activate(move |_cx, id| activate.push(id))
      .on_open_submenu(move |_cx, id| open.push(id))
  });
  click(&popup, &[0, 4]);
  assert_eq!(opened_ids.get(), [5]);
  assert!(!popup.wants_close());
  let mut nested = popup.take_popups();
  assert_eq!(nested.len(), 1);
  let submenu = nested.remove(0);
  assert_eq!(submenu.anchor(), Some(rect_at(&popup, &[0, 4])));
  h.frame(&submenu);
  assert_eq!(text_at(&submenu, &[0, 0, 0, 1]), "Inner");
  click(&submenu, &[0, 0]);
  assert_eq!(activated.get(), [6]);
  assert!(submenu.wants_close());
  assert!(popup.wants_close(), "the menu that opened it closes too");
}

#[test]
fn entries_and_submenus_follow_their_signal() {
  let mut h = Harness::new();
  let list = h.cx.signal(entries());
  let window = h.window(h.panel().child(Menu::new(move || list.get())), 200.0, 300.0);
  click(&window, &[0, 4]);
  let submenu = window.take_popups().remove(0);
  h.frame(&submenu);

  list.update(|entries| {
    entries[0].label = "Close".into();
    entries[4].children = vec![entry(6, "Inner"), entry(8, "Filled late")];
  });
  h.frame(&window);
  h.frame(&submenu);
  assert_eq!(text_at(&window, &[0, 0, 0, 1]), "Close");
  assert_eq!(text_at(&submenu, &[0, 1, 0, 1]), "Filled late");

  // An id that turns from a separator into an entry gets a button.
  list.update(|entries| entries[1] = entry(2, "Was a line"));
  h.frame(&window);
  assert_eq!(text_at(&window, &[0, 1, 0, 1]), "Was a line");

  // The submenu's entry going closes the submenu alone.
  list.update(|entries| entries.retain(|entry| entry.id != 5));
  h.frame(&window);
  assert!(submenu.wants_close());
}

#[test]
fn a_handler_that_panics_is_logged_with_its_entry_and_the_rest_still_work() {
  let mut h = Harness::new();
  let activated = Ids::default();
  let ids = activated.clone();
  let menu = Menu::new(entries()).on_activate(move |_cx, id| {
    assert_ne!(id, 1, "entry 1 is broken");
    ids.push(id);
  });
  let window = h.window(h.panel().child(menu), 200.0, 300.0);
  let logs = capture_logs(|| click(&window, &[0, 0]));
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Menu/Button[0].on_click panicked"),
    "{logs}"
  );
  click(&window, &[0, 2]);
  assert_eq!(activated.get(), [3]);
}
