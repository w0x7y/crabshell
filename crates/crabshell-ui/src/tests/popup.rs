//! Popups: opening one from a widget, its size, closing it, and its owner.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, capture_logs, paths};
use crate::{
  Align, Button, Cx, Icon, Point, PointerButton, Popup, PopupWindow, Rect, Row, Size, Style, Text,
  Window,
};

fn click_at(window: &Window, point: Point) {
  window.pointer_motion(point);
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
}

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// A bar with one 20 px button at x = 10 that opens `build`'s popup.
fn opener(h: &mut Harness, build: impl Fn(&Cx) -> PopupWindow + 'static) -> Window {
  let button = Button::new(square()).on_click(move |cx| cx.open_popup(&build));
  let row = Row::new().padding(0, 10).align(Align::Start).child(button);
  h.window(h.panel().child(row), 200.0, 30.0)
}

/// Clicks the button and takes the one popup it opened, laid out.
fn open(h: &mut Harness, window: &Window) -> Popup {
  click_at(window, Point::new(20.0, 10.0));
  let mut popups = window.take_popups();
  assert_eq!(popups.len(), 1, "one popup");
  let popup = popups.remove(0);
  h.frame(&popup.window);
  popup
}

#[test]
fn a_click_opens_a_popup_next_to_its_button() {
  let mut h = Harness::new();
  let window = opener(&mut h, |_| PopupWindow::new().child(square()));
  assert!(window.take_popups().is_empty());
  let popup = open(&mut h, &window);
  assert_eq!(popup.anchor, Rect::new(10.0, 0.0, 20.0, 20.0));
  assert!(popup.window.is_popup());
  assert!(!window.is_popup());
  assert!(!popup.window.wants_close());
  assert_eq!(popup.window.output(), window.output());
  assert_eq!(
    paths(&popup.window),
    [
      "PanelWindow[DP-1]/Row/Button[0]/PopupWindow",
      "PanelWindow[DP-1]/Row/Button[0]/PopupWindow/Icon",
    ]
  );
  // Taken once.
  assert!(window.take_popups().is_empty());
}

#[test]
fn a_popup_is_as_large_as_its_content() {
  let mut h = Harness::new();
  let side = h.cx.signal(20u32);
  let window = opener(&mut h, move |_| {
    PopupWindow::new()
      .style(Style::new().padding(4, 6))
      .child(Icon::new("x").size(move || side.get()))
  });
  let popup = open(&mut h, &window);
  assert_eq!(popup.window.size(), Size::new(32.0, 28.0));
  side.set(30);
  h.frame(&popup.window);
  assert_eq!(popup.window.size(), Size::new(42.0, 38.0));

  // Sizes are whole pixels, as xdg_positioner takes them.
  let window = opener(&mut h, |_| PopupWindow::new().child(Text::new("menu")));
  let size = open(&mut h, &window).window.size();
  assert!(size.width > 0.0 && size.width.fract() == 0.0, "{size:?}");
  assert!(size.height > 0.0 && size.height.fract() == 0.0, "{size:?}");

  // An empty popup, such as a submenu not filled yet, is 1 by 1, since
  // xdg_positioner refuses an empty size.
  let window = opener(&mut h, |_| PopupWindow::new());
  assert_eq!(open(&mut h, &window).window.size(), Size::new(1.0, 1.0));
}

#[test]
fn close_popup_inside_a_popup_asks_it_to_close() {
  let mut h = Harness::new();
  let window = opener(&mut h, |_| {
    PopupWindow::new().child(Button::new(square()).on_click(|cx| cx.close_popup()))
  });
  let popup = open(&mut h, &window);
  click_at(&popup.window, Point::new(10.0, 10.0));
  assert!(popup.window.wants_close());

  // A bar has no popup to close.
  let bar = h.window(
    h.panel()
      .child(Button::new(square()).on_click(|cx| cx.close_popup())),
    20.0,
    20.0,
  );
  let logs = capture_logs(|| click_at(&bar, Point::new(10.0, 10.0)));
  assert!(
    logs.contains("close_popup was called outside a popup"),
    "{logs}"
  );
}

