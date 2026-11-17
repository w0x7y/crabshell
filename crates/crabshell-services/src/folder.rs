//! A folder's entries, followed as they change, as Qt's
//! `FolderListModel` gives a wallpaper picker.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::os::unix::fs::MetadataExt;
use std::panic::{self, AssertUnwindSafe, Location};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;

use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::{Cx, IntoProp};

use crate::Shared;
use crate::file::{FileError, FileErrorKind, Files, Follow, Keep, set};
use crate::lines::panic_message;
use crate::watch::{self, Watch};

/// The most entries a folder view lists; the rest are dropped, logged.
const MAX_ENTRIES: usize = 10_000;

/// How long after a change the folder is read, so a burst of changes,
/// or a program writing steadily, reads it at most 20 times a second.
const SETTLE: Duration = Duration::from_millis(50);

#[cfg(test)]
thread_local! {
  /// How many times this thread's folder views read their folder.
  static READS: Cell<usize> = const { Cell::new(0) };
}

/// A folder's entries, from `cx.folder_view(path)`, read at the next flush
/// and again when the folder changes. A `Copy` handle, like a signal.
#[derive(Clone, Copy)]
pub struct FolderView {
  entries: Signal<Vec<FolderEntry>>,
  error: Signal<Option<FileError>>,
}

/// A file, directory or link in a folder, from [`FolderView::entries`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
  /// The folder's path joined with the name.
  pub path: PathBuf,
  /// The name in the folder, such as `beach.png`.
  pub name: String,
  /// Whether it is a directory or a link to one.
  pub is_dir: bool,
  /// Its size in bytes; for a link, its target's.
  pub size: u64,
  /// When it was last written, if the system says.
  pub modified: Option<SystemTime>,
}

impl FolderView {
  /// The folder's entries, one level, hidden ones too, sorted by name
  /// regardless of case, then byte by byte. Empty while the folder can't
  /// be read; [`error`](Self::error) says why.
  pub fn entries(&self) -> ReadSignal<Vec<FolderEntry>> {
    self.entries.read_only()
  }

  /// Why the last read failed, or `None` once one worked. A file at the
  /// path is [`NotAFile`](FileErrorKind::NotAFile).
  pub fn error(&self) -> ReadSignal<Option<FileError>> {
    self.error.read_only()
  }
}

/// `cx.folder_view(path)`.
pub(crate) fn folder_view(
  cx: &Cx,
  path: impl IntoProp<PathBuf>,
  at: &'static Location<'static>,
) -> FolderView {
  let view = FolderView {
    entries: cx.signal(Vec::new()),
    error: cx.signal(None),
  };
  let shared = cx.service::<Files>().map(|files| Rc::clone(&files.0));
  let path = path.into_prop().shared();
  // A memo, so a closure that reads other signals follows a new folder
  // only when the path itself changes.
  let path = cx.memo(move || {
    panic::catch_unwind(AssertUnwindSafe(|| path()))
      .inspect_err(|payload| {
        let message = panic_message(payload.as_ref());
        tracing::error!("cx.folder_view (called at {at}) panicked choosing its path: {message}");
      })
      .ok()
  });
  // Each run's scope holds the folder it follows, so a new path or the
  // owner going stops following the old one.
  cx.owner().effect_scoped(move |scope| match path.get() {
    Some(path) => {
      let folder = Folder::new(shared.clone(), path, view);
      scope.signal(Keep(folder));
    }
    None => {
      set(view.entries, Vec::new());
      set(view.error, None);
    }
  });
  view
}

/// One folder followed for a view.
struct Folder {
  /// `None` on a context the shell didn't set up: read once, never
  /// followed.
  shared: Option<Rc<Shared>>,
  path: PathBuf,
  view: FolderView,
  /// Follows the path through its parents, for the folder made, removed
  /// or replaced.
  follow: RefCell<Option<Rc<Follow>>>,
  /// The watch on the folder itself, with its device and inode.
  own: RefCell<Option<(Watch, (u64, u64))>>,
  /// The timer of a read waiting for [`SETTLE`] to pass.
  waiting: Cell<Option<RegistrationToken>>,
  logged_names: Cell<bool>,
  logged_cap: Cell<bool>,
}

impl Folder {
  fn new(shared: Option<Rc<Shared>>, path: PathBuf, view: FolderView) -> Rc<Folder> {
    let folder = Rc::new(Folder {
      shared,
      path,
      view,
      follow: RefCell::new(None),
      own: RefCell::new(None),
      waiting: Cell::new(None),
      logged_names: Cell::new(false),
      logged_cap: Cell::new(false),
    });
    if let Some(shared) = &folder.shared {
      let weak = Rc::downgrade(&folder);
      let follow = Follow::new(shared, folder.path.clone(), move || {
        if let Some(folder) = weak.upgrade() {
          folder.queue();
        }
      });
      folder.follow.replace(Some(follow));
    }
    folder.read();
    folder
  }

