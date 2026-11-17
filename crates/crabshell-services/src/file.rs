//! Files a config reads and writes, as Quickshell's `FileView` and
//! `JsonAdapter`.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use inotify::{EventMask, WatchMask};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::Shared;
use crate::lines::panic_message;
use crate::watch::{self, Watch};

/// The largest file read; a larger one reads as missing, and that is
/// logged.
pub(crate) const MAX_FILE: u64 = 16 * 1024 * 1024;

/// How long after a write to a file still open it is read, unless its
/// writer closes it first: a log shows its lines within a frame or
/// three, and one written in many pieces is read once per wait, not per
/// piece.
const WRITING: Duration = Duration::from_millis(50);

/// What `cx.file_view` and `cx.json_file` need from the shell: the loop,
/// to watch files.
pub(crate) struct Files(pub(crate) Rc<Shared>);

/// Follows the file at a path through whatever happens to the directories
/// on the way: one not made yet is waited for from the nearest one that
/// is there, and one removed, renamed or made again is followed anew.
// shortcut: watches the directory the path names, not a link's target's,
// so a target replaced by a rename in its own directory isn't seen, while
// writes to it are, through its inode; follow the link when a config
// needs that.
pub(crate) struct Follow {
  shared: Rc<Shared>,
  path: PathBuf,
  watch: RefCell<Option<Watch>>,
  /// A watch on the file's own inode for writes while it is open, as its
  /// directory's watch hears only their close.
  file: RefCell<Option<Watch>>,
  /// The timer that reads a file still being written, while one waits.
  writing: Cell<Option<RegistrationToken>>,
  /// Reads the file again.
  changed: Box<dyn Fn()>,
}

impl Follow {
  /// Calls `changed` when the file at `path` may have changed: written,
  /// replaced or removed.
  pub(crate) fn new(
    shared: &Rc<Shared>,
    path: PathBuf,
    changed: impl Fn() + 'static,
  ) -> Rc<Follow> {
    let follow = Rc::new(Follow {
      shared: Rc::clone(shared),
      path,
      watch: RefCell::new(None),
      file: RefCell::new(None),
      writing: Cell::new(None),
      changed: Box::new(changed),
    });
    follow.rewatch();
    follow
  }

  /// Watches the file's directory, or while it is missing the nearest
  /// directory above it that is there, for the next name on the way.
  fn rewatch(self: &Rc<Self>) {
    self.watch.replace(None);
    self.file.replace(None);
    let (Some(dir), Some(name)) = (self.path.parent(), self.path.file_name()) else {
      return;
    };
    let mut dir = if dir.as_os_str().is_empty() {
      Path::new(".")
    } else {
      dir
    };
    let mut next: OsString = name.to_owned();
    let own = dir.is_dir();
    while !dir.is_dir() {
      let (Some(above), Some(name)) = (dir.parent(), dir.file_name()) else {
        return;
      };
      next = name.to_owned();
      dir = if above.as_os_str().is_empty() {
        Path::new(".")
      } else {
        above
      };
    }
    let weak = Rc::downgrade(self);
    let listener = move |change: &watch::Change| {
      let Some(follow) = weak.upgrade() else {
        return;
      };
      let moved = EventMask::DELETE_SELF | EventMask::MOVE_SELF;
      match &change.name {
        // The directory itself went, or changes were lost: start over.
        None => {
          if change.mask.intersects(moved) || change.mask.contains(EventMask::Q_OVERFLOW) {
            follow.rewatch();
          }
          follow.read();
        }
        // Not on a file's create, when a writer hasn't written yet: its
        // writes or its close come next. One that wrote before the
        // watch on the new file was in is read after [`WRITING`].
        Some(changed) if *changed == next && own => {
          follow.watch_file();
          if !change.mask.contains(EventMask::CREATE) || change.mask.contains(EventMask::ISDIR) {
            follow.read();
          } else if follow.path.metadata().is_ok_and(|meta| meta.len() > 0) {
            follow.written();
          }
        }
        // A directory on the way came or went.
        Some(changed) if *changed == next => {
          follow.rewatch();
          follow.read();
        }
        Some(_) => {}
      }
    };
    match watch::watch(&self.shared, dir, watch::DIRECTORY, listener) {
      Ok(watch) => {
        self.watch.replace(Some(watch));
      }
      Err(error) => {
        let path = self.path.display();
        tracing::info!(%path, %error, "can't watch the file, so changes to it aren't seen");
      }
    }
    if own {
      self.watch_file();
    }
  }