#[test]
fn a_popup_closes_with_the_widget_that_opened_it_and_frees_its_nodes() {
  let mut h = Harness::new();
  let shown = h.cx.signal(vec![1]);
  let before = h.runtime.node_count();
  let row = Row::new()
    .padding(0, 10)
    .align(Align::Start)
    .children_keyed(
      move || shown.get(),
      |id| *id,
      |_cx, _id| {
        Button::new(square()).on_click(|cx| {
          cx.open_popup(|cx| {
            // The popup's own state, freed with it.
            let state = cx.signal(0);
            PopupWindow::new().child(Text::new(move || state.get().to_string()))
          })
        })
      },
    );
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let popup = open(&mut h, &window);
  shown.set(Vec::new());
  h.frame(&window);
  assert!(popup.window.wants_close());
  drop(popup);
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn a_context_kept_from_a_handler_opens_a_popup_later() {
  let mut h = Harness::new();
  let kept: Rc<RefCell<Option<Cx>>> = Rc::default();
  let keep = Rc::clone(&kept);
  let button = Button::new(square()).on_click(move |cx| *keep.borrow_mut() = Some(cx.clone()));
  let window = h.window(h.panel().child(button), 20.0, 20.0);
  click_at(&window, Point::new(10.0, 10.0));
  let cx = kept
    .borrow_mut()
    .take()
    .expect("the handler kept its context");
  cx.open_popup(|_| PopupWindow::new().child(square()));
  assert_eq!(window.take_popups().len(), 1);
}

#[test]
fn a_popup_that_panics_while_built_is_logged_and_the_button_keeps_working() {
  let mut h = Harness::new();
  let tries = Counter::default();
  let counted = tries.clone();
  let window = opener(&mut h, move |cx| {
    counted.hit();
    let _made_first = cx.signal(1);
    panic!("no popup today");
  });
  let nodes = h.runtime.node_count();
  let logs = capture_logs(|| click_at(&window, Point::new(20.0, 10.0)));
  assert!(
    logs.contains(
      "PanelWindow[DP-1]/Row/Button[0]/PopupWindow panicked while it was built, so it doesn't open: no popup today"
    ),
    "{logs}"
  );
  assert!(window.take_popups().is_empty());
  assert_eq!(h.runtime.node_count(), nodes, "what it made is freed");
  capture_logs(|| click_at(&window, Point::new(20.0, 10.0)));
  assert_eq!(tries.get(), 2, "the handler still runs");
}

#[test]
fn an_effect_that_opens_a_popup_runs_once_for_what_the_popup_reads() {
  let mut h = Harness::new();
  let kept: Rc<RefCell<Option<Cx>>> = Rc::default();
  let keep = Rc::clone(&kept);
  let button = Button::new(square()).on_click(move |cx| *keep.borrow_mut() = Some(cx.clone()));
  let window = h.window(h.panel().child(button), 20.0, 20.0);
  click_at(&window, Point::new(10.0, 10.0));
  let cx = kept
    .borrow_mut()
    .take()
    .expect("the handler kept its context");
  let title = h.cx.signal("menu".to_owned());
  h.cx.effect(move || {
    cx.open_popup(|_| PopupWindow::new().child(Text::new(title.get())));
  });
  h.runtime.flush();
  title.set("renamed".to_owned());
  h.runtime.flush();
  assert_eq!(window.take_popups().len(), 1);
}

#[test]
fn a_context_of_no_widget_opens_nothing() {
  let h = Harness::new();
  let logs = capture_logs(|| h.cx.open_popup(|_| PopupWindow::new()));
  assert!(
    logs.contains("open_popup needs a widget's context"),
    "{logs}"
  );
}
