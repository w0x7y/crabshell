//! The installed applications, from their desktop entries: what a
//! launcher lists, and where a window's app id finds its icon.

mod parse;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::{Rc, Weak};
use std::sync::Once;
use std::time::{Duration, Instant};

use calloop::timer::TimeoutAction;
use crabshell_reactive::{ReadSignal, Signal, untrack};
use crabshell_ui::Cx;
use inotify::EventMask;
use zbus::OwnedObjectPath;
use zbus::names::WellKnownName;

use crate::dbus::{CALL_TIMEOUT, set_changed};
use crate::watch::{self, Change, Watch};
use crate::{Shared, spawn};

/// The interface a D-Bus activatable application is started through.
const APPLICATION: &str = "org.freedesktop.Application";
/// How long the files stay still after a change before the list is read
/// again, so a package manager that writes many reads it once.
const SETTLE: Duration = Duration::from_millis(100);
/// The largest desktop file read.
const MAX_FILE: u64 = 64 * 1024;
/// The most desktop files read. The rest are left out.
const MAX_ENTRIES: usize = 10_000;

/// An installed application, from its desktop entry.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopEntry {
  /// Its desktop file ID: the file's path under `applications/`, with
  /// `/` as `-` and without `.desktop`, such as `org.gnome.Nautilus`.
  pub id: String,
  /// Its name, in the user's language when the file has it.
  pub name: String,
  /// What kind of program it is, such as `Web Browser`, or `""`.
  pub generic_name: String,
  /// A line about it, for a tooltip, or `""`.
  pub comment: String,
  /// Its icon, a name in the icon theme or a file's path as the entry
  /// writes it, or `""`.
  pub icon: String,
  /// What it runs, as its `Exec` key writes it, field codes such as `%U`
  /// and quotes included. `""` for one only D-Bus starts.
  pub exec: String,
  /// Whether it is started over D-Bus, from `DBusActivatable`, with
  /// `exec` only for when that fails.
  pub dbus_activatable: bool,
  /// Whether it runs in a terminal.
  pub terminal: bool,
  /// Its categories, such as `Network` and `WebBrowser`.
  pub categories: Vec<String>,
  /// More words a search may match, in the user's language when the
  /// file has them.
  pub keywords: Vec<String>,
  /// Whether a launcher leaves it out: for `NoDisplay`, or because
  /// `OnlyShowIn` or `NotShowIn` leave out this desktop. A window's icon
  /// may still come from it.
  pub no_display: bool,
  /// The window class its windows have, if the entry says, or `""`.
  pub startup_wm_class: String,
  /// The directory it runs in, if not the shell's.
  pub working_dir: Option<PathBuf>,
  /// What else it can do, such as open a private window, in the
  /// entry's order.
  pub actions: Vec<DesktopAction>,
  /// The desktop file it was read from.
  pub path: PathBuf,
}

/// Something more an application can do, as a launcher's menu offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopAction {
  /// Its name in the desktop file, such as `new-window`.
  pub id: String,
  /// What a menu shows, in the user's language when the file has it.
  pub name: String,
  /// Its icon, as [`DesktopEntry::icon`], or `""`.
  pub icon: String,
  /// What it runs, as [`DesktopEntry::exec`].
  pub exec: String,
}

/// Where the desktop entries are, and what picks among them. Tests point
/// it at their own directories.
#[derive(Default)]
pub(crate) struct Sources {
  /// Each `applications` directory, the user's first.
  pub(crate) dirs: Vec<PathBuf>,
  /// The desktops `XDG_CURRENT_DESKTOP` names, such as `sway`.
  pub(crate) desktops: Vec<String>,
  /// The locale for messages, from `LC_ALL`, `LC_MESSAGES` or `LANG`.
  pub(crate) locale: String,
  /// `PATH`'s directories, where `TryExec` and `xdg-terminal-exec` are
  /// looked for.
  pub(crate) path: Vec<PathBuf>,
}

