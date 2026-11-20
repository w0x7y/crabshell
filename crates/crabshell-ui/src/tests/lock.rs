//! Session locks: `locked`, each output's surface, the compositor's word
//! and the lock's owner.

use std::cell::RefCell;
use std::rc::Rc;

use super::{Harness, capture_logs, paths};
use crate::{
  Color, Key, KeyEvent, Lock, Modifiers, Output, SessionLock, Size, Style, Text, TextInput,
};

const BLACK: Style = Style::new().background(Color::rgb(0, 0, 0));

#[test]
fn locked_takes_a_value_or_follows_a_binding_from_the_next_flush() {
  let h = Harness::new();
  assert_eq!(Lock::new(&h.cx, SessionLock::new()).wanted(), Some(false));
  assert_eq!(
    Lock::new(&h.cx, SessionLock::new().locked(true)).wanted(),
    Some(true)
  );

  let on = h.cx.signal(false);
  let lock = Lock::new(&h.cx, SessionLock::new().locked(move || on.get()));
  h.runtime.flush();
  assert_eq!(lock.wanted(), Some(false));
  on.set(true);
  assert_eq!(lock.wanted(), Some(false), "not before the flush");
  h.runtime.flush();
  assert_eq!(lock.wanted(), Some(true));
}

#[test]
fn each_outputs_surface_has_its_path_the_style_and_what_the_builder_made() {
  let mut h = Harness::new();
  let lock = Lock::new(
    &h.cx,
    SessionLock::new()
      .style(BLACK)
      .surface(|_, output| Text::new(format!("locked on {}", output.name()))),
  );
  let window = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  window.set_size(Size::new(1920.0, 1080.0));
  h.frame(&window);
  assert_eq!(
    paths(&window),
    ["SessionLock[DP-1]", "SessionLock[DP-1]/Text"]
  );
  assert_eq!(window.output(), Output::new("DP-1"));
  let view = window.view();
  let root = view.root();
  assert_eq!(root.style(), BLACK);
  let text = root.children().next().expect("the builder's text");
  assert!(
    matches!(text.content(), crate::Content::Text(text) if text.content() == "locked on DP-1")
  );
  drop(view);
  // A surface isn't a panel: it asks the compositor for nothing.
  assert!(!window.take_surface_changed());
}

#[test]
fn the_compositors_word_is_recorded_once_per_change() {
  let h = Harness::new();
  let lock = Lock::new(&h.cx, SessionLock::new().locked(true));
  let locked = lock.locked();
  let seen = Rc::new(RefCell::new(Vec::new()));
  let log = Rc::clone(&seen);
  h.cx.effect(move || log.borrow_mut().push(locked.get()));
  h.runtime.flush();
  lock.set_locked(true);
  h.runtime.flush();
  lock.set_locked(true);
  h.runtime.flush();
  lock.set_locked(false);
  h.runtime.flush();
  assert_eq!(*seen.borrow(), [false, true, false]);
}

#[test]
fn a_panicking_builder_or_binding_is_logged_with_its_surfaces_path() {
  let mut h = Harness::new();
  let lock = Lock::new(
    &h.cx,
    SessionLock::new().style(BLACK).surface(|_, output| {
      if output.name() == "DP-1" {
        panic!("no surface for DP-1");
      }
      Text::new(|| -> String { panic!("no label") })
    }),
  );
  let mut built = None;
  let logs = capture_logs(|| built = lock.surface(&Output::new("DP-1")));
  assert!(
    logs.contains("SessionLock[DP-1] panicked while it was built, so it shows only the lock's style: no surface for DP-1"),
    "{logs}"
  );
  let window = built.expect("the lock lives");
  h.frame(&window);
  assert_eq!(paths(&window), ["SessionLock[DP-1]"]);
  assert_eq!(window.view().root().style(), BLACK);

  let other = lock.surface(&Output::new("DP-2")).expect("the lock lives");
  other.set_size(Size::new(100.0, 100.0));
  let logs = capture_logs(|| h.frame(&other));
  assert!(
    logs.contains("SessionLock[DP-2]/Text.text panicked"),
    "{logs}"
  );
}

#[test]
fn a_field_on_a_surface_takes_typing() {
  let mut h = Harness::new();
  let typed = h.cx.signal(String::new());
  let lock = Lock::new(
    &h.cx,
    SessionLock::new().surface(move |_, _| TextInput::new(typed).focus(true)),
  );
  let window = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  window.set_size(Size::new(300.0, 40.0));
  h.frame(&window);
  window.keyboard_enter();
  window.key(&KeyEvent {
    key: Key::Character('a'),
    text: Some(String::from("a")),
    modifiers: Modifiers::default(),
    repeat: false,
  });
  assert_eq!(typed.get_untracked(), "a");
}

fn key(key: Key, text: Option<&str>) -> KeyEvent {
  KeyEvent {
    key,
    text: text.map(str::to_owned),
    modifiers: Modifiers::default(),
    repeat: false,
  }
}