  /// Reads the folder [`SETTLE`] from now, on a timer that exists only
  /// while it waits, unless a read already waits.
  fn queue(self: &Rc<Self>) {
    if self.waiting.get().is_some() {
      return;
    }
    let Some(event_loop) = self.shared.as_ref().and_then(|shared| shared.event_loop()) else {
      return;
    };
    let weak = Rc::downgrade(self);
    let tick = Box::new(move || {
      if let Some(folder) = weak.upgrade() {
        folder.waiting.set(None);
        folder.read();
      }
      TimeoutAction::Drop
    });
    self
      .waiting
      .set(event_loop.insert_timer(Instant::now() + SETTLE, tick));
  }

  /// Reads the folder now and shows what it holds.
  fn read(self: &Rc<Self>) {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    let meta = std::fs::metadata(&self.path);
    let dir = meta.as_ref().ok().filter(|meta| meta.is_dir());
    // Watched before it is listed, so no change between is missed.
    self.watch_own(dir.map(|meta| (meta.dev(), meta.ino())));
    let listed = match meta {
      Err(error) => Err(FileError::from(error)),
      Ok(meta) if !meta.is_dir() => Err(FileError::new(
        FileErrorKind::NotAFile,
        "it isn't a directory",
      )),
      Ok(_) => self.list(),
    };
    let (entries, error) = match listed {
      Ok(entries) => (entries, None),
      Err(error) => {
        // Once, while the same error stays.
        let view = self.view;
        if untrack(|| view.error.try_with(|old| old.as_ref() != Some(&error))) == Some(true) {
          let path = self.path.display();
          tracing::info!(%path, error = error.message, "can't read the folder");
        }
        (Vec::new(), Some(error))
      }
    };
    set(self.view.entries, entries);
    set(self.view.error, error);
  }

  /// Watches the folder at `id`, the device and inode of what is at the
  /// path now, unless it is watched already; `None` drops the watch.
  fn watch_own(self: &Rc<Self>, id: Option<(u64, u64)>) {
    let Some(shared) = &self.shared else {
      return;
    };
    if self.own.borrow().as_ref().map(|(_, watched)| *watched) == id {
      return;
    }
    self.own.replace(None);
    let Some(id) = id else {
      return;
    };
    let weak = Rc::downgrade(self);
    let listener = move |_: &watch::Change| {
      if let Some(folder) = weak.upgrade() {
        folder.queue();
      }
    };
    // shortcut: a folder swapped in between the stat and the add is
    // watched under the old one's id, and rewatched at the next change.
    match watch::watch(shared, &self.path, watch::DIRECTORY, listener) {
      Ok(watch) => {
        self.own.replace(Some((watch, id)));
      }
      Err(error) => {
        let path = self.path.display();
        tracing::info!(%path, %error, "can't watch the folder, so changes in it aren't seen");
      }
    }
  }

  /// The folder's entries, sorted, the first [`MAX_ENTRIES`] of them.
  fn list(&self) -> Result<Vec<FolderEntry>, FileError> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&self.path)? {
      let entry = match entry {
        Ok(entry) => entry,
        Err(error) => {
          let path = self.path.display();
          tracing::debug!(%path, %error, "can't read an entry in the folder");
          continue;
        }
      };
      let Ok(name) = entry.file_name().into_string() else {
        if !self.logged_names.replace(true) {
          let path = self.path.display();
          tracing::info!(%path, "a name in the folder isn't UTF-8, so it is left out");
        }
        continue;
      };
      names.push(name);
    }
    names.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
    if names.len() > MAX_ENTRIES {
      names.truncate(MAX_ENTRIES);
      if !self.logged_cap.replace(true) {
        let path = self.path.display();
        tracing::info!(%path, "the folder holds over 10000 entries; the rest are left out");
      }
    }
    let entries = names.into_iter().map(|name| {
      let path = self.path.join(&name);
      // A link counts as what it names, and as itself when that's gone.
      let meta = std::fs::metadata(&path)
        .or_else(|_| std::fs::symlink_metadata(&path))
        .ok();
      FolderEntry {
        is_dir: meta.as_ref().is_some_and(std::fs::Metadata::is_dir),
        size: meta.as_ref().map_or(0, std::fs::Metadata::len),
        modified: meta.and_then(|meta| meta.modified().ok()),
        path,
        name,
      }
    });
    Ok(entries.collect())
  }
}

impl Drop for Folder {
  fn drop(&mut self) {
    let event_loop = self.shared.as_ref().and_then(|shared| shared.event_loop());
    if let (Some(token), Some(event_loop)) = (self.waiting.take(), event_loop) {
      event_loop.remove(token);
    }
  }
}