  /// Watches the regular file at the path, if one is there, for writes
  /// while it is open; a directory or a pipe is left to its directory's
  /// watch, as the kernel keeps one mask per inode.
  fn watch_file(self: &Rc<Self>) {
    if !self.path.metadata().is_ok_and(|meta| meta.is_file()) {
      self.file.replace(None);
      return;
    }
    let weak = Rc::downgrade(self);
    let listener = move |change: &watch::Change| {
      if let Some(follow) = weak
        .upgrade()
        .filter(|_| change.mask.contains(EventMask::MODIFY))
      {
        follow.written();
      }
    };
    // The new one first, so the same inode keeps its kernel watch.
    // shortcut: a directory swapped in for the file between the check and
    // the add would get this mask in place of its own, and dropping ours
    // wouldn't give it back; watch by descriptor if that ever happens.
    let watched = watch::watch(&self.shared, &self.path, WatchMask::MODIFY, listener);
    if let Err(error) = &watched {
      let path = self.path.display();
      tracing::debug!(%path, %error, "can't watch the file's writes; its close still counts");
    }
    self.file.replace(watched.ok());
  }
}

impl Follow {
  /// The file was written and is still open: reads it after [`WRITING`],
  /// on a timer that exists only while it waits, unless a close or
  /// another change reads it first.
  fn written(self: &Rc<Self>) {
    if self.writing.get().is_some() {
      return;
    }
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let weak = Rc::downgrade(self);
    let tick = Box::new(move || {
      if let Some(follow) = weak.upgrade() {
        follow.writing.set(None);
        (follow.changed)();
      }
      TimeoutAction::Drop
    });
    let token = event_loop.insert_timer(Instant::now() + WRITING, tick);
    self.writing.set(token);
  }

  /// Reads the file now, so a read waiting for a writer is dropped.
  fn read(&self) {
    self.stop_waiting();
    (self.changed)();
  }

  fn stop_waiting(&self) {
    if let (Some(token), Some(event_loop)) = (self.writing.take(), self.shared.event_loop()) {
      event_loop.remove(token);
    }
  }
}

impl Drop for Follow {
  fn drop(&mut self) {
    self.stop_waiting();
  }
}

/// Keeps a file's watch for as long as the opening context's owner.
pub(crate) struct Keep<T>(#[allow(dead_code, reason = "held for its drop")] pub(crate) Rc<T>);

/// A text file, from `cx.file_view(path)`. Its text is read at once and
/// again when it changes on disk. A clone is cheap and shares it.
#[derive(Clone)]
pub struct FileView {
  inner: Rc<ViewInner>,
}

/// Why a file can't be read, from [`FileView::error`], as Quickshell's
/// `FileViewError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileError {
  /// What kind of failure it is.
  pub kind: FileErrorKind,
  /// What the system said, such as `No such file or directory (os error
  /// 2)`, or the shell, such as `it isn't UTF-8`.
  pub message: String,
}

/// The kinds of [`FileError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileErrorKind {
  /// Nothing is at the path.
  NotFound,
  /// The shell may not read it.
  PermissionDenied,
  /// Something is there, but it isn't a regular file, such as a directory
  /// or a pipe.
  NotAFile,
  /// Anything else, such as a file that isn't UTF-8 or is over 16 MiB.
  Other,
}

impl FileError {
  pub(crate) fn new(kind: FileErrorKind, message: &str) -> FileError {
    let message = message.to_owned();
    FileError { kind, message }
  }
}

impl From<std::io::Error> for FileError {
  fn from(error: std::io::Error) -> FileError {
    let kind = match error.kind() {
      ErrorKind::NotFound => FileErrorKind::NotFound,
      ErrorKind::PermissionDenied => FileErrorKind::PermissionDenied,
      ErrorKind::IsADirectory => FileErrorKind::NotAFile,
      _ => FileErrorKind::Other,
    };
    FileError::new(kind, &error.to_string())
  }
}

struct ViewInner {
  path: PathBuf,
  follow: RefCell<Option<Rc<Follow>>>,
  /// These live under the opening context's owner.
  text: Signal<Option<String>>,
  error: Signal<Option<FileError>>,
}

/// `cx.file_view(path)`.
pub(crate) fn file_view(cx: &Cx, path: PathBuf) -> FileView {
  let inner = Rc::new(ViewInner {
    text: cx.signal(None),
    error: cx.signal(None),
    follow: RefCell::new(None),
    path,
  });
  inner.reread();
  if let Some(files) = cx.service::<Files>() {
    let weak = Rc::downgrade(&inner);
    let follow = Follow::new(&files.0, inner.path.clone(), move || {
      if let Some(inner) = Weak::upgrade(&weak) {
        inner.reread();
      }
    });
    inner.follow.replace(Some(follow));
  }
  // So `text()` follows the file even once every `FileView` is dropped.
  cx.owner().signal(Keep(Rc::clone(&inner)));
  FileView { inner }
}