impl Sources {
  /// The user's: the XDG data directories, `XDG_CURRENT_DESKTOP`, the
  /// locale and `PATH`.
  pub(crate) fn system() -> Sources {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    // The XDG spec ignores relative paths.
    let home = var("XDG_DATA_HOME")
      .map(PathBuf::from)
      .filter(|dir| dir.is_absolute())
      .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    let data_dirs = var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let dirs = home.into_iter().chain(std::env::split_paths(&data_dirs));
    let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
      .into_iter()
      .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    Sources {
      dirs: dirs
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("applications"))
        .collect(),
      desktops: desktops
        .split(':')
        .filter(|desktop| !desktop.is_empty())
        .map(str::to_owned)
        .collect(),
      locale: locale.unwrap_or_default(),
      path: var("PATH").map_or_else(Vec::new, |path| std::env::split_paths(&path).collect()),
    }
  }
}

/// The installed applications, from `cx.desktop_entries()`. Cloning it
/// is cheap.
#[derive(Clone)]
pub struct DesktopEntries {
  service: Option<Rc<Service>>,
  applications: ReadSignal<Vec<DesktopEntry>>,
}

impl DesktopEntries {
  /// Every application, sorted by name ignoring case, then by ID. Empty
  /// until the loop's next turn after the first `cx.desktop_entries()`.
  pub fn applications(&self) -> ReadSignal<Vec<DesktopEntry>> {
    self.applications
  }

  /// The application a window's app id names, as a task bar finds the
  /// window's icon: the one with that ID, else one whose ID differs only
  /// in case, else one whose `StartupWMClass` matches ignoring case,
  /// else one whose ID ends in that part after a dot, as
  /// `org.gnome.Nautilus` does for `nautilus`. It reads the list as
  /// `get` does, so a binding that calls it runs again when the list
  /// changes.
  pub fn lookup(&self, app_id: &str) -> Option<DesktopEntry> {
    let found = self
      .applications
      .try_with(|apps| lookup(apps, app_id).cloned());
    found.flatten()
  }

  /// Starts the application with ID `id`, on its own: in a process
  /// group of its own, with no input or output, and in its `working_dir`
  /// if it has one. A terminal app runs through `xdg-terminal-exec`. One
  /// that is [`dbus_activatable`](DesktopEntry::dbus_activatable) is
  /// called with `org.freedesktop.Application.Activate` on the session
  /// bus, which starts it if it isn't running, and runs its `Exec` if
  /// that fails. What can't start is logged: an unknown ID, a malformed
  /// `Exec`, a terminal app without `xdg-terminal-exec` on `PATH`, and a
  /// D-Bus app that didn't start with no `Exec` to fall back on.
  pub fn launch(&self, id: &str) {
    self.launch_with(id, None);
  }

  /// Starts one of an application's actions, named by its
  /// [`DesktopAction::id`], as [`launch`](Self::launch) does, a D-Bus
  /// app's with `ActivateAction`.
  pub fn launch_action(&self, id: &str, action: &str) {
    self.launch_with(id, Some(action));
  }

  fn launch_with(&self, id: &str, action: Option<&str>) {
    let Some(service) = &self.service else {
      return;
    };
    let find = |apps: &Vec<DesktopEntry>| apps.iter().find(|app| app.id == id).cloned();
    match untrack(|| self.applications.try_with(find)).flatten() {
      Some(app) => launch(&service.shared, &app, action),
      None => tracing::warn!(id, "no application has this ID, so nothing starts"),
    }
  }
}

fn lookup<'a>(apps: &'a [DesktopEntry], app_id: &str) -> Option<&'a DesktopEntry> {
  if app_id.is_empty() {
    return None;
  }
  let last_part = |app: &&DesktopEntry| {
    let part = app.id.rsplit('.').next();
    part.is_some_and(|part| part.eq_ignore_ascii_case(app_id))
  };
  apps
    .iter()
    .find(|app| app.id == app_id)
    .or_else(|| apps.iter().find(|app| app.id.eq_ignore_ascii_case(app_id)))
    .or_else(|| {
      apps
        .iter()
        .find(|app| app.startup_wm_class.eq_ignore_ascii_case(app_id))
    })
    .or_else(|| apps.iter().find(last_part))
}

