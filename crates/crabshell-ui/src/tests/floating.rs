//! Floating windows: settings, closing, hiding and what widgets ask.

use super::{Counter, Harness, capture_logs};
use crate::{
  Button, Edge, FloatingConfig, FloatingWindow, Icon, Point, PointerButton, Size, Text, Window,
  WindowRequest,
};

fn shown(window: &Window) -> FloatingConfig {
  window.floating_config().expect("a floating window")
}

/// Builds `floating`, runs its bindings and lays it out at 200 by 100.
fn build(h: &mut Harness, floating: FloatingWindow) -> Window {
  let window = Window::floating(&h.cx, floating);
  window.set_size(Size::new(200.0, 100.0));
  h.frame(&window);
  window
}

#[test]
fn settings_reach_the_config_and_changes_are_reported() {
  let mut h = Harness::new();
  let big = h.cx.signal(false);
  let window = build(
    &mut h,
    FloatingWindow::new()
      .title("About")
      .width(move || if big.get() { 600 } else { 320 })
      .height(200)
      .min_size((100, 50))
      .max_size((800, 0))
      .fullscreen(move || big.get()),
  );
  assert!(window.take_surface_changed());
  assert_eq!(
    shown(&window),
    FloatingConfig {
      title: String::from("About"),
      width: 320,
      height: 200,
      min_size: (100, 50),
      max_size: (800, 0),
      maximized: false,
      fullscreen: false,
      shown: true,
    }
  );
  big.set(true);
  h.frame(&window);
  assert!(window.take_surface_changed());
  assert_eq!(
    (shown(&window).width, shown(&window).fullscreen),
    (600, true)
  );
  // A panel has none.
  let panel = h.window(h.panel(), 200.0, 30.0);
  assert_eq!(panel.floating_config(), None);
}

#[test]
fn closing_runs_on_close_or_hides_until_visible_turns_false() {
  let mut h = Harness::new();
  let open = h.cx.signal(true);
  let handled = build(
    &mut h,
    FloatingWindow::new()
      .visible(move || open.get())
      .on_close(move |_| open.set(false)),
  );
  handled.request_close();
  h.frame(&handled);
  assert!(!shown(&handled).shown, "on_close hid it");

  let open = h.cx.signal(true);
  let plain = build(&mut h, FloatingWindow::new().visible(move || open.get()));
  plain.take_surface_changed();
  plain.request_close();
  assert!(plain.take_surface_changed());
  assert!(!shown(&plain).shown, "closed without on_close");
  open.set(false);
  h.frame(&plain);
  open.set(true);
  h.frame(&plain);
  assert!(shown(&plain).shown, "shown again");
}

#[test]
fn a_panicking_on_close_is_logged_with_its_path() {
  let mut h = Harness::new();
  let window = build(
    &mut h,
    FloatingWindow::new().on_close(|_| panic!("close failure")),
  );
  let logs = capture_logs(|| window.request_close());
  assert!(
    logs.contains("handler FloatingWindow.on_close panicked and is disabled"),
    "{logs}"
  );
}

#[test]
fn widgets_ask_the_window_to_move_resize_and_minimize() {
  let mut h = Harness::new();
  let clicks = Counter::default();
  let c = clicks.clone();
  let window = build(
    &mut h,
    FloatingWindow::new().child(Button::new(Icon::new("x").size(20)).on_press(move |cx| {
      c.hit();
      cx.start_move();
      cx.start_resize(Edge::Bottom | Edge::Right);
      cx.minimize();
    })),
  );
  window.pointer_motion(Point::new(100.0, 50.0));
  // On the press itself, before any release.
  window.pointer_button(PointerButton::Left, true);
  assert_eq!(clicks.get(), 1);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!(clicks.get(), 1, "not again on release");
  assert_eq!(
    window.take_requests(),
    [
      WindowRequest::Move,
      WindowRequest::Resize(Edge::Bottom | Edge::Right),
      WindowRequest::Minimize
    ]
  );
  assert!(window.take_requests().is_empty());
}

#[test]
fn a_panel_logs_window_requests_and_ignores_them() {
  let mut h = Harness::new();
  let row = crate::Row::new().child(Button::new(Text::new("x")).on_click(|cx| cx.start_move()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  let logs = capture_logs(|| {
    window.pointer_motion(Point::new(2.0, 10.0));
    window.pointer_button(PointerButton::Left, true);
    window.pointer_button(PointerButton::Left, false);
  });
  assert!(logs.contains("works only in a floating window"), "{logs}");
  assert!(window.take_requests().is_empty());
}

#[test]
fn dropping_a_floating_window_frees_what_it_made() {
  let mut h = Harness::new();
  let before = h.runtime.node_count();
  let window = build(
    &mut h,
    FloatingWindow::new().on_close(|_| {}).child(Text::new("x")),
  );
  drop(window);
  assert_eq!(h.runtime.node_count(), before);
}

#[test]
fn a_floating_window_takes_a_blur_region_too() {
  let mut h = Harness::new();
  let frosted = h.cx.signal(false);
  let window = build(
    &mut h,
    FloatingWindow::new().blur(move || frosted.get().then(|| crate::Region::rect(0, 0, 50, 50))),
  );
  assert_eq!(window.blur(), None);
  window.take_surface_changed();
  frosted.set(true);
  h.frame(&window);
  assert!(window.take_surface_changed());
  assert_eq!(window.blur(), Some(crate::Region::rect(0, 0, 50, 50)));
}

#[test]
fn a_floating_window_can_inhibit_idle_too() {
  let mut h = Harness::new();
  let window = build(&mut h, FloatingWindow::new().inhibit_idle(true));
  assert!(window.inhibits_idle());
}

#[test]
fn a_floating_window_can_inhibit_shortcuts_and_grab_focus_too() {
  let mut h = Harness::new();
  let inhibited = h.cx.signal(false);
  let grabbed = h.cx.signal(false);
  let window = build(
    &mut h,
    FloatingWindow::new()
      .inhibit_shortcuts(true)
      .shortcuts_inhibited(inhibited)
      .focus_grab(grabbed),
  );
  assert!(window.inhibits_shortcuts());
  assert_eq!(window.shortcuts_inhibited(), Some(inhibited));
  assert_eq!(window.focus_grab(), Some(grabbed));
}
