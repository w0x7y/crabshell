//! Files a config reads and writes, as Quickshell's `FileView` and
//! `JsonAdapter`.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::io::{ErrorKind, Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use serde::Serialize;

use crate::Shared;
use crate::lines::panic_message;
use crate::watch::{self, Watch};
use serde::de::DeserializeOwned;

/// The largest file read; a larger one reads as missing, and that is
/// logged.
pub(crate) const MAX_FILE: u64 = 16 * 1024 * 1024;

/// What `cx.file_view` and `cx.json_file` need from the shell: the loop,
/// to watch files.
pub(crate) struct Files(pub(crate) Rc<Shared>);

/// Calls `reread` when the file at `path` may have changed: written,
/// replaced, made or removed. `None` if its directory can't be watched,
/// as while it doesn't exist, which is logged; a write that makes it
/// watches again.
// shortcut: watches the directory the path names, not a link's target's,
// so an edit through another name of a linked file isn't seen; follow
// the link when a config needs that.
fn watch_file(shared: &Rc<Shared>, path: &Path, reread: impl Fn() + 'static) -> Option<Watch> {
  let (dir, name) = (path.parent()?, path.file_name()?.to_owned());
  let dir = if dir.as_os_str().is_empty() {
    Path::new(".")
  } else {
    dir
  };
  let on_change = move |change: &watch::Change| {
    // `None`: the directory itself changed, or changes were lost.
    if change.name.as_ref().is_none_or(|changed| *changed == name) {
      reread();
    }
  };
  watch::watch(shared, dir, on_change)
    .inspect_err(|error| {
      let path = path.display();
      tracing::info!(%path, %error, "can't watch the file, so changes to it aren't seen");
    })
    .ok()
}

/// Keeps a file's watch for as long as the opening context's owner.
struct Keep<T>(#[allow(dead_code, reason = "held for its drop")] Rc<T>);

/// A text file, from `cx.file_view(path)`. Its text is read at once and
/// again when it changes on disk. A clone is cheap and shares it.
#[derive(Clone)]
pub struct FileView {
  inner: Rc<ViewInner>,
}

struct ViewInner {
  path: PathBuf,
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  watch: RefCell<Option<Watch>>,
  /// Lives under the opening context's owner.
  text: Signal<Option<String>>,
  /// The last error logged, so a file that stays missing logs once.
  logged: RefCell<Option<ErrorKind>>,
}

/// `cx.file_view(path)`.
pub(crate) fn file_view(cx: &Cx, path: PathBuf) -> FileView {
  let inner = Rc::new(ViewInner {
    text: cx.signal(None),
    shared: cx.service::<Files>().map(|files| Rc::clone(&files.0)),
    watch: RefCell::new(None),
    path,
    logged: RefCell::new(None),
  });
  inner.reread();
  inner.watch();
  // So `text()` follows the file even once every `FileView` is dropped.
  cx.owner().signal(Keep(Rc::clone(&inner)));
  FileView { inner }
}

impl FileView {
  /// The file's text, or `None` while it is missing, can't be read, isn't
  /// UTF-8, or is over 16 MiB, each logged once until it reads again.
  pub fn text(&self) -> ReadSignal<Option<String>> {
    self.inner.text.read_only()
  }

  /// Replaces the file's text, through a new file beside it renamed over
  /// it, so a reader never sees half of it. A link is followed and the
  /// file it names is replaced, keeping its permissions. A failure is
  /// logged and the text stays as it was.
  pub fn write(&self, text: impl AsRef<str>) {
    let text = text.as_ref();
    match write_whole(&self.inner.path, text.as_bytes()) {
      Ok(()) => {
        set(self.inner.text, Some(text.to_owned()));
        // Its directory may be new.
        self.inner.watch();
      }
      Err(error) => {
        let path = self.inner.path.display();
        tracing::warn!(%path, %error, "couldn't write the file");
      }
    }
  }
}

impl ViewInner {
  /// Follows the file's changes, unless it does already or can't.
  fn watch(self: &Rc<Self>) {
    let (Some(shared), true) = (&self.shared, self.watch.borrow().is_none()) else {
      return;
    };
    let weak = Rc::downgrade(self);
    let watch = watch_file(shared, &self.path, move || {
      if let Some(inner) = Weak::upgrade(&weak) {
        inner.reread();
      }
    });
    self.watch.replace(watch);
  }

  /// Reads the file again and shows what it holds now.
  fn reread(&self) {
    let text = match read_text(&self.path) {
      Ok(text) => {
        self.logged.replace(None);
        Some(text)
      }
      Err(error) => {
        if self.logged.replace(Some(error.kind())) != Some(error.kind()) {
          let path = self.path.display();
          tracing::info!(%path, %error, "can't read the file");
        }
        None
      }
    };
    set(self.text, text);
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
    shared: cx.service::<Files>().map(|files| Rc::clone(&files.0)),
    watch: RefCell::new(None),
  });
  file.reread();
  file.watch();
  let writer = Rc::clone(&file);
  let first = Cell::new(true);
  // Writes what the config sets, not the value it starts from, so a
  // missing file isn't made until then. A value just read from the file
  // is already there, so it isn't written back. A `Serialize` that
  // panics is the effect's to catch and log.
  cx.effect(move || {
    let text = value.with(|value| serde_json::to_string_pretty(value));
    if !first.replace(false) {
      untrack(|| writer.write(text));
    }
  });
  value
}

