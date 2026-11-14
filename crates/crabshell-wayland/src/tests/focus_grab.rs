//! Focus grabs' bookkeeping: which windows each grab holds, what changes
//! are sent, and when the grab's signal turns false. Numbers stand in for
//! surfaces, and a log for the protocol object, which needs a compositor.

use std::cell::RefCell;
use std::rc::Rc;

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{Cx, Text, Window};

use crate::focus_grab::{Grabs, wanted};
use crate::windows::Windows;

/// What happened to the fake grabs.
type Log = Rc<RefCell<Vec<String>>>;

/// Stands in for a grab object, and logs being destroyed.
struct Fake {
  id: u64,
  log: Log,
}

impl Drop for Fake {
  fn drop(&mut self) {
    self.log.borrow_mut().push(format!("destroy {}", self.id));
  }
}

/// Syncs `grabs` with `wanted`, logging what is made and changed.
fn sync(grabs: &mut Grabs<Fake, u32>, log: &Log, wanted: Vec<(Signal<bool>, Vec<u32>)>) -> bool {
  grabs.sync(
    wanted,
    |id| {
      log.borrow_mut().push(format!("make {id}"));
      Fake {
        id,
        log: Rc::clone(log),
      }
    },
    |grab, added, removed| {
      log
        .borrow_mut()
        .push(format!("{} +{added:?} -{removed:?}", grab.id));
    },
  )
}

fn taken(log: &Log) -> Vec<String> {
  std::mem::take(&mut *log.borrow_mut())
}

#[test]
fn a_grab_is_made_once_and_sent_only_what_changes() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let grab = cx.signal(true);
  let (mut grabs, log) = (Grabs::default(), Log::default());
  sync(&mut grabs, &log, vec![(grab, vec![1, 2])]);
  assert_eq!(taken(&log), ["make 0", "0 +[1, 2] -[]"]);
  sync(&mut grabs, &log, vec![(grab, vec![1, 2])]);
  assert!(taken(&log).is_empty(), "nothing changed");
  sync(&mut grabs, &log, vec![(grab, vec![2, 3])]);
  assert_eq!(taken(&log), ["0 +[3] -[1]"]);
  assert!(grab.get_untracked());
}

#[test]
fn a_signal_turned_false_ends_its_grab_and_true_asks_again() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let grab = cx.signal(true);
  let (mut grabs, log) = (Grabs::default(), Log::default());
  sync(&mut grabs, &log, vec![(grab, vec![1])]);
  taken(&log);
  grab.set(false);
  assert!(!sync(&mut grabs, &log, Vec::new()), "nothing to flush");
  assert_eq!(taken(&log), ["destroy 0"]);
  assert!(!grab.get_untracked());
  grab.set(true);
  sync(&mut grabs, &log, vec![(grab, vec![1])]);
  assert_eq!(taken(&log), ["make 1", "1 +[1] -[]"]);
}

#[test]
fn a_grab_whose_windows_all_went_ends_and_turns_false() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let grab = cx.signal(true);
  let other = cx.signal(true);
  let (mut grabs, log) = (Grabs::default(), Log::default());
  sync(&mut grabs, &log, vec![(grab, vec![1]), (other, vec![2])]);
  taken(&log);
  assert!(
    sync(&mut grabs, &log, vec![(other, vec![2])]),
    "a signal to flush"
  );
  assert_eq!(taken(&log), ["destroy 0"]);
  assert!(!grab.get_untracked(), "cleared, as the compositor would");
  assert!(other.get_untracked());
}

#[test]
fn the_compositor_clearing_a_grab_ends_it_and_turns_false() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let grab = cx.signal(true);
  let (mut grabs, log) = (Grabs::default(), Log::default());
  sync(&mut grabs, &log, vec![(grab, vec![1])]);
  taken(&log);
  grabs.cleared(7);
  assert!(grab.get_untracked(), "another grab's id");
  grabs.cleared(0);
  assert_eq!(taken(&log), ["destroy 0"]);
  assert!(!grab.get_untracked());
}

#[test]
fn a_grab_whose_signal_is_gone_ends_quietly() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let inner = cx.child();
  let grab = inner.signal(true);
  let (mut grabs, log) = (Grabs::default(), Log::default());
  sync(&mut grabs, &log, vec![(grab, vec![1])]);
  taken(&log);
  inner.owner().dispose();
  sync(&mut grabs, &log, Vec::new());
  assert_eq!(taken(&log), ["destroy 0"]);
  assert_eq!(grabs.len(), 0);
}

/// A window for DP-1 that grabs with `grab`, if given.
fn window(cx: &Cx, grab: Option<Signal<bool>>) -> Window {
  let panel = super::panel("DP-1").child(Text::new("grab"));
  let panel = match grab {
    Some(grab) => panel.focus_grab(grab),
    None => panel,
  };
  Window::new(cx, panel)
}

#[test]
fn windows_sharing_a_true_signal_and_their_popups_make_one_grab() {
  let runtime = Runtime::current();
  let cx = Cx::new(runtime.new_owner());
  let (shared, off, alone) = (cx.signal(true), cx.signal(false), cx.signal(true));
  // The surface stands for the number the grab holds; `None` is a window
  // not shown yet.
  let mut windows: Windows<u32, ()> = Windows::new();
  let first = windows.add(window(&cx, Some(shared)));
  windows.add(window(&cx, Some(off)));
  windows.add(window(&cx, None));
  windows.add(window(&cx, Some(shared)));
  windows.add(window(&cx, Some(alone)));
  windows.add(window(&cx, Some(shared)));
  for (index, surface) in [(0, 10), (1, 11), (2, 12), (3, 13), (4, 14)] {
    windows.entries[index].surface = Some(surface);
  }
  let popup = windows
    .add_popup(first, window(&cx, None))
    .expect("its opener is held");
  let index = windows.entries.len() - 1;
  assert_eq!(windows.entries[index].id, popup);
  windows.entries[index].surface = Some(15);

  assert_eq!(
    wanted(&windows, |surface| Some(*surface)),
    [(shared, vec![10, 13, 15]), (alone, vec![14])]
  );
  // A surface not mapped yet isn't held.
  assert_eq!(
    wanted(&windows, |surface: &u32| (*surface != 13)
      .then_some(*surface)),
    [(shared, vec![10, 15]), (alone, vec![14])]
  );
}
