//! Watching the config for edits: its `Cargo.toml`, and every file under
//! `src/`, with inotify.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

/// The events that mean a file changed, appeared or went: a write that
/// finished, and the renames and removals editors save with.
const CHANGES: WatchMask = WatchMask::CLOSE_WRITE
  .union(WatchMask::CREATE)
  .union(WatchMask::DELETE)
  .union(WatchMask::MOVED_FROM)
  .union(WatchMask::MOVED_TO);

/// The config's watches.
pub(crate) struct Watcher {
  inotify: Inotify,
  config: PathBuf,
  /// Each watched path under `src/`, by its watch.
  dirs: HashMap<WatchDescriptor, PathBuf>,
  /// The config directory's own watch.
  root: WatchDescriptor,
}

impl Watcher {
  /// Watches `config`'s `Cargo.toml` and `src/`.
  pub(crate) fn new(config: &Path) -> std::io::Result<Watcher> {
    let inotify = Inotify::init()?;
    let root = inotify.watches().add(config, CHANGES)?;
    let mut watcher = Watcher {
      inotify,
      config: config.to_owned(),
      dirs: HashMap::new(),
      root,
    };
    watcher.watch_tree(&config.join("src"));
    Ok(watcher)
  }

  /// A copy of the inotify descriptor, for the loop to wait on.
  pub(crate) fn fd(&self) -> std::io::Result<OwnedFd> {
    self.inotify.as_fd().try_clone_to_owned()
  }

  /// Reads what happened since last time, and says whether the config
  /// changed. New directories under `src/` are watched too, and ones
  /// moved out of it no longer are.
  pub(crate) fn changed(&mut self) -> bool {
    let mut buffer = [0_u8; 4096];
    let mut changed = false;
    let mut new_dirs = Vec::new();
    let mut gone = Vec::new();
    loop {
      let events = match self.inotify.read_events(&mut buffer) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
        Err(error) => {
          crate::log(&format!("can't read file changes: {error}"));
          break;
        }
      };
      let mut any = false;
      for event in events {
        any = true;
        if event.mask.contains(EventMask::Q_OVERFLOW) {
          // Events were lost, new directories' among them.
          changed = true;
          new_dirs.push(self.config.join("src"));
          continue;
        }
        if event.mask.contains(EventMask::IGNORED) {
          self.dirs.remove(&event.wd);
          continue;
        }
        let name = event.name.unwrap_or_default();
        let is_dir = event.mask.contains(EventMask::ISDIR);
        let arrived = event
          .mask
          .intersects(EventMask::CREATE | EventMask::MOVED_TO);
        let path = if event.wd == self.root {
          if name == OsStr::new("Cargo.toml") {
            changed = true;
            continue;
          }
          if name != OsStr::new("src") || !is_dir {
            continue;
          }
          changed = true;
          self.config.join("src")
        } else if let Some(dir) = self.dirs.get(&event.wd) {
          changed |= relevant(name);
          dir.join(name)
        } else {
          continue;
        };
        // A new symlink is watched for what it points at.
        if arrived && (is_dir || path.is_symlink()) {
          new_dirs.push(path);
        } else if is_dir && event.mask.contains(EventMask::MOVED_FROM) {
          gone.push(path);
        }
      }
      if !any {
        break;
      }
    }
    // A directory moved away is no longer the config's.
    for path in gone {
      let watches: Vec<WatchDescriptor> = (self.dirs.iter())
        .filter(|(_, dir)| dir.starts_with(&path))
        .map(|(watch, _)| watch.clone())
        .collect();
      for watch in watches {
        self.dirs.remove(&watch);
        let _ = self.inotify.watches().remove(watch);
      }
    }
    for dir in new_dirs {
      self.watch_tree(&dir);
    }
    changed
  }

  /// Watches `dir` and every directory under it, and each symlink's
  /// target, since a write through a link isn't seen in the link's
  /// directory. One that can't be read is logged and skipped.
  fn watch_tree(&mut self, dir: &Path) {
    match self.inotify.watches().add(dir, CHANGES) {
      // Watched already, as through a symlink loop.
      Ok(watch) if self.dirs.contains_key(&watch) => return,
      Ok(watch) => {
        self.dirs.insert(watch, dir.to_owned());
      }
      Err(error) => {
        if error.kind() != std::io::ErrorKind::NotFound {
          crate::log(&format!("can't watch {}: {error}", dir.display()));
        }
        return;
      }
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
      return;
    };
    for entry in entries.flatten() {
      if entry
        .file_type()
        .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
      {
        self.watch_tree(&entry.path());
      }
    }
  }
}

/// Whether a change to `name` under `src/` can change the build: not an
/// editor's hidden swap, backup or auto-save file.
pub(crate) fn relevant(name: &OsStr) -> bool {
  let name = name.to_string_lossy();
  let auto_save = name.len() > 1 && name.starts_with('#') && name.ends_with('#');
  !(name.starts_with('.') || name.ends_with('~') || name == "4913" || auto_save)
}
