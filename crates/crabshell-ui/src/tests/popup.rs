//! Popups: opening one from a widget, its size, closing it, and its owner.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, capture_logs, paths};
use crate::{
  Align, Button, Cx, Icon, Point, PointerButton, PopupWindow, Rect, Row, Size, Style, Text, Window,
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
fn open(h: &mut Harness, window: &Window) -> Window {
  click_at(window, Point::new(20.0, 10.0));
  let mut popups = window.take_popups();
  assert_eq!(popups.len(), 1, "one popup");
  let popup = popups.remove(0);
  h.frame(&popup);
  popup
}

#[test]
fn a_click_opens_a_popup_next_to_its_button() {
  let mut h = Harness::new();
  let window = opener(&mut h, |_| PopupWindow::new().child(square()));
  assert!(window.take_popups().is_empty());
  let popup = open(&mut h, &window);
  assert_eq!(popup.anchor(), Some(Rect::new(10.0, 0.0, 20.0, 20.0)));
  assert!(popup.is_popup());
  assert!(!window.is_popup());
  assert!(!popup.wants_close());
  assert_eq!(popup.output(), window.output());
  assert_eq!(
    paths(&popup),
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
  assert_eq!(popup.size(), Size::new(32.0, 28.0));
  side.set(30);
  h.frame(&popup);
  assert_eq!(popup.size(), Size::new(42.0, 38.0));

  // Sizes are whole pixels, as xdg_positioner takes them.
  let window = opener(&mut h, |_| PopupWindow::new().child(Text::new("menu")));
  let size = open(&mut h, &window).size();
  assert!(size.width > 0.0 && size.width.fract() == 0.0, "{size:?}");
  assert!(size.height > 0.0 && size.height.fract() == 0.0, "{size:?}");

  // An empty popup, such as a submenu not filled yet, is 1 by 1, since
  // xdg_positioner refuses an empty size.
  let window = opener(&mut h, |_| PopupWindow::new());
  assert_eq!(open(&mut h, &window).size(), Size::new(1.0, 1.0));
}

#[test]
fn close_popup_inside_a_popup_asks_it_to_close() {
  let mut h = Harness::new();
  let window = opener(&mut h, |_| {
    PopupWindow::new().child(Button::new(square()).on_click(|cx| cx.close_popup()))
  });
  let popup = open(&mut h, &window);
  click_at(&popup, Point::new(10.0, 10.0));
  assert!(popup.wants_close());

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
  assert!(popup.wants_close());
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

#[test]
fn a_popup_is_anchored_where_its_widget_was_at_the_last_layout() {
  let mut h = Harness::new();
  let left = h.cx.signal(10);
  // The keyed child opens a popup as it is built, before any layout. Its
  // context is the inner row's.
  let inner = Row::new().child(square()).children_keyed(
    || vec![()],
    |_| (),
    |cx, _| {
      let opener = cx.clone();
      cx.effect(move || opener.open_popup(|_| PopupWindow::new().child(square())));
      Row::new()
    },
  );
  let outer = Row::new()
    .padding(0, move || left.get())
    .align(Align::Start)
    .child(inner);
  let window = Window::new(&h.cx, h.panel().child(outer));
  h.runtime.flush();
  let popup = window.take_popups().pop().expect("opened as it was built");
  assert_eq!(popup.anchor(), None, "the row has no layout yet");

  window.set_size(Size::new(200.0, 30.0));
  h.frame(&window);
  assert_eq!(popup.anchor(), Some(Rect::new(10.0, 0.0, 20.0, 20.0)));
  left.set(30);
  h.frame(&window);
  assert_eq!(popup.anchor(), Some(Rect::new(30.0, 0.0, 20.0, 20.0)));
}

#[test]
fn a_popup_carries_the_placement_its_config_set() {
  use crate::{Adjust, Edge, Placement};

  let mut h = Harness::new();
  let window = opener(&mut h, |_| {
    PopupWindow::new()
      .edges(Edge::Top | Edge::Right)
      .gravity(Edge::Top)
      .adjustment(Adjust::FLIP | Adjust::SLIDE_Y)
      .child(square())
  });
  let popup = open(&mut h, &window);
  assert_eq!(
    popup.placement(),
    Placement {
      edges: Some(Edge::Top | Edge::Right),
      gravity: Some(Edge::Top.into()),
      adjustment: Some(Adjust::FLIP | Adjust::SLIDE_Y),
    }
  );
  assert!(Adjust::ALL.contains(Adjust::SLIDE));
  assert!(!Adjust::FLIP.contains(Adjust::SLIDE_X));
  // Unset, the platform picks.
  let plain = opener(&mut h, |_| PopupWindow::new().child(square()));
  assert_eq!(open(&mut h, &plain).placement(), Placement::default());
}

#[test]
fn a_popup_takes_a_mask() {
  let mut h = Harness::new();
  let masked = h.cx.signal(true);
  let window = opener(&mut h, move |_| {
    PopupWindow::new()
      .mask(move || masked.get().then(|| crate::Region::rect(0, 0, 10, 10)))
      .child(square())
  });
  let popup = open(&mut h, &window);
  assert!(popup.take_surface_changed());
  assert_eq!(
    popup.surface().mask,
    Some(crate::Region::rect(0, 0, 10, 10))
  );
  assert!(!popup.take_surface_changed(), "reported once");
  masked.set(false);
  h.frame(&popup);
  assert!(popup.take_surface_changed());
  assert_eq!(popup.surface().mask, None);

  // A mask that panics is logged with its path, and the popup opens.
  let window = opener(&mut h, |_| {
    PopupWindow::new()
      .mask(|| -> Option<crate::Region> { panic!("no mask") })
      .child(square())
  });
  let logs = capture_logs(|| {
    let popup = open(&mut h, &window);
    assert_eq!(popup.size(), Size::new(20.0, 20.0));
    assert_eq!(popup.surface().mask, None);
  });
  assert!(
    logs.contains("PanelWindow[DP-1]/Row/Button[0]/PopupWindow.mask panicked and is disabled"),
    "{logs}"
  );
}

#[test]
fn all_adjustments_include_resizing_as_in_quickshell() {
  use crate::Adjust;

  assert_eq!(Adjust::RESIZE, Adjust::RESIZE_X | Adjust::RESIZE_Y);
  assert_eq!(Adjust::ALL, Adjust::FLIP | Adjust::SLIDE | Adjust::RESIZE);
  assert!(!Adjust::SLIDE.contains(Adjust::RESIZE_X));
}

#[test]
fn a_popup_the_compositor_shrinks_is_laid_out_at_that_size() {
  let mut h = Harness::new();
  let window = opener(&mut h, |_| {
    PopupWindow::new()
      .style(Style::new().background(crate::Color::hex("#313244")))
      .child(Icon::new("x").size(40))
  });
  let popup = open(&mut h, &window);
  assert_eq!(popup.size(), Size::new(40.0, 40.0));
  popup.take_damage();

  popup.limit_size(Some(Size::new(25.0, 100.0)));
  assert!(popup.needs_layout());
  h.frame(&popup);
  // No larger than the limit, nor than its content.
  assert_eq!(popup.size(), Size::new(25.0, 40.0));
  assert_eq!(popup.wanted_size(), Size::new(40.0, 40.0));
  assert!(
    popup
      .take_damage()
      .contains(&Rect::new(0.0, 0.0, 25.0, 40.0)),
    "repainted whole"
  );

  // Given what it asks for again, it is its content's size.
  popup.limit_size(None);
  h.frame(&popup);
  assert_eq!(popup.size(), Size::new(40.0, 40.0));
  // A panel takes the size it is given, and has no limit.
  window.limit_size(Some(Size::new(1.0, 1.0)));
  assert!(!window.needs_layout());
  assert_eq!(window.wanted_size(), window.size());
}
