//! `cx.persist`: values that survive a reload. The shell keeps one map of
//! them, by key, under the root owner. On `save` it writes them to a file,
//! which the next shell reads on startup.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crabshell_reactive::{Owner, Signal};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// A saved value of one key, as JSON.
type Saver = Rc<dyn Fn() -> Option<Value>>;

/// One key's signal, as `Any` since each key has its own type, and how
/// to save it.
type Kept = (Box<dyn Any>, Saver);

/// The persisted values, and the saved ones not asked for yet.
pub(crate) struct Persisted {
  root: Owner,
  values: RefCell<HashMap<String, Kept>>,
  saved: RefCell<Map<String, Value>>,
}

impl Persisted {
  /// Values under `root`, starting from `saved`.
  pub(crate) fn new(root: Owner, saved: Map<String, Value>) -> Persisted {
    Persisted {
      root,
      values: RefCell::default(),
      saved: RefCell::new(saved),
    }
  }

  /// The signal for `key`: the one made before, or a new one under the
  /// root owner, from the saved value if it still fits `T`, else
  /// `default`. If `key` was asked for with another type, that is logged
  /// and `default` comes back.
  pub(crate) fn signal<T>(&self, key: &str, default: T) -> Result<Signal<T>, T>
  where
    T: Serialize + DeserializeOwned + 'static,
  {
    let kept = self
      .values
      .borrow()
      .get(key)
      .map(|(signal, _)| signal.downcast_ref::<Signal<T>>().copied());
    match kept {
      Some(Some(signal)) => return Ok(signal),
      Some(None) => {
        tracing::error!(
          key,
          "cx.persist({key:?}) was asked for before with another type; this one isn't kept"
        );
        return Err(default);
      }
      None => {}
    }
    let saved = self.saved.borrow_mut().remove(key);
    // A config's own Deserialize impl is user code.
    let read = saved.map(|value| {
      panic::catch_unwind(AssertUnwindSafe(|| serde_json::from_value::<T>(value)))
        .map_err(|_| String::from("its Deserialize panicked"))
        .and_then(|read| read.map_err(|error| error.to_string()))
    });
    let initial = match read {
      Some(Ok(value)) => value,
      Some(Err(problem)) => {
        tracing::warn!(
          key,
          "the saved value no longer fits ({problem}); using the default"
        );
        default
      }
      None => default,
    };
    let signal = self.root.signal(initial);
    let saver: Saver = Rc::new(move || {
      signal
        .try_with(|value| serde_json::to_value(value).ok())
        .flatten()
    });
    self
      .values
      .borrow_mut()
      .insert(key.to_owned(), (Box::new(signal), saver));
    Ok(signal)
  }

  /// Every value, as JSON. One whose Serialize fails or panics is logged
  /// and left out.
  pub(crate) fn save(&self) -> Map<String, Value> {
    let savers: Vec<(String, Saver)> = self
      .values
      .borrow()
      .iter()
      .map(|(key, (_, saver))| (key.clone(), Rc::clone(saver)))
      .collect();
    let mut saved = Map::new();
    for (key, saver) in savers {
      match panic::catch_unwind(AssertUnwindSafe(|| saver())) {
        Ok(Some(value)) => {
          saved.insert(key, value);
        }
        Ok(None) => tracing::warn!(key, "a persisted value couldn't be saved"),
        Err(_) => tracing::error!(
          key,
          "a persisted value's Serialize panicked; it isn't saved"
        ),
      }
    }
    saved
  }
}

/// Where values are saved between shells: `crabshell/state.json` in
/// `XDG_RUNTIME_DIR`. `None`, logged, without one.
pub(crate) fn state_file() -> Option<PathBuf> {
  match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
    Some(dir) => Some(Path::new(&dir).join("crabshell").join("state.json")),
    None => {
      tracing::warn!("no XDG_RUNTIME_DIR, so cx.persist values don't survive a reload");
      None
    }
  }
}

/// The values the last shell saved in `path`, which is then removed, so a
/// shell started later, as after a crash, starts fresh. A file that is
/// missing gives none; one that doesn't parse is logged.
pub(crate) fn load(path: &Path) -> Map<String, Value> {
  let text = match std::fs::read_to_string(path) {
    Ok(text) => text,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Map::new(),
    Err(error) => {
      tracing::warn!(path = %path.display(), %error, "can't read the saved values");
      return Map::new();
    }
  };
  let _ = std::fs::remove_file(path);
  match serde_json::from_str(&text) {
    Ok(values) => values,
    Err(error) => {
      tracing::warn!(path = %path.display(), %error, "the saved values don't parse");
      Map::new()
    }
  }
}

/// Writes `values` to `path`, through a new file beside it that replaces
/// it whole, in a directory only this user can read. A directory that is
/// a symlink is refused, and one others can read is made private first.
pub(crate) fn write(path: &Path, values: &Map<String, Value>) -> std::io::Result<()> {
  use std::io::Write;
  use std::os::unix::fs::OpenOptionsExt;

  if let Some(dir) = path.parent() {
    private_dir(dir)?;
  }
  let partial = path.with_extension("json.partial");
  match std::fs::remove_file(&partial) {
    Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
    _ => {}
  }
  // New, so it never writes through a link left in its place.
  let mut file = std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .mode(0o600)
    .open(&partial)?;
  file.write_all(&serde_json::to_vec(values)?)?;
  drop(file);
  std::fs::rename(&partial, path)
}

/// Creates `dir` for this user only. One that is a symlink is refused,
/// and one others can read is made private.
pub(crate) fn private_dir(dir: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

  std::fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(dir)?;
  let meta = std::fs::symlink_metadata(dir)?;
  if !meta.is_dir() {
    return Err(std::io::Error::other(format!(
      "{} isn't a directory",
      dir.display()
    )));
  }
  if meta.permissions().mode() & 0o077 != 0 {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
  }
  Ok(())
}
