//! What a screen reader is shown: roles, names, states, children, clicks
//! and the events heard while one listens.

use std::rc::Rc;
use std::task::Waker;

use super::{Counter, Harness, capture_logs};
use crate::{
  AccessEvent, AccessRole, Accessibility, Button, Icon, Key, KeyEvent, Modifiers, Row, Slider,
  Spacer, Style, Text, TextInput, Window,
};

fn tab() -> KeyEvent {
  KeyEvent {
    key: Key::Tab,
    text: None,
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

/// A harness whose root context has the registry, as the shell's does.
fn harness() -> (Harness, Rc<Accessibility>) {
  let harness = Harness::new();
  let access = harness.cx.provide(Accessibility::default());
  (harness, access)
}

/// The counter bar's row: a button with a label, a spacer and a text.
fn counter(harness: &mut Harness, clicks: &Counter) -> Window {
  let count = harness.cx.signal(0);
  let clicks = clicks.clone();
  let panel = harness.panel().child(
    Row::new()
      .child(
        Button::new(Text::new(move || format!("clicked {} times", count.get()))).on_click(
          move |_| {
            clicks.hit();
            count.update(|n| *n += 1);
          },
        ),
      )
      .child(Spacer::new())
      .child(Text::new("hello")),
  );
  harness.window(panel, 300.0, 30.0)
}

fn take(access: &Accessibility) -> Vec<AccessEvent> {
  access.take_events(Waker::noop())
}

#[test]
fn a_counter_bar_shows_its_button_by_role_and_name() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let _window = counter(&mut harness, &clicks);
  let windows = access.windows();
  assert_eq!(windows.len(), 1);
  let frame = access.node(windows[0]).expect("the window");
  assert_eq!(frame.role, AccessRole::Frame);
  assert_eq!(frame.name, "crabshell");
  assert_eq!(frame.parent, None);
  let row = access.node(frame.children[0]).expect("the row");
  assert_eq!(row.role, AccessRole::Filler);
  assert_eq!(row.parent, Some(windows[0]));
  let roles: Vec<_> = row
    .children
    .iter()
    .map(|&id| access.node(id).expect("a child").role)
    .collect();
  assert_eq!(
    roles,
    [AccessRole::Button, AccessRole::Filler, AccessRole::Label]
  );
  let button = access.node(row.children[0]).expect("the button");
  assert_eq!(button.name, "clicked 0 times");
  assert_eq!(button.index, 0);
  assert!(button.focusable && button.enabled && button.shown && button.clickable);
  assert!(!button.focused);
  assert!(button.rect.width > 0.0);
  let label = access.node(row.children[2]).expect("the label");
  assert_eq!((label.name.as_str(), label.index), ("hello", 2));
  assert!(!label.clickable && !label.focusable);
}

#[test]
fn accessible_name_wins_over_the_text_and_follows_its_closure() {
  let (mut harness, access) = harness();
  let name = harness.cx.signal(String::from("volume"));
  let panel = harness.panel().child(
    Row::new()
      .child(Button::new(Icon::new("audio")).accessible_name(move || name.get()))
      .child(Button::new(Text::new("text")).accessible_name("named")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let names = |access: &Accessibility| -> Vec<String> {
    let row = access.node(row.children[0]).expect("a row").parent;
    let row = access.node(row.expect("a parent")).expect("the row");
    row
      .children
      .iter()
      .map(|&id| access.node(id).expect("a button").name)
      .collect()
  };
  assert_eq!(names(&access), ["volume", "named"]);
  // Heard only while a screen reader listens.
  assert!(take(&access).is_empty());
  access.listen(true);
  name.set(String::from("muted"));
  harness.frame(&window);
  assert_eq!(names(&access), ["muted", "named"]);
  assert_eq!(take(&access), [AccessEvent::NameChanged(row.children[0])]);
}

#[test]
fn a_label_change_renames_the_button_around_it() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  access.listen(true);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let button = row.children[0];
  let label = access.node(button).expect("the button").children[0];
  assert!(access.click(button));
  harness.frame(&window);
  assert_eq!(clicks.get(), 1);
  assert_eq!(
    access.node(button).expect("the button").name,
    "clicked 1 times"
  );
  assert_eq!(
    take(&access),
    [
      AccessEvent::NameChanged(label),
      AccessEvent::NameChanged(button)
    ]
  );
}

#[test]
fn a_click_needs_an_enabled_shown_button() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let (on, shown) = (harness.cx.signal(true), harness.cx.signal(true));
  let hit = clicks.clone();
  let panel = harness.panel().child(
    Row::new()
      .child(
        Button::new(Text::new("a"))
          .enabled(move || on.get())
          .visible(move || shown.get())
          .on_click(move |_| hit.hit()),
      )
      .child(Text::new("b")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  let frame = access.node(access.windows()[0]).expect("the window");
  let row = access.node(frame.children[0]).expect("the row");
  let (button, text) = (row.children[0], row.children[1]);
  assert!(!access.click(text), "a text has no action");
  on.set(false);
  harness.frame(&window);
  let state = access.node(button).expect("the button");
  assert!(!state.enabled && !state.focusable);
  assert!(!access.click(button));
  on.set(true);
  shown.set(false);
  harness.frame(&window);
  assert!(!access.click(button));
  // A hidden widget leaves its parent's children.
  let row = access.node(row.children[1]).expect("the text").parent;
  assert_eq!(
    access
      .node(row.expect("a parent"))
      .expect("the row")
      .children,
    [text]
  );
  assert_eq!(clicks.get(), 0);
}

#[test]
fn tab_and_the_keyboard_coming_and_going_are_heard() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  let root = access.windows()[0];
  let button = access
    .node(access.node(root).expect("the window").children[0])
    .expect("the row")
    .children[0];
  // Nothing is kept while nobody listens.
  window.keyboard_enter();
  window.key(&tab());
  assert!(take(&access).is_empty());
  window.keyboard_leave();
  access.listen(true);
  window.keyboard_enter();
  assert_eq!(
    take(&access),
    [
      AccessEvent::Active(root, true),
      AccessEvent::Focused(button, true)
    ]
  );
  assert!(access.node(button).expect("the button").focused);
  assert!(access.node(root).expect("the window").active);
  window.key(&tab());
  assert!(take(&access).is_empty(), "Tab stays on the only button");
  window.keyboard_leave();
  assert_eq!(
    take(&access),
    [
      AccessEvent::Focused(button, false),
      AccessEvent::Active(root, false)
    ]
  );
  // What nobody took goes when the screen reader does.
  window.keyboard_enter();
  access.listen(false);
  assert!(take(&access).is_empty());
}

#[test]
fn a_slider_is_one_object_that_takes_the_focus() {
  let (mut harness, access) = harness();
  let value = harness.cx.signal(0.5);
  let panel = harness.panel().child(
    Row::new()
      .child(Slider::new(value).accessible_name("brightness"))
      .child(TextInput::new(harness.cx.signal(String::new())).placeholder("search")),
  );
  let window = harness.window(panel, 300.0, 30.0);
  access.listen(true);
  let row = access
    .node(
      access
        .node(access.windows()[0])
        .expect("the window")
        .children[0],
    )
    .expect("the row");
  let slider = access.node(row.children[0]).expect("the slider");
  assert_eq!(slider.role, AccessRole::Slider);
  assert_eq!(slider.name, "brightness");
  assert!(slider.children.is_empty() && slider.focusable);
  let field = access.node(row.children[1]).expect("the field");
  assert_eq!(
    (field.role, field.name.as_str()),
    (AccessRole::Entry, "search")
  );
  assert!(field.editable && !field.multi_line);
  window.keyboard_enter();
  take(&access);
  window.key(&tab());
  assert_eq!(take(&access), [AccessEvent::Focused(row.children[0], true)]);
}

#[test]
fn a_window_leaves_the_list_when_dropped() {
  let (mut harness, access) = harness();
  let clicks = Counter::default();
  let window = counter(&mut harness, &clicks);
  let root = access.windows()[0];
  drop(window);
  assert!(access.windows().is_empty());
  assert!(access.node(root).is_none());
  assert!(!access.click(root));
}

#[test]
fn a_panicking_accessible_name_is_logged_with_its_path() {
  let (mut harness, _access) = harness();
  let logs = capture_logs(|| {
    let panel = harness.panel().child(
      Row::new().child(
        Button::new(Text::new("ok"))
          .style(Style::new())
          .accessible_name(|| -> String { panic!("no name") }),
      ),
    );
    let _window = harness.window(panel, 300.0, 30.0);
  });
  assert!(
    logs.contains(
      "PanelWindow[DP-1]/Row/Button[0].accessible_name panicked and is disabled: no name"
    ),
    "{logs}"
  );
}