/// Starts `app`, or its `action`, apart from the shell: over D-Bus if
/// it says so, falling back to its `Exec`.
fn launch(shared: &Rc<Shared>, app: &DesktopEntry, action: Option<&str>) {
  let id = app.id.as_str();
  let exec = match action {
    None => &app.exec,
    Some(action) => match app.actions.iter().find(|found| found.id == action) {
      Some(found) => &found.exec,
      None => {
        return tracing::warn!(
          id,
          action,
          "the application has no such action, so nothing starts"
        );
      }
    },
  };
  if !app.dbus_activatable {
    return run(shared, app, exec);
  }
  let Some(path) = object_path(id) else {
    tracing::warn!(
      id,
      "the application's ID isn't a D-Bus name, so it can't start over D-Bus"
    );
    return run(shared, app, exec);
  };
  let Some(address) = shared.config.session_bus.clone() else {
    tracing::warn!(id, "no session bus to start the application over D-Bus");
    return run(shared, app, exec);
  };
  let (shared, app) = (Rc::clone(shared), app.clone());
  let (exec, action) = (exec.clone(), action.map(str::to_owned));
  let started = Rc::clone(&shared);
  started.spawn(async move {
    let activated = activate(&shared, address, &app.id, path, action.as_deref()).await;
    if let Err(error) = activated {
      tracing::warn!(id = app.id, %error, "couldn't start the application over D-Bus");
      run(&shared, &app, &exec);
    }
  });
}

/// The object path of the application with ID `id`, as the Desktop
/// Entry Specification derives it, or `None` when `id` isn't a D-Bus
/// well-known name.
fn object_path(id: &str) -> Option<OwnedObjectPath> {
  WellKnownName::try_from(id).ok()?;
  let path = format!("/{}", id.replace('.', "/").replace('-', "_"));
  OwnedObjectPath::try_from(path).ok()
}

/// Calls `Activate`, or `ActivateAction` for `action`, on the
/// application `id` at `path` on the session bus, which starts it first
/// if it isn't running.
async fn activate(
  shared: &Shared,
  address: zbus::Address,
  id: &str,
  path: OwnedObjectPath,
  action: Option<&str>,
) -> zbus::Result<()> {
  let connection = zbus::connection::Builder::address(address).build().await?;
  // No startup notification or activation token: the shell has neither.
  let platform_data = HashMap::<&str, zbus::Variant<'_>>::new();
  let (destination, interface) = (Some(id), Some(APPLICATION));
  let reply = match action {
    None => {
      let body = (platform_data,);
      let call = connection.call_method(destination, &path, interface, "Activate", &body);
      shared.timeout(CALL_TIMEOUT, call).await
    }
    Some(action) => {
      let body = (action, Vec::<zbus::Variant<'_>>::new(), platform_data);
      let call = connection.call_method(destination, &path, interface, "ActivateAction", &body);
      shared.timeout(CALL_TIMEOUT, call).await
    }
  };
  reply.map(drop)
}

/// Runs `exec`, `app`'s or one of its actions', apart from the shell.
fn run(shared: &Rc<Shared>, app: &DesktopEntry, exec: &str) {
  let id = app.id.as_str();
  if exec.is_empty() {
    return tracing::warn!(
      id,
      "the application has no Exec to fall back on, so nothing starts"
    );
  }
  let argv = match parse::command_line(exec, app) {
    Ok(argv) => argv,
    Err(why) => return tracing::warn!(id, exec, why, "a malformed Exec, so nothing starts"),
  };
  let mut command = if app.terminal {
    let path = &shared.config.desktop_entries.path;
    let Some(terminal) = parse::find("xdg-terminal-exec", path) else {
      return tracing::warn!(
        id,
        "a terminal application, and xdg-terminal-exec isn't on PATH, so nothing starts"
      );
    };
    let mut command = Command::new(terminal);
    command.args(&argv);
    command
  } else {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
  };
  if let Some(dir) = &app.working_dir {
    command.current_dir(dir);
  }
  // A failure is logged there.
  let _ = spawn::spawn(shared, &mut command);
}

/// `cx.desktop_entries()`.
pub(crate) fn desktop_entries(cx: &Cx) -> DesktopEntries {
  let Some(service) = cx.service::<Service>() else {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
      tracing::warn!("cx.desktop_entries() needs the services the shell provides; it stays empty");
    });
    return DesktopEntries {
      service: None,
      applications: cx.signal(Vec::new()).read_only(),
    };
  };
  service.start()
}

/// The desktop entries of one shell, read from the first ask.
pub(crate) struct Service {
  shared: Rc<Shared>,
  state: RefCell<Option<State>>,
}

