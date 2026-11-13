//! Keyboard input: focus, Tab, Enter and Space, and key handlers.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Counter, Harness, capture_logs};
use crate::{
  Align, Button, Color, Icon, Key, KeyEvent, Modifiers, Point, PointerButton, Row, Style, Text,
  Window,
};

const NORMAL: Style = Style::new().background(Color::rgb(1, 1, 1));
const FOCUS: Style = Style::new().background(Color::rgb(4, 4, 4));

fn press(key: Key) -> KeyEvent {
  KeyEvent {
    key,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

fn shift_tab() -> KeyEvent {
  KeyEvent {
    modifiers: Modifiers {
      shift: true,
      ..Modifiers::default()
    },
    ..press(Key::Tab)
  }
}

fn background(window: &Window, path: &[usize]) -> Option<Color> {
  super::node_at(window.view().root(), path)
    .style()
    .background
}

fn square() -> Icon {
  Icon::new("x").size(20)
}

/// A row of three 20-pixel buttons, each counting its clicks, with the
/// keyboard in the window.
fn three_buttons(h: &mut Harness) -> (Window, [Counter; 3]) {
  let counters = [Counter::default(), Counter::default(), Counter::default()];
  let row = counters
    .iter()
    .fold(Row::new().align(Align::Start), |row, counter| {
      let counter = counter.clone();
      row.child(
        Button::new(square())
          .style(NORMAL)
          .focus_style(FOCUS)
          .on_click(move |_| counter.hit()),
      )
    });
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  (window, counters)
}

#[test]
fn tab_moves_the_focus_through_the_buttons_and_wraps() {
  let mut h = Harness::new();
  let (window, _) = three_buttons(&mut h);
  assert_eq!(window.focused(), None);
  let paths = (0..3)
    .map(|index| format!("PanelWindow[DP-1]/Row/Button[{index}]"))
    .collect::<Vec<_>>();
  for expected in [&paths[0], &paths[1], &paths[2], &paths[0]] {
    window.key(&press(Key::Tab));
    assert_eq!(window.focused().as_ref(), Some(expected));
  }
  window.key(&shift_tab());
  assert_eq!(window.focused().as_ref(), Some(&paths[2]));
}

#[test]
fn the_focused_button_shows_its_focus_style_only_while_the_window_has_the_keyboard() {
  let mut h = Harness::new();
  let (window, _) = three_buttons(&mut h);
  window.key(&press(Key::Tab));
  window.take_damage();
  window.key(&press(Key::Tab));
  assert_eq!(background(&window, &[0, 0]), NORMAL.background);
  assert_eq!(background(&window, &[0, 1]), FOCUS.background);
  // Both buttons repaint, and only they do.
  let damage = window.take_damage();
  assert_eq!(damage.len(), 2, "{damage:?}");

  window.keyboard_leave();
  assert_eq!(background(&window, &[0, 1]), NORMAL.background);
  window.keyboard_enter();
  assert_eq!(background(&window, &[0, 1]), FOCUS.background);
}

#[test]
fn enter_and_space_click_the_focused_button() {
  let mut h = Harness::new();
  let (window, [first, second, _]) = three_buttons(&mut h);
  // Nothing is focused yet, so nothing is clicked.
  window.key(&press(Key::Enter));
  window.key(&press(Key::Tab));
  window.key(&press(Key::Enter));
  window.key(&press(Key::Tab));
  window.key(&press(Key::Character(' ')));
  window.key(&press(Key::Character('x')));
  assert_eq!((first.get(), second.get()), (1, 1));
}

#[test]
fn keys_go_to_the_focused_button_then_the_window_then_the_defaults() {
  let mut h = Harness::new();
  let seen = Rc::new(RefCell::new(Vec::new()));
  let clicks = Counter::default();
  let (button_seen, window_seen, c) = (Rc::clone(&seen), Rc::clone(&seen), clicks.clone());
  let row = Row::new().child(
    Button::new(Text::new("a"))
      .on_click(move |_| c.hit())
      .on_key(move |_, event| {
        button_seen
          .borrow_mut()
          .push(format!("button {:?}", event.key));
        event.key == Key::Escape
      }),
  );
  let panel = h.panel().child(row).on_key(move |_, event| {
    window_seen
      .borrow_mut()
      .push(format!("window {:?}", event.key));
    event.key == Key::Down
  });
  let window = h.window(panel, 200.0, 30.0);
  window.keyboard_enter();

  // With nothing focused, the window gets the key first.
  window.key(&press(Key::Up));
  window.key(&press(Key::Tab));
  window.key(&press(Key::Escape));
  window.key(&press(Key::Down));
  window.key(&press(Key::Enter));
  assert_eq!(
    *seen.borrow(),
    [
      "window Up",
      "window Tab",
      "button Escape",
      "button Down",
      "window Down",
      "button Enter",
      "window Enter"
    ]
  );
  assert_eq!(clicks.get(), 1);
}

#[test]
fn a_panicking_key_handler_is_logged_and_the_window_still_gets_keys() {
  let mut h = Harness::new();
  let window_keys = Counter::default();
  let w = window_keys.clone();
  let row = Row::new().child(Button::new(square()).on_key(|_, _| panic!("key failure")));
  let panel = h.panel().child(row).on_key(move |_, event| {
    w.hit();
    event.key == Key::Left
  });
  let window = h.window(panel, 200.0, 30.0);
  window.keyboard_enter();
  window.key(&press(Key::Tab));
  let logs = capture_logs(|| {
    window.key(&press(Key::Left));
    window.key(&press(Key::Left));
  });
  assert!(
    logs.contains("handler PanelWindow[DP-1]/Row/Button[0].on_key panicked and is disabled"),
    "{logs}"
  );
  assert!(logs.contains("key failure"), "{logs}");
  // The window got Tab, then both lefts: the first after the button
  // panicked and the second with it disabled.
  assert_eq!(window_keys.get(), 3);
  assert!(window.focused().is_some());
}

#[test]
fn removing_the_focused_button_drops_the_focus() {
  let mut h = Harness::new();
  let shown = h.cx.signal(vec![1, 2]);
  let row = Row::new().children_keyed(move || shown.get(), |n| *n, |_, _| Button::new(square()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  window.key(&press(Key::Tab));
  window.key(&press(Key::Tab));
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[1]")
  );
  shown.set(vec![1]);
  h.frame(&window);
  assert_eq!(window.focused(), None);
  // Tab starts again from the first.
  window.key(&press(Key::Tab));
  assert_eq!(
    window.focused().as_deref(),
    Some("PanelWindow[DP-1]/Row/Button[0]")
  );
}

#[test]
fn tab_in_a_window_without_buttons_does_nothing() {
  let mut h = Harness::new();
  let window = h.window(h.panel().child(Text::new("a")), 200.0, 30.0);
  window.keyboard_enter();
  window.key(&press(Key::Tab));
  window.key(&shift_tab());
  window.key(&press(Key::Enter));
  assert_eq!(window.focused(), None);
}

#[test]
fn a_focus_style_that_moves_buttons_updates_the_hover() {
  let mut h = Harness::new();
  let (first, second) = (Counter::default(), Counter::default());
  let (f, s) = (first.clone(), second.clone());
  let row = Row::new()
    .align(Align::Start)
    .child(
      Button::new(square())
        .focus_style(Style::new().padding(0, 10))
        .on_click(move |_| f.hit()),
    )
    .child(Button::new(square()).on_click(move |_| s.hit()));
  let window = h.window(h.panel().child(row), 200.0, 30.0);
  window.keyboard_enter();
  // Over the second button, which focusing the first pushes 20 pixels
  // right, putting the first under the pointer.
  window.pointer_motion(Point::new(25.0, 10.0));
  window.key(&press(Key::Tab));
  h.frame(&window);
  window.pointer_button(PointerButton::Left, true);
  window.pointer_button(PointerButton::Left, false);
  assert_eq!((first.get(), second.get()), (1, 0));
}

#[test]
fn a_panicking_window_key_handler_is_logged_and_tab_still_works() {
  let mut h = Harness::new();
  let row = Row::new().child(Button::new(square()));
  let panel = h
    .panel()
    .child(row)
    .on_key(|_, _| panic!("window key failure"));
  let window = h.window(panel, 200.0, 30.0);
  window.keyboard_enter();
  let logs = capture_logs(|| window.key(&press(Key::Tab)));
  assert!(
    logs.contains("handler PanelWindow[DP-1].on_key panicked and is disabled"),
    "{logs}"
  );
  // A panicked handler didn't use the key, so Tab still moved the focus.
  assert!(window.focused().is_some());
}