impl FileView {
  /// The file's text, or `None` while it is missing, can't be read, isn't
  /// UTF-8, or is over 16 MiB, each logged once until it reads again.
  /// [`error`](Self::error) says why.
  pub fn text(&self) -> ReadSignal<Option<String>> {
    self.inner.text.read_only()
  }

  /// Why the last read failed, or `None` once a read or a
  /// [`write`](Self::write) worked. A pipe, a directory or a device is
  /// [`NotAFile`](FileErrorKind::NotAFile) and never opened.
  pub fn error(&self) -> ReadSignal<Option<FileError>> {
    self.inner.error.read_only()
  }

  /// Replaces the file's text, through a new file beside it renamed over
  /// it, so a reader never sees half of it, making its directory if it is
  /// missing. A link is followed and the file it names is replaced,
  /// keeping its permissions. A failure is logged and the text stays as
  /// it was.
  pub fn write(&self, text: impl AsRef<str>) {
    let text = text.as_ref();
    match write_whole(&self.inner.path, text.as_bytes()) {
      Ok(()) => {
        set(self.inner.text, Some(text.to_owned()));
        set(self.inner.error, None);
      }
      Err(error) => {
        let path = self.inner.path.display();
        tracing::warn!(%path, %error, "couldn't write the file");
      }
    }
  }
}

impl ViewInner {
  /// Reads the file again and shows what it holds now.
  fn reread(&self) {
    let (text, error) = match read_text(&self.path) {
      Ok(text) => (Some(text), None),
      Err(error) => {
        // Once, while the same error stays.
        if untrack(|| self.error.try_with(|old| old.as_ref() != Some(&error))) == Some(true) {
          let path = self.path.display();
          tracing::info!(%path, error = error.message, "can't read the file");
        }
        (None, Some(error))
      }
    };
    set(self.text, text);
    set(self.error, error);
  }
}

/// A JSON file kept in step with a signal: `cx.json_file(path, default)`.
pub(crate) fn json_file<T>(cx: &Cx, path: PathBuf, default: T) -> Signal<T>
where
  T: Serialize + DeserializeOwned + PartialEq + 'static,
{
  let value = cx.signal(default);
  let file = Rc::new(JsonInner {
    path,
    value,
    // What the file holds, as far as this shell knows.
    on_disk: RefCell::new(None),
    logged: RefCell::new(None),
    follow: RefCell::new(None),
  });
  file.reread();
  if let Some(files) = cx.service::<Files>() {
    let weak = Rc::downgrade(&file);
    let follow = Follow::new(&files.0, file.path.clone(), move || {
      if let Some(file) = Weak::upgrade(&weak) {
        file.reread();
      }
    });
    file.follow.replace(Some(follow));
  }
  // The value it starts from, so the first run doesn't make a missing
  // file the config never set.
  let start = untrack(|| value.try_with(|value| file.to_json(value))).flatten();
  let writer = Rc::clone(&file);
  let first = Cell::new(true);
  cx.effect(move || {
    let text = value.with(|value| writer.to_json(value));
    let unset = first.replace(false) && start.is_some() && text == start;
    if let (Some(text), false) = (text, unset) {
      untrack(|| writer.write(text));
    }
  })
  .named(format!("cx.json_file {}", file.path.display()));
  value
}

struct JsonInner<T: 'static> {
  path: PathBuf,
  value: Signal<T>,
  on_disk: RefCell<Option<String>>,
  /// The last read problem logged, so it logs once until it reads again.
  logged: RefCell<Option<String>>,
  follow: RefCell<Option<Rc<Follow>>>,
}

