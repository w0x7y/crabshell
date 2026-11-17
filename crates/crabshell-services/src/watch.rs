//! Watching directories and files for changes, with one inotify instance
//! per shell that sleeps in the loop until the kernel reports one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::rc::Rc;

use calloop::{Interest, RegistrationToken};
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

use crate::Shared;

/// What a directory's watch hears: a file closed after writing, a file
/// or directory made, removed or renamed, and the directory itself
/// removed or renamed. The kernel keeps one mask per inode, so every
/// directory watch takes this one and a listener passes over what it
/// doesn't want. A write to a file still open isn't in it, so a log
/// beside a followed file never wakes the shell.
pub(crate) const DIRECTORY: WatchMask = WatchMask::CLOSE_WRITE
  .union(WatchMask::CREATE)
  .union(WatchMask::DELETE)
  .union(WatchMask::MOVED_FROM)
  .union(WatchMask::MOVED_TO)
  .union(WatchMask::DELETE_SELF)
  .union(WatchMask::MOVE_SELF)
  .union(WatchMask::ONLYDIR);

/// One change in a watched directory.
pub(crate) struct Change {
  /// The file or directory in it that changed. `None` for a change to
  /// the directory itself, and when the kernel lost changes
  /// (`Q_OVERFLOW`), which every watch hears.
  pub(crate) name: Option<OsString>,
  /// What happened, such as `CREATE | ISDIR`.
  pub(crate) mask: EventMask,
}

type Listener = Rc<dyn Fn(&Change)>;

/// The shell's inotify instance, while any watch is alive.
#[derive(Default)]
pub(crate) struct Watcher(RefCell<Option<Instance>>);

impl Watcher {
  /// Whether the inotify instance is open.
  #[cfg(test)]
  pub(crate) fn is_open(&self) -> bool {
    self.0.borrow().is_some()
  }
}

struct Instance {
  inotify: Inotify,
  /// Its descriptor's source in the loop.
  token: RegistrationToken,
  /// Each kernel watch's listeners, with the number of the [`Watch`]
  /// each came from.
  listeners: HashMap<WatchDescriptor, Vec<(u64, Listener)>>,
  next: u64,
}

/// A watch on one directory, from [`watch`]. Dropping it stops its
/// listener, and dropping the last one takes inotify out of the loop.
pub(crate) struct Watch {
  shared: Rc<Shared>,
  wd: WatchDescriptor,
  id: u64,
}

/// Calls `on_change` for each change `mask` names at `path`, a directory
/// with [`DIRECTORY`] or another inode with a mask of its own, until the
/// returned [`Watch`] drops. An inode watched twice has two listeners,
/// and the kernel keeps the last mask it was given. One that drops may
/// still hear the changes read along with its last.
pub(crate) fn watch(
  shared: &Rc<Shared>,
  path: &Path,
  mask: WatchMask,
  on_change: impl Fn(&Change) + 'static,
) -> io::Result<Watch> {
  let mut slot = shared.watcher.0.borrow_mut();
  if slot.is_none() {
    *slot = Some(Instance::new(shared)?);
  }
  let instance = slot.as_mut().expect("invariant: made above");
  let wd = match instance.inotify.watches().add(path, mask) {
    Ok(wd) => wd,
    Err(error) => {
      if instance.listeners.is_empty() {
        let token = instance.token;
        *slot = None;
        drop(slot);
        remove(shared, token);
      }
      return Err(error);
    }
  };
  instance.next += 1;
  let id = instance.next;
  let listeners = instance.listeners.entry(wd.clone()).or_default();
  listeners.push((id, Rc::new(on_change)));
  Ok(Watch {
    shared: Rc::clone(shared),
    wd,
    id,
  })
}

impl Instance {
  /// A new inotify instance, waited on by the loop.
  fn new(shared: &Rc<Shared>) -> io::Result<Instance> {
    let inotify = Inotify::init()?;
    let fd = inotify.as_fd().try_clone_to_owned()?;
    // Weak, as the last watch takes it out of the loop.
    let weak = Rc::downgrade(shared);
    let ready = Box::new(move || {
      if let Some(shared) = weak.upgrade() {
        dispatch(&shared);
      }
    });
    let token = shared
      .event_loop()
      .and_then(|event_loop| event_loop.insert_fd(fd, Interest::READ, ready))
      .ok_or_else(|| io::Error::other("the event loop can't wait on inotify"))?;
    Ok(Instance {
      inotify,
      token,
      listeners: HashMap::new(),
      next: 0,
    })
  }
}

/// Takes inotify's descriptor out of the loop.
fn remove(shared: &Shared, token: RegistrationToken) {
  if let Some(event_loop) = shared.event_loop() {
    event_loop.remove(token);
  }
}

impl Drop for Watch {
  fn drop(&mut self) {
    let mut slot = self.shared.watcher.0.borrow_mut();
    let Some(instance) = slot.as_mut() else {
      return;
    };
    if let Some(listeners) = instance.listeners.get_mut(&self.wd) {
      listeners.retain(|(id, _)| *id != self.id);
      if listeners.is_empty() {
        instance.listeners.remove(&self.wd);
        // Gone already if the directory went.
        let _ = instance.inotify.watches().remove(self.wd.clone());
      }
    }
    if instance.listeners.is_empty() {
      let token = instance.token;
      *slot = None;
      drop(slot);
      remove(&self.shared, token);
    }
  }
}