struct State {
  applications: Signal<Vec<DesktopEntry>>,
  /// A watch on each directory read, and on the parent of each
  /// `applications` directory that isn't there yet.
  watches: Vec<Watch>,
  /// Whether a read waits on a timer.
  waiting: bool,
  /// The last change heard while a read waits.
  changed_at: Option<Instant>,
  /// The caps logged so far, as each is logged once.
  logged: Logged,
}

#[derive(Clone, Copy, Default)]
struct Logged {
  big: bool,
  long: bool,
  many: bool,
}

impl Service {
  pub(crate) fn new(shared: Rc<Shared>) -> Service {
    Service {
      shared,
      state: RefCell::new(None),
    }
  }

  fn start(self: &Rc<Self>) -> DesktopEntries {
    let started = self.state.borrow().as_ref().map(|state| state.applications);
    let applications = started.unwrap_or_else(|| {
      let applications = self.shared.root.signal(Vec::new());
      *self.state.borrow_mut() = Some(State {
        applications,
        watches: Vec::new(),
        waiting: false,
        changed_at: None,
        logged: Logged::default(),
      });
      self.read_later();
      applications
    });
    DesktopEntries {
      service: Some(Rc::clone(self)),
      applications: applications.read_only(),
    }
  }

  /// Something changed in a watched directory.
  fn changed(self: &Rc<Self>) {
    let waiting = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.changed_at = Some(Instant::now());
      state.waiting
    };
    if !waiting {
      self.read_later();
    }
  }

  /// Reads the list on the loop's next turn, or [`SETTLE`] after the
  /// last change, on one timer that moves while changes come.
  fn read_later(self: &Rc<Self>) {
    let Some(event_loop) = self.shared.event_loop() else {
      return;
    };
    let settled = |state: &State| state.changed_at.map(|at| at + SETTLE);
    let at = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.waiting = true;
      settled(state).unwrap_or_else(Instant::now)
    };
    let service = Rc::clone(self);
    let tick = Box::new(move || {
      let at = service.state.borrow().as_ref().and_then(settled);
      match at {
        Some(at) if at > Instant::now() => TimeoutAction::ToInstant(at),
        _ => {
          service.read();
          TimeoutAction::Drop
        }
      }
    });
    if event_loop.insert_timer(at, tick).is_none()
      && let Some(state) = self.state.borrow_mut().as_mut()
    {
      state.waiting = false;
    }
  }

  /// Reads every directory and shows what it holds.
  fn read(self: &Rc<Self>) {
    let (applications, logged) = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.waiting = false;
      state.changed_at = None;
      (state.applications, state.logged)
    };
    // Gone with the shell.
    if applications.try_with(|_| ()).is_none() {
      return;
    }
    let sources = &self.shared.config.desktop_entries;
    let mut scan = Scan {
      service: Rc::downgrade(self),
      shared: &self.shared,
      sources,
      locales: parse::locales(&sources.locale),
      watches: Vec::new(),
      visited: HashSet::new(),
      seen: HashSet::new(),
      entries: Vec::new(),
      logged,
    };
    for root in &sources.dirs {
      scan.root(root);
    }
    let Scan {
      mut entries,
      watches,
      logged,
      ..
    } = scan;
    entries.sort_by_cached_key(|app| (app.name.to_lowercase(), app.id.clone()));
    let old = {
      let mut state = self.state.borrow_mut();
      let Some(state) = state.as_mut() else {
        return;
      };
      state.logged = logged;
      std::mem::replace(&mut state.watches, watches)
    };
    // After the new ones, so a directory still there keeps its kernel
    // watch.
    drop(old);
    set_changed(applications, entries);
  }
}

/// One read of every directory.
struct Scan<'a> {
  service: Weak<Service>,
  shared: &'a Rc<Shared>,
  sources: &'a Sources,
  locales: Vec<String>,
  watches: Vec<Watch>,
  /// Each directory read, by device and inode, so one listed twice or
  /// reached again through a link is read once.
  visited: HashSet<(u64, u64)>,
  /// Each ID found so far, since the first file with an ID decides it.
  seen: HashSet<String>,
  entries: Vec<DesktopEntry>,
  logged: Logged,
}