#[test]
fn on_key_gets_the_keys_a_surfaces_field_left() {
  let mut h = Harness::new();
  let typed = h.cx.signal(String::from("pw"));
  let seen = Rc::new(RefCell::new(Vec::new()));
  let record = Rc::clone(&seen);
  let lock = Lock::new(
    &h.cx,
    SessionLock::new()
      .surface(move |_, _| TextInput::new(typed).focus(true))
      .on_key(move |_, event| {
        record.borrow_mut().push(event.key.clone());
        if event.key == Key::Escape {
          typed.set(String::new());
        }
        event.key == Key::Escape
      }),
  );
  let window = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  window.set_size(Size::new(300.0, 40.0));
  h.frame(&window);
  window.keyboard_enter();
  window.key(&key(Key::Character('a'), Some("a")));
  window.key(&key(Key::Escape, None));
  assert_eq!(*seen.borrow(), [Key::Escape], "the field took the a");
  assert_eq!(typed.get_untracked(), "", "Escape cleared it");
}

#[test]
fn a_panicking_on_key_is_logged_with_its_surfaces_path_and_the_rest_runs() {
  let mut h = Harness::new();
  let typed = h.cx.signal(String::new());
  let seen = Rc::new(RefCell::new(Vec::new()));
  let record = Rc::clone(&seen);
  let lock = Lock::new(
    &h.cx,
    SessionLock::new()
      .surface(move |_, _| TextInput::new(typed).focus(true))
      .on_key(move |_, event| {
        if event.key == Key::Escape {
          panic!("escape failure");
        }
        record.borrow_mut().push(event.key.clone());
        true
      }),
  );
  let surfaces: Vec<_> = ["DP-1", "DP-2"]
    .into_iter()
    .map(|name| {
      let window = lock.surface(&Output::new(name)).expect("the lock lives");
      window.set_size(Size::new(300.0, 40.0));
      h.frame(&window);
      window.keyboard_enter();
      window
    })
    .collect();
  let logs = capture_logs(|| {
    surfaces[0].key(&key(Key::Escape, None));
  });
  assert!(
    logs.contains("handler SessionLock[DP-1].on_key panicked and is disabled: escape failure"),
    "{logs}"
  );
  surfaces[0].key(&key(Key::Down, None));
  surfaces[0].key(&key(Key::Character('a'), Some("a")));
  assert_eq!(typed.get_untracked(), "a", "the field still types");
  surfaces[1].key(&key(Key::Down, None));
  assert_eq!(*seen.borrow(), [Key::Down], "only DP-1's is disabled");
}

#[test]
fn surfaces_and_the_lock_free_what_they_made_and_a_gone_lock_builds_nothing() {
  let mut h = Harness::new();
  let on = h.cx.signal(true);
  let after_signal = h.runtime.node_count();
  let lock = Lock::new(
    &h.cx,
    SessionLock::new()
      .locked(move || on.get())
      .style(move || BLACK)
      .surface(|cx, _| {
        let label = cx.signal(String::from("locked"));
        Text::new(move || label.get())
      }),
  );
  h.runtime.flush();
  let with_lock = h.runtime.node_count();
  for output in ["DP-1", "DP-2"] {
    let window = lock.surface(&Output::new(output)).expect("the lock lives");
    h.frame(&window);
    assert!(h.runtime.node_count() > with_lock);
    drop(window);
    assert_eq!(h.runtime.node_count(), with_lock, "{output}'s surface");
  }
  drop(lock);
  assert_eq!(h.runtime.node_count(), after_signal, "the lock");

  let child = h.cx.child();
  let lock = Lock::new(&child, SessionLock::new().locked(true));
  child.owner().dispose();
  assert_eq!(lock.wanted(), None);
  assert!(lock.surface(&Output::new("DP-1")).is_none());
  lock.set_locked(true);
  drop(lock);
  assert_eq!(h.runtime.node_count(), after_signal);
}

#[test]
fn a_panicking_locked_or_style_binding_never_unlocks_and_is_logged_with_its_path() {
  let mut h = Harness::new();
  let boom = h.cx.signal(false);
  let lock = Lock::new(
    &h.cx,
    SessionLock::new()
      .locked(move || {
        if boom.get() {
          panic!("locked boom");
        }
        true
      })
      .style(move || {
        if boom.get() {
          panic!("style boom");
        }
        BLACK
      })
      .surface(|_, _| Text::new("x")),
  );
  h.runtime.flush();
  assert_eq!(lock.wanted(), Some(true));
  let window = lock.surface(&Output::new("DP-1")).expect("the lock lives");
  window.set_size(Size::new(100.0, 100.0));
  h.frame(&window);
  boom.set(true);
  let logs = capture_logs(|| {
    h.runtime.flush();
    h.frame(&window);
  });
  assert_eq!(lock.wanted(), Some(true), "a panic never unlocks");
  assert!(logs.contains("SessionLock.locked panicked"), "{logs}");
  assert!(logs.contains("SessionLock[DP-1].style panicked"), "{logs}");
}