/// Reads what the kernel reported and tells each listener.
fn dispatch(shared: &Shared) {
  let mut heard: Vec<(Listener, Change)> = Vec::new();
  {
    let mut slot = shared.watcher.0.borrow_mut();
    let Some(instance) = slot.as_mut() else {
      return;
    };
    let mut buffer = [0_u8; 4096];
    loop {
      let events = match instance.inotify.read_events(&mut buffer) {
        Ok(events) => events,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
        Err(error) => {
          tracing::warn!(%error, "can't read file changes");
          break;
        }
      };
      for event in events {
        let lost = event.mask.contains(EventMask::Q_OVERFLOW);
        let listeners = instance
          .listeners
          .iter()
          .filter(|(wd, _)| lost || **wd == event.wd)
          .flat_map(|(_, listeners)| listeners);
        for (_, listener) in listeners {
          let name = event.name.map(OsString::from);
          heard.push((
            Rc::clone(listener),
            Change {
              name,
              mask: event.mask,
            },
          ));
        }
      }
    }
  }
  // Outside the borrow, since a listener may add or drop watches.
  for (listener, change) in heard {
    listener(&change);
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::ffi::OsString;
  use std::rc::Rc;
  use std::time::{Duration, Instant};

  use inotify::EventMask;

  use super::{DIRECTORY, watch};
  use crate::Shared;
  use crate::testing::{Harness, TempDir, offline};

  fn shared(shell: &Harness) -> Rc<Shared> {
    Rc::new(Shared::new(
      shell.cx.owner(),
      &shell.event_loop.handle(),
      offline(),
    ))
  }

  type Heard = Rc<RefCell<Vec<(Option<OsString>, EventMask)>>>;

  /// A listener that keeps what it hears.
  fn heard() -> (Heard, impl Fn(&super::Change)) {
    let heard: Heard = Rc::default();
    let sink = Rc::clone(&heard);
    let listener = move |change: &super::Change| {
      sink.borrow_mut().push((change.name.clone(), change.mask));
    };
    (heard, listener)
  }

  fn run(shell: &mut Harness, ms: u64) -> usize {
    shell.run_until(Instant::now() + Duration::from_millis(ms))
  }

  #[test]
  fn a_change_calls_back_until_the_watch_drops() {
    let mut shell = Harness::new(offline());
    let shared = shared(&shell);
    let dir = TempDir::new();
    let (heard, listener) = heard();
    let watch = watch(&shared, dir.path(), DIRECTORY, listener).expect("a watch");
    // Asleep until something changes.
    assert_eq!(run(&mut shell, 200), 1);

    std::fs::write(dir.path().join("a.desktop"), "x").expect("a file");
    std::fs::create_dir(dir.path().join("sub")).expect("a directory");
    run(&mut shell, 100);
    let names: Vec<_> = heard
      .borrow()
      .iter()
      .map(|(name, _)| name.clone())
      .collect();
    assert!(names.contains(&Some("a.desktop".into())), "{names:?}");
    let made_dir = heard.borrow().iter().any(|(name, mask)| {
      name.as_deref() == Some("sub".as_ref()) && mask.contains(EventMask::CREATE | EventMask::ISDIR)
    });
    assert!(made_dir, "{:?}", heard.borrow());

    // The last watch takes inotify out of the loop, whose callback held
    // the one weak reference.
    assert_eq!(Rc::weak_count(&shared), 1);
    drop(watch);
    assert_eq!(Rc::weak_count(&shared), 0);
    assert!(shared.watcher.0.borrow().is_none());
    heard.borrow_mut().clear();
    std::fs::write(dir.path().join("b.desktop"), "x").expect("a file");
    assert_eq!(run(&mut shell, 100), 1);
    assert!(heard.borrow().is_empty(), "{:?}", heard.borrow());
  }

  #[test]
  fn two_watches_on_one_directory_each_hear_it() {
    let mut shell = Harness::new(offline());
    let shared = shared(&shell);
    let dir = TempDir::new();
    let (first, listener) = heard();
    let first_watch = watch(&shared, dir.path(), DIRECTORY, listener).expect("a watch");
    let (second, listener) = heard();
    let _second_watch = watch(&shared, dir.path(), DIRECTORY, listener).expect("a watch");

    std::fs::write(dir.path().join("a"), "x").expect("a file");
    run(&mut shell, 100);
    assert!(!first.borrow().is_empty());
    assert!(!second.borrow().is_empty());

    drop(first_watch);
    first.borrow_mut().clear();
    second.borrow_mut().clear();
    std::fs::write(dir.path().join("b"), "x").expect("a file");
    run(&mut shell, 100);
    assert!(first.borrow().is_empty(), "{:?}", first.borrow());
    assert!(!second.borrow().is_empty());
  }

  #[test]
  fn a_missing_directory_is_refused_and_leaves_nothing() {
    let shell = Harness::new(offline());
    let shared = shared(&shell);
    let dir = TempDir::new();
    let (_, listener) = heard();
    assert!(watch(&shared, &dir.path().join("missing"), DIRECTORY, listener).is_err());
    assert_eq!(Rc::weak_count(&shared), 0);
    assert!(shared.watcher.0.borrow().is_none());
  }
}