struct JsonInner<T: 'static> {
  path: PathBuf,
  value: Signal<T>,
  on_disk: RefCell<Option<String>>,
  /// The last read problem logged, so it logs once until it reads again.
  logged: RefCell<Option<String>>,
  /// `None` when the shell provided no services.
  shared: Option<Rc<Shared>>,
  /// Held by the effect that writes, which lives with the context.
  watch: RefCell<Option<Watch>>,
}

impl<T> JsonInner<T>
where
  T: Serialize + DeserializeOwned + PartialEq + 'static,
{
  /// Follows the file's changes, unless it does already or can't.
  fn watch(self: &Rc<Self>) {
    let (Some(shared), true) = (&self.shared, self.watch.borrow().is_none()) else {
      return;
    };
    let weak = Rc::downgrade(self);
    let watch = watch_file(shared, &self.path, move || {
      if let Some(file) = Weak::upgrade(&weak) {
        file.reread();
      }
    });
    self.watch.replace(watch);
  }

  /// Reads the file again. A missing file keeps the value; one that
  /// isn't the type's JSON is logged and keeps it too.
  fn reread(&self) {
    let text = match read_text(&self.path) {
      Ok(text) => text,
      Err(error) => {
        self.log_once(format!("can't read the file: {error}"));
        return;
      }
    };
    // A `Deserialize` of the config's may panic; this may run from the
    // loop, where nothing else would catch it.
    let parsed = panic::catch_unwind(AssertUnwindSafe(|| serde_json::from_str::<T>(&text)));
    let parsed = match parsed {
      Ok(parsed) => parsed,
      Err(payload) => {
        let message = panic_message(payload.as_ref());
        return self.log_once(format!("reading it panicked: {message}"));
      }
    };
    match parsed {
      Ok(read) => {
        self.logged.replace(None);
        self
          .on_disk
          .replace(Some(normalized::<T>(&read).unwrap_or(text)));
        set(self.value, read);
      }
      Err(error) => self.log_once(format!("the file isn't the JSON expected: {error}")),
    }
  }

  fn write(self: &Rc<Self>, text: serde_json::Result<String>) {
    let text = match text {
      Ok(text) => text,
      Err(error) => {
        let path = self.path.display();
        return tracing::warn!(%path, %error, "couldn't turn the value into JSON");
      }
    };
    if self.on_disk.borrow().as_deref() == Some(text.as_str()) {
      return;
    }
    match write_whole(&self.path, format!("{text}\n").as_bytes()) {
      Ok(()) => {
        self.on_disk.replace(Some(text));
        // Its directory may be new.
        self.watch();
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

/// `value` as this module writes it, to tell whether the file already
/// holds it.
fn normalized<T: Serialize>(value: &T) -> Option<String> {
  serde_json::to_string_pretty(value).ok()
}

/// The whole of `path` as UTF-8, up to [`MAX_FILE`].
fn read_text(path: &Path) -> std::io::Result<String> {
  let file = std::fs::File::open(path)?;
  let mut bytes = Vec::new();
  file.take(MAX_FILE + 1).read_to_end(&mut bytes)?;
  if bytes.len() as u64 > MAX_FILE {
    return Err(std::io::Error::other("it is over 16 MiB"));
  }
  String::from_utf8(bytes)
    .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "it isn't UTF-8"))
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
fn set<T: PartialEq + 'static>(signal: Signal<T>, value: T) {
  if untrack(|| signal.try_with(|old| *old != value)) == Some(true) {
    signal.try_set(value);
  }
}