impl Scan<'_> {
  fn root(&mut self, root: &Path) {
    if root.is_dir() {
      return self.dir(root, root);
    }
    // Watched for the directory to appear.
    if let (Some(parent), Some(name)) = (
      root.parent().filter(|parent| parent.is_dir()),
      root.file_name(),
    ) {
      let name = name.to_owned();
      self.watch(parent, move |change| {
        change.name.as_ref().is_none_or(|changed| *changed == name)
      });
    }
  }

  /// Reads the list again on each change in `dir` that `counts`.
  fn watch(&mut self, dir: &Path, counts: impl Fn(&Change) -> bool + 'static) {
    let service = self.service.clone();
    let on_change = move |change: &Change| {
      if let Some(service) = service.upgrade().filter(|_| counts(change)) {
        service.changed();
      }
    };
    match watch::watch(self.shared, dir, on_change) {
      Ok(watch) => self.watches.push(watch),
      Err(error) => {
        tracing::warn!(%error, dir = %dir.display(), "can't watch a directory of applications")
      }
    }
  }

  fn dir(&mut self, root: &Path, dir: &Path) {
    let Ok(meta) = dir.metadata() else {
      return;
    };
    if !self.visited.insert((meta.dev(), meta.ino())) {
      return;
    }
    // Before the read, so a change during it is heard.
    self.watch(dir, |change| {
      let name = change.name.as_ref().map(Path::new);
      let entry = name.is_some_and(|name| name.extension() == Some(OsStr::new("desktop")));
      // A link made or removed carries no ISDIR, and may lead to a
      // directory: anything but a written file or a cache counts.
      let linked = !change.mask.contains(EventMask::CLOSE_WRITE)
        && name.is_some_and(|name| name.extension() != Some(OsStr::new("cache")));
      name.is_none() || entry || linked || change.mask.contains(EventMask::ISDIR)
    });
    let Ok(read) = std::fs::read_dir(dir) else {
      return;
    };
    let mut paths: Vec<PathBuf> = read.flatten().map(|entry| entry.path()).collect();
    // So the same file wins each time when two make one ID.
    paths.sort();
    for path in paths {
      // Followed through links, as packages often link their entries in.
      match path.metadata() {
        Ok(meta) if meta.is_dir() => self.dir(root, &path),
        // A regular file only: opening a FIFO would wait for a writer.
        Ok(meta) if meta.is_file() && path.extension() == Some(OsStr::new("desktop")) => {
          self.file(root, &path);
        }
        _ => {}
      }
    }
  }

  fn file(&mut self, root: &Path, path: &Path) {
    // A path that isn't UTF-8 has no ID.
    let Some(id) = id(root, path) else {
      return;
    };
    if self.seen.contains(&id) {
      return;
    }
    let shown = path.display();
    if self.seen.len() == MAX_ENTRIES {
      if !std::mem::replace(&mut self.logged.many, true) {
        tracing::warn!(path = %shown, "over {MAX_ENTRIES} desktop entries; the rest are left out");
      }
      return;
    }
    self.seen.insert(id.clone());
    let mut bytes = Vec::new();
    let read = File::open(path).and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut bytes));
    if let Err(error) = read {
      return tracing::info!(%error, path = %shown, "can't read a desktop entry");
    }
    if bytes.len() as u64 > MAX_FILE {
      if !std::mem::replace(&mut self.logged.big, true) {
        tracing::warn!(path = %shown, "a desktop entry over 64 KiB is left out");
      }
      return;
    }
    let Ok(text) = String::from_utf8(bytes) else {
      return tracing::info!(path = %shown, "a desktop entry that isn't UTF-8 is left out");
    };
    let (groups, long) = parse::groups(&text);
    if long && !std::mem::replace(&mut self.logged.long, true) {
      tracing::warn!(path = %shown, "a line over 4 KiB in a desktop entry is left out");
    }
    let entry = parse::entry(id, path.to_owned(), &groups, self.sources, &self.locales);
    self.entries.extend(entry);
  }
}

/// The desktop file ID of `path` under `root`: `kde4/konsole.desktop`
/// is `kde4-konsole`.
fn id(root: &Path, path: &Path) -> Option<String> {
  let relative = path.strip_prefix(root).ok()?.to_str()?;
  Some(relative.strip_suffix(".desktop")?.replace('/', "-"))
}
