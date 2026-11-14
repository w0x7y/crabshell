//! Global shortcuts' bookkeeping: one registration per app id and name,
//! presses told to every watch, and a release in its press's turn told
//! after the flush. A counter stands in for the protocol object, which
//! needs a compositor.

use std::cell::RefCell;
use std::rc::Rc;

use crate::global_shortcuts::{Shortcuts, refusal};

/// Stands in for a registered shortcut, and counts the live ones.
struct Registered(Rc<RefCell<i32>>);

impl Drop for Registered {
  fn drop(&mut self) {
    *self.0.borrow_mut() -= 1;
  }
}

struct Fake {
  shortcuts: Shortcuts<Registered>,
  live: Rc<RefCell<i32>>,
  /// The id each registration's events carry, by name.
  ids: Vec<(String, u64)>,
}

impl Fake {
  fn new() -> Fake {
    Fake {
      shortcuts: Shortcuts::default(),
      live: Rc::new(RefCell::new(0)),
      ids: Vec::new(),
    }
  }

  /// Watches `name` and returns what the watch heard, the watch, and the
  /// id the protocol object's events carry.
  fn watch(&mut self, name: &str) -> (Rc<RefCell<Vec<bool>>>, crate::ShortcutWatch, u64) {
    let heard = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&heard);
    let (watch, id) = self.watch_with(name, Box::new(move |held| log.borrow_mut().push(held)));
    (heard, watch, id)
  }

  fn watch_with(
    &mut self,
    name: &str,
    on_change: Box<dyn FnMut(bool)>,
  ) -> (crate::ShortcutWatch, u64) {
    let live = Rc::clone(&self.live);
    let ids = &mut self.ids;
    let watch = self.shortcuts.watch("crabshell", name, on_change, |id| {
      ids.push((name.to_owned(), id));
      *live.borrow_mut() += 1;
      Registered(live)
    });
    let id = self
      .ids
      .iter()
      .rev()
      .find(|(registered, _)| registered == name)
      .map(|(_, id)| *id)
      .expect("registered");
    (watch, id)
  }
}

#[test]
fn two_watches_share_one_registration_until_both_end() {
  let mut fake = Fake::new();
  let (first, first_watch, id) = fake.watch("launcher");
  let (second, second_watch, again) = fake.watch("launcher");
  assert_eq!(id, again);
  assert_eq!(*fake.live.borrow(), 1, "registered once");
  fake.shortcuts.pressed(id);
  fake.shortcuts.flushed();
  fake.shortcuts.released(id);
  assert_eq!(*first.borrow(), [true, false]);
  assert_eq!(*second.borrow(), [true, false]);

  fake.shortcuts.unwatch(first_watch);
  fake.shortcuts.unwatch(first_watch);
  assert_eq!(*fake.live.borrow(), 1, "the second still watches");
  fake.shortcuts.unwatch(second_watch);
  assert_eq!(*fake.live.borrow(), 0, "destroyed with the last watch");
  assert_eq!(fake.shortcuts.watches(), 0);
  // A press for it afterward finds nothing.
  fake.shortcuts.pressed(id);
  assert_eq!(*second.borrow(), [true, false]);
}

#[test]
fn another_name_is_another_registration() {
  let mut fake = Fake::new();
  let (_, _, launcher) = fake.watch("launcher");
  let (heard, _, lock) = fake.watch("lock");
  assert_ne!(launcher, lock);
  assert_eq!(*fake.live.borrow(), 2);
  fake.shortcuts.pressed(launcher);
  assert!(heard.borrow().is_empty());
}

#[test]
fn a_release_in_its_press_turn_is_told_after_the_flush() {
  let mut fake = Fake::new();
  let (heard, _, id) = fake.watch("launcher");
  fake.shortcuts.pressed(id);
  fake.shortcuts.released(id);
  assert_eq!(*heard.borrow(), [true], "held until the flush shows it");
  assert!(fake.shortcuts.flushed(), "a release to flush");
  assert_eq!(*heard.borrow(), [true, false]);
  assert!(!fake.shortcuts.flushed(), "nothing more");

  // Pressed again before the flush, it stays held.
  fake.shortcuts.pressed(id);
  fake.shortcuts.released(id);
  fake.shortcuts.pressed(id);
  assert!(!fake.shortcuts.flushed());
  assert_eq!(*heard.borrow(), [true, false, true]);
  fake.shortcuts.released(id);
  assert_eq!(*heard.borrow(), [true, false, true, false]);
}

#[test]
fn a_watch_on_a_held_shortcut_hears_it_held_at_once() {
  let mut fake = Fake::new();
  let (_, _, id) = fake.watch("launcher");
  fake.shortcuts.pressed(id);
  let (late, _, _) = fake.watch("launcher");
  assert_eq!(*late.borrow(), [true]);
}

#[test]
fn a_watch_that_panics_is_logged_and_the_others_still_hear() {
  let mut fake = Fake::new();
  let (_, id) = fake.watch_with("launcher", Box::new(|_| panic!("a bad watch")));
  let (heard, _, _) = fake.watch("launcher");
  let logs = super::capture_logs(|| fake.shortcuts.pressed(id));
  assert!(
    logs.contains("a watch of the global shortcut crabshell:launcher panicked: a bad watch"),
    "{logs}"
  );
  assert_eq!(*heard.borrow(), [true]);
}

#[test]
fn names_the_protocol_cant_carry_are_refused() {
  assert_eq!(refusal("crabshell", "launcher", "Show the launcher"), None);
  assert!(refusal("crab\0shell", "launcher", "").is_some());
  assert!(refusal("crabshell", "launcher", &"a".repeat(4000)).is_some());
}

#[test]
fn the_same_name_under_another_app_id_is_another_registration() {
  let mut fake = Fake::new();
  let (heard, _, ours) = fake.watch("toggle");
  let live = Rc::clone(&fake.live);
  let mut theirs = None;
  fake
    .shortcuts
    .watch("other", "toggle", Box::new(|_| {}), |id| {
      theirs = Some(id);
      *live.borrow_mut() += 1;
      Registered(live)
    });
  assert_eq!(*fake.live.borrow(), 2);
  let theirs = theirs.expect("registered on its own");
  assert_ne!(theirs, ours);
  fake.shortcuts.pressed(theirs);
  assert!(heard.borrow().is_empty(), "the other app's shortcut");
}

#[test]
fn a_late_watch_that_panics_on_a_held_shortcut_is_logged() {
  let mut fake = Fake::new();
  let (_, _, id) = fake.watch("launcher");
  fake.shortcuts.pressed(id);
  let logs = crate::tests::capture_logs(|| {
    fake.watch_with("launcher", Box::new(|_| panic!("a broken watch")));
  });
  assert!(logs.contains("a broken watch"), "{logs}");
}