impl<T> JsonInner<T>
where
  T: Serialize + DeserializeOwned + PartialEq + 'static,
{
  /// Reads the file again. A missing file keeps the value; one that
  /// isn't the type's JSON is logged and keeps it too. The config's
  /// `Deserialize`, `Serialize` and `PartialEq` may panic, and this may
  /// run from the loop, where nothing else would catch them.
  fn reread(&self) {
    let text = match read_text(&self.path) {
      Ok(text) => text,
      Err(error) => {
        self.log_once(format!("can't read the file: {}", error.message));
        return;
      }
    };
    let applied = panic::catch_unwind(AssertUnwindSafe(|| {
      let read = serde_json::from_str::<T>(&text)?;
      let normal = serde_json::to_string_pretty(&read).unwrap_or_else(|_| text.clone());
      self.on_disk.replace(Some(normal));
      set(self.value, read);
      serde_json::Result::Ok(())
    }));
    match applied {
      Ok(Ok(())) => {
        self.logged.replace(None);
      }
      Ok(Err(error)) => self.log_once(format!("the file isn't the JSON expected: {error}")),
      Err(payload) => {
        let message = panic_message(payload.as_ref());
        self.log_once(format!("reading it panicked: {message}"));
      }
    }
  }

  /// `value` as JSON, or `None` when the config's `Serialize` fails or
  /// panics, which is logged.
  fn to_json(&self, value: &T) -> Option<String> {
    let path = self.path.display();
    match panic::catch_unwind(AssertUnwindSafe(|| serde_json::to_string_pretty(value))) {
      Ok(Ok(text)) => Some(text),
      Ok(Err(error)) => {
        tracing::warn!(%path, %error, "couldn't turn the value into JSON");
        None
      }
      Err(payload) => {
        let message = panic_message(payload.as_ref());
        tracing::error!("cx.json_file {path}: turning the value into JSON panicked: {message}");
        None
      }
    }
  }

  fn write(&self, text: String) {
    if self.on_disk.borrow().as_deref() == Some(text.as_str()) {
      return;
    }
    match write_whole(&self.path, format!("{text}\n").as_bytes()) {
      Ok(()) => {
        self.on_disk.replace(Some(text));
      }
      Err(error) => {
        let path = self.path.display();
        tracing::warn!(%path, %error, "couldn't write the file");
      }
    }
  }

  fn log_once(&self, problem: String) {
    if self.logged.borrow().as_deref() != Some(problem.as_str()) {
      let path = self.path.display();
      tracing::info!(%path, problem, "the JSON file keeps its value");
      self.logged.replace(Some(problem));
    }
  }
}

/// The whole of `path` as UTF-8, up to [`MAX_FILE`].
fn read_text(path: &Path) -> Result<String, FileError> {
  // Checked before the open, so a device or a pipe is never opened.
  if !std::fs::metadata(path)?.is_file() {
    return Err(FileError::new(FileErrorKind::NotAFile, "it isn't a file"));
  }
  let file = open_file(path)?;
  let mut bytes = Vec::new();
  file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > MAX_FILE {
    return Err(FileError::new(FileErrorKind::Other, "it is over 16 MiB"));
  }
  String::from_utf8(bytes).map_err(|_| FileError::new(FileErrorKind::Other, "it isn't UTF-8"))
}

/// `path` opened to read, if it is a regular file. The open doesn't wait,
/// as it would for a writer on a pipe put there since a check.
pub(crate) fn open_file(path: &Path) -> Result<std::fs::File, FileError> {
  use rustix::fs::{Mode, OFlags};

  let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
  let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(std::io::Error::from)?;
  let file = std::fs::File::from(fd);
  if !file.metadata()?.is_file() {
    return Err(FileError::new(FileErrorKind::NotAFile, "it isn't a file"));
  }
  // A regular file's reads never wait anyway; this says so.
  rustix::fs::fcntl_setfl(&file, OFlags::empty()).map_err(std::io::Error::from)?;
  Ok(file)
}

/// Replaces the file at `path` with `bytes` through a new file beside it,
/// renamed over it, making its directory if it is missing. A link is
/// followed, so the file it names is replaced, and a file's permissions
/// are kept.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  use std::os::unix::fs::OpenOptionsExt;

  if let Some(dir) = path.parent()
    && !dir.as_os_str().is_empty()
    && !dir.exists()
  {
    std::fs::create_dir_all(dir)?;
  }
  let target = match std::fs::canonicalize(path) {
    Ok(target) => target,
    Err(error) if error.kind() == ErrorKind::NotFound => path.to_owned(),
    Err(error) => return Err(error),
  };
  let name = target
    .file_name()
    .ok_or_else(|| std::io::Error::other("the path names no file"))?;
  let mut partial_name = std::ffi::OsString::from(".");
  partial_name.push(name);
  partial_name.push(".crabshell-partial");
  let partial = target.with_file_name(partial_name);
  match std::fs::remove_file(&partial) {
    Err(error) if error.kind() != ErrorKind::NotFound => return Err(error),
    _ => {}
  }
  let mode = std::fs::metadata(&target).map_or(0o644, |meta| {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o7777
  });
  // New, so it never writes through a link left in its place.
  let mut file = std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .mode(mode)
    .open(&partial)?;
  let written = file.write_all(bytes).and_then(|()| file.sync_all());
  drop(file);
  if let Err(error) = written.and_then(|()| std::fs::rename(&partial, &target)) {
    let _ = std::fs::remove_file(&partial);
    return Err(error);
  }
  Ok(())
}

/// Sets `signal` unless it holds `value` or is gone. The read is
/// untracked, so an effect that opens a file doesn't run again for it.
pub(crate) fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
