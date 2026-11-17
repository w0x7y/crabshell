use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::{DesktopEntries, Service, Sources, object_path};
use crate::testing::{FakeService, Harness, PrivateBus, TempDir, capture_logs, offline, reply};
use crate::{Config, ServicesExt};

/// A user's and a system's `applications` directory, and a `bin` for
/// `PATH`, in a directory of the test's own.
struct Dirs(TempDir);

impl Dirs {
  fn new() -> Dirs {
    let temp = TempDir::new();
    for dir in ["user", "system", "bin"] {
      std::fs::create_dir(temp.path().join(dir)).expect("a directory");
    }
    Dirs(temp)
  }

  fn user(&self) -> PathBuf {
    self.0.path().join("user")
  }

  fn system(&self) -> PathBuf {
    self.0.path().join("system")
  }

  fn bin(&self) -> PathBuf {
    self.0.path().join("bin")
  }

  fn sources(&self) -> Sources {
    Sources {
      dirs: vec![self.user(), self.system()],
      path: vec![self.bin()],
      ..Sources::default()
    }
  }

  /// A program in `bin` that writes its pid, its directory and its
  /// arguments, a line each, to `<name>.out` beside it.
  fn recorder(&self, name: &str) -> (PathBuf, PathBuf) {
    let program = self.bin().join(name);
    std::fs::write(
      &program,
      "#!/bin/sh\n{ echo $$; pwd; printf '%s\\n' \"$@\"; } > \"$0.tmp\" && mv \"$0.tmp\" \"$0.out\"\n",
    )
    .expect("a program");
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&program, mode).expect("its mode");
    let out = self.bin().join(format!("{name}.out"));
    (program, out)
  }
}

/// Writes `text` at `relative` under `dir`, with the directories it needs.
fn write(dir: &Path, relative: &str, text: &str) {
  let path = dir.join(relative);
  let parent = path.parent().expect("a file in a directory");
  std::fs::create_dir_all(parent).expect("its directory");
  std::fs::write(&path, text).expect("a desktop file");
}

/// An application named `name` that runs `exec`, and `extra` keys.
fn app(name: &str, exec: &str, extra: &str) -> String {
  format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n{extra}\n")
}

fn shell(sources: Sources) -> (Harness, DesktopEntries) {
  let shell = Harness::new(Config {
    desktop_entries: sources,
    ..offline()
  });
  let entries = shell.cx.desktop_entries();
  (shell, entries)
}

fn run(shell: &mut Harness, ms: u64) -> usize {
  shell.run_until(Instant::now() + Duration::from_millis(ms))
}

fn ids(entries: &DesktopEntries) -> Vec<String> {
  let apps = entries.applications().get_untracked();
  apps.into_iter().map(|app| app.id).collect()
}

#[test]
fn nothing_is_read_before_the_loop_turns_after_the_first_ask() {
  let dirs = Dirs::new();
  write(&dirs.user(), "a.desktop", &app("A", "a", ""));
  let mut shell = Harness::new(Config {
    desktop_entries: dirs.sources(),
    ..offline()
  });
  let service = shell.cx.service::<Service>().expect("provided");
  run(&mut shell, 50);
  assert!(service.state.borrow().is_none());

  let entries = shell.cx.desktop_entries();
  assert_eq!(ids(&entries), Vec::<String>::new());
  assert!(!service.shared.watcher.is_open());
  run(&mut shell, 50);
  assert_eq!(ids(&entries), ["a"]);
}

#[test]
fn ids_come_from_the_path_and_the_list_is_sorted_by_name() {
  let dirs = Dirs::new();
  write(
    &dirs.user(),
    "kde4/konsole.desktop",
    &app("Konsole", "konsole", ""),
  );
  write(&dirs.user(), "a/b/c.desktop", &app("beta", "c", ""));
  write(&dirs.user(), "notes.txt", &app("Notes", "notes", ""));
  write(&dirs.system(), "x.desktop", &app("Alpha", "x", ""));
  write(&dirs.system(), "w.desktop", &app("alpha", "w", ""));
  // A linked directory is followed, and a link loop read once.
  let elsewhere = dirs.0.path().join("elsewhere");
  write(&elsewhere, "e.desktop", &app("Linked", "e", ""));
  std::os::unix::fs::symlink(&elsewhere, dirs.user().join("linked")).expect("a link");
  std::os::unix::fs::symlink(".", dirs.user().join("loop")).expect("a link");
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  assert_eq!(
    ids(&entries),
    ["w", "x", "a-b-c", "kde4-konsole", "linked-e"]
  );
}

#[test]
fn the_first_directory_wins_and_hidden_removes_the_id() {
  let dirs = Dirs::new();
  write(&dirs.user(), "both.desktop", &app("User's", "u", ""));
  write(&dirs.system(), "both.desktop", &app("System's", "s", ""));
  write(
    &dirs.user(),
    "gone.desktop",
    "[Desktop Entry]\nHidden=true\n",
  );
  write(&dirs.system(), "gone.desktop", &app("Gone", "g", ""));
  write(&dirs.system(), "system.desktop", &app("System", "s", ""));
  // Listed twice, it is read once.
  let mut sources = dirs.sources();
  sources.dirs.push(dirs.system());
  let (mut shell, entries) = shell(sources);
  run(&mut shell, 50);
  let apps = entries.applications().get_untracked();
  let names: Vec<_> = apps.iter().map(|app| (&*app.id, &*app.name)).collect();
  assert_eq!(names, [("system", "System"), ("both", "User's")]);
}

#[test]
fn lookup_finds_a_windows_application() {
  let dirs = Dirs::new();
  for (id, extra) in [
    ("foot", ""),
    ("Code", ""),
    ("code", ""),
    ("alpha", ""),
    ("beta", "StartupWMClass=Alpha"),
    ("org.x.Gamma", ""),
    ("delta", "StartupWMClass=gamma"),
    ("org.gnome.Nautilus", ""),
    ("wm", "StartupWMClass=WM-Class-X"),
  ] {
    write(&dirs.user(), &format!("{id}.desktop"), &app(id, id, extra));
  }
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  let found = |app_id| entries.lookup(app_id).map(|app| app.id);
  for (app_id, id) in [
    ("foot", "foot"),
    ("FOOT", "foot"),
    ("Code", "Code"),
    ("code", "code"),
    ("ALPHA", "alpha"),
    ("gamma", "delta"),
    ("nautilus", "org.gnome.Nautilus"),
    ("wm-class-x", "wm"),
  ] {
    assert_eq!(found(app_id).as_deref(), Some(id), "{app_id}");
  }
  assert_eq!(found("missing"), None);
  assert_eq!(found(""), None);
}

#[test]
fn a_change_shows_once_the_files_are_still() {
  let dirs = Dirs::new();
  write(&dirs.user(), "a.desktop", &app("A", "a", ""));
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  let seen: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
  let (sink, looked) = (Rc::clone(&seen), entries.clone());
  shell.cx.effect(move || {
    let found = looked.lookup("c").map(|app| app.name);
    sink.borrow_mut().push(found);
  });
  crabshell_reactive::Runtime::current().flush();

  write(&dirs.user(), "b.desktop", &app("B", "b", ""));
  run(&mut shell, 60);
  write(&dirs.system(), "c.desktop", &app("C", "c", ""));
  // B's 100 ms have passed, but not C's.
  run(&mut shell, 70);
  assert_eq!(ids(&entries), ["a"]);
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["a", "b", "c"]);
  assert_eq!(*seen.borrow(), [None, Some("C".to_owned())]);

  // One wakeup for the change, one for the read, one for the end of
  // the run.
  std::fs::remove_file(dirs.user().join("a.desktop")).expect("removed");
  assert!(run(&mut shell, 300) <= 3);
  assert_eq!(ids(&entries), ["b", "c"]);
}

#[test]
fn new_directories_are_watched() {
  let dirs = Dirs::new();
  let later = dirs.0.path().join("later");
  std::fs::create_dir(&later).expect("a directory");
  let sources = Sources {
    dirs: vec![dirs.user(), later.join("applications")],
    ..Sources::default()
  };
  let (mut shell, entries) = shell(sources);
  run(&mut shell, 50);

  std::fs::create_dir(dirs.user().join("sub")).expect("a directory");
  run(&mut shell, 300);
  write(&dirs.user(), "sub/x.desktop", &app("X", "x", ""));
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["sub-x"]);

  // An `applications` directory made after the first read.
  write(&later, "applications/y.desktop", &app("Y", "y", ""));
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["sub-x", "y"]);
  write(&later, "applications/z.desktop", &app("Z", "z", ""));
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["sub-x", "y", "z"]);
}

#[test]
fn an_idle_list_wakes_nothing() {
  let dirs = Dirs::new();
  write(&dirs.user(), "a.desktop", &app("A", "a", ""));
  write(&dirs.user(), "sub/b.desktop", &app("B", "b", ""));
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  assert_eq!(ids(&entries), ["a", "sub-b"]);
  // The one wakeup is the end of the run.
  assert!(run(&mut shell, 2000) <= 1);
}

#[test]
fn a_file_written_in_place_beside_the_entries_reads_nothing() {
  use std::io::Write;
  let dirs = Dirs::new();
  write(&dirs.user(), "a.desktop", &app("A", "a", ""));
  write(&dirs.user(), "mimeapps.list", "");
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  assert_eq!(ids(&entries), ["a"]);
  let service = shell.cx.service::<Service>().expect("provided");
  let waiting = || {
    service
      .state
      .borrow()
      .as_ref()
      .is_some_and(|state| state.waiting)
  };
  let path = dirs.user().join("mimeapps.list");
  let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
  file.write_all(b"[Default Applications]\n").unwrap();
  run(&mut shell, 20);
  assert!(!waiting(), "a write that isn't done counts for nothing");
}

#[test]
fn caps_leave_out_what_is_too_big_and_log_once() {
  let dirs = Dirs::new();
  let big = format!("{}# {}\n", app("Big", "big", ""), "x".repeat(70 * 1024));
  write(&dirs.user(), "big.desktop", &big);
  let long = app("Long", "long", &format!("Comment={}", "x".repeat(5000)));
  write(&dirs.user(), "long.desktop", &long);
  for n in 0..super::MAX_ENTRIES {
    write(
      &dirs.user(),
      &format!("many/{n:05}.desktop"),
      &app("Many", "many", ""),
    );
  }
  let logs = capture_logs(|| {
    let (mut shell, entries) = shell(dirs.sources());
    run(&mut shell, 50);
    let apps = entries.applications().get_untracked();
    // Big and long count toward the cap, so the last two are left out.
    assert_eq!(apps.len(), super::MAX_ENTRIES - 1);
    let long = apps
      .iter()
      .find(|app| app.id == "long")
      .expect("long, without its comment");
    assert_eq!(long.comment, "");
    assert!(
      !apps
        .iter()
        .any(|app| app.id == "big" || app.id == "many-many-09999")
    );
    // Read again, logging nothing more.
    write(&dirs.user(), "again.desktop", &app("Again", "again", ""));
    run(&mut shell, 300);
  });
  for line in ["over 64 KiB", "over 4 KiB", "over 10000 desktop entries"] {
    assert_eq!(logs.matches(line).count(), 1, "{line}: {logs}");
  }
}

#[test]
fn launch_runs_the_program_apart_and_reaps_it() {
  let dirs = Dirs::new();
  let (program, out) = dirs.recorder("record");
  let program = program.display();
  let workdir = dirs.0.path().display();
  let extra = format!(
    "Path={workdir}\nActions=second;\n[Desktop Action second]\nName=Second\n\
     Exec=\"{program}\" second %u"
  );
  let exec = format!("\"{program}\" %U --name %c");
  write(
    &dirs.user(),
    "record.desktop",
    &app("Record", &exec, &extra),
  );
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);

  let ran = |shell: &mut Harness| {
    shell.retry_until("the program's output", || out.exists(), |_| {});
    let lines = std::fs::read_to_string(&out).expect("its output");
    std::fs::remove_file(&out).expect("removed");
    let lines: Vec<String> = lines.lines().map(str::to_owned).collect();
    let proc = PathBuf::from(format!("/proc/{}", lines[0]));
    // A zombie would keep its /proc entry.
    shell.retry_until("the reap", || !proc.exists(), |_| {});
    lines[1..].to_vec()
  };
  entries.launch("record");
  assert_eq!(ran(&mut shell), [&*workdir.to_string(), "--name", "Record"]);
  entries.launch_action("record", "second");
  assert_eq!(ran(&mut shell), [&*workdir.to_string(), "second"]);
}

#[test]
fn a_terminal_application_runs_in_xdg_terminal_exec() {
  let dirs = Dirs::new();
  let (_, out) = dirs.recorder("xdg-terminal-exec");
  write(
    &dirs.user(),
    "top.desktop",
    &app("Top", "htop -d 10", "Terminal=true"),
  );
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  entries.launch("top");
  shell.retry_until("the terminal's output", || out.exists(), |_| {});
  let lines = std::fs::read_to_string(&out).expect("its output");
  assert_eq!(
    lines.lines().skip(2).collect::<Vec<_>>(),
    ["htop", "-d", "10"]
  );
}

#[test]
fn what_cant_start_is_logged_and_runs_nothing() {
  let dirs = Dirs::new();
  let (program, out) = dirs.recorder("record");
  let program = program.display();
  write(
    &dirs.user(),
    "bad.desktop",
    &app("Bad", &format!("\"{program}\" \"open"), ""),
  );
  let bus = "[Desktop Entry]\nType=Application\nName=Bus\nDBusActivatable=true\n";
  write(&dirs.user(), "bus.desktop", bus);
  let terminal = app("Term", &format!("\"{program}\""), "Terminal=true");
  write(&dirs.user(), "term.desktop", &terminal);
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  let logs = capture_logs(|| {
    entries.launch("bad");
    entries.launch("bus");
    entries.launch("term");
    entries.launch("missing");
    entries.launch_action("bus", "missing");
    run(&mut shell, 300);
  });
  assert!(!out.exists());
  for line in [
    "a malformed Exec",
    "no Exec to fall back on",
    "xdg-terminal-exec isn't on PATH",
    "no application has this ID",
    "no such action",
  ] {
    assert!(logs.contains(line), "{line}: {logs}");
  }
}

#[test]
fn an_exec_the_spec_refuses_runs_as_a_shell_splits_it_logged_once() {
  let dirs = Dirs::new();
  let (program, out) = dirs.recorder("record");
  let exec = format!("{} --opt=\"a b\" 'c d' e\\ f", program.display());
  write(&dirs.user(), "loose.desktop", &app("Loose", &exec, ""));
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  let logs = capture_logs(|| {
    for _ in 0..2 {
      entries.launch("loose");
      shell.retry_until("the program's output", || out.exists(), |_| {});
      let lines = std::fs::read_to_string(&out).expect("its output");
      std::fs::remove_file(&out).expect("removed");
      let args: Vec<&str> = lines.lines().skip(2).collect();
      assert_eq!(args, ["--opt=a b", "c d", "e f"]);
    }
  });
  assert_eq!(logs.matches("split as a shell would").count(), 1, "{logs}");
}

#[test]
fn a_d_bus_application_is_activated_and_falls_back_to_its_exec() {
  let dirs = Dirs::new();
  let (program, out) = dirs.recorder("record");
  let extra = "DBusActivatable=true\nActions=new-window;\n\
               [Desktop Action new-window]\nName=New Window\nExec=record --new";
  let notes = app("Notes", &format!("\"{}\" plain", program.display()), extra);
  write(&dirs.user(), "org.example.Notes.desktop", &notes);
  let bus = PrivateBus::start();
  let mut shell = Harness::new(Config {
    desktop_entries: dirs.sources(),
    session_bus: Some(bus.address.parse().expect("dbus-daemon's address")),
    ..offline()
  });
  let entries = shell.cx.desktop_entries();
  run(&mut shell, 50);
  let found = entries.lookup("org.example.Notes").expect("the entry");
  assert!(found.dbus_activatable);

  let fake = FakeService::start(&mut shell, &bus, "org.example.Notes");
  let actions: Rc<RefCell<Vec<String>>> = Rc::default();
  fake.on("Activate", |call| reply(call, &()));
  let named = Rc::clone(&actions);
  fake.on("ActivateAction", move |call| {
    type Body = (
      String,
      Vec<zbus::OwnedVariant>,
      std::collections::HashMap<String, zbus::OwnedVariant>,
    );
    let (action, parameters, _): Body = call.body().deserialize().expect("(sava{sv})");
    assert!(parameters.is_empty());
    named.borrow_mut().push(action);
    reply(call, &())
  });
  fake.own(&mut shell);
  let called = |member: &str| {
    let (calls, call) = (
      Rc::clone(&fake.calls),
      format!("{member} /org/example/Notes"),
    );
    move || calls.borrow().contains(&call)
  };
  entries.launch("org.example.Notes");
  shell.retry_until("Activate", called("Activate"), |_| {});
  entries.launch_action("org.example.Notes", "new-window");
  shell.retry_until("ActivateAction", called("ActivateAction"), |_| {});
  assert_eq!(*actions.borrow(), ["new-window"]);
  run(&mut shell, 100);
  assert!(!out.exists(), "Exec didn't run");

  // Nobody owns the name now, and the bus starts nothing.
  fake.leave(&mut shell);
  let logs = capture_logs(|| {
    entries.launch("org.example.Notes");
    shell.retry_until("Exec instead", || out.exists(), |_| {});
  });
  assert!(
    logs.contains("couldn't start the application over D-Bus"),
    "{logs}"
  );
  let lines = std::fs::read_to_string(&out).expect("its output");
  assert_eq!(lines.lines().nth(2), Some("plain"));
}

#[test]
fn an_id_that_isnt_a_d_bus_name_runs_its_exec() {
  assert_eq!(
    object_path("org.gnome.Nautilus")
      .as_ref()
      .map(|path| path.as_str()),
    Some("/org/gnome/Nautilus")
  );
  assert_eq!(
    object_path("org.example.my-app")
      .as_ref()
      .map(|path| path.as_str()),
    Some("/org/example/my_app")
  );
  assert_eq!(object_path("firefox"), None, "one element");
  assert_eq!(object_path("org..x"), None);
  assert_eq!(object_path("1org.example"), None);

  let dirs = Dirs::new();
  let (program, out) = dirs.recorder("record");
  let exec = format!("\"{}\"", program.display());
  write(
    &dirs.user(),
    "firefox.desktop",
    &app("Firefox", &exec, "DBusActivatable=true"),
  );
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  let logs = capture_logs(|| entries.launch("firefox"));
  shell.retry_until("Exec instead", || out.exists(), |_| {});
  assert!(logs.contains("isn't a D-Bus name"), "{logs}");
}

#[test]
fn a_linked_directory_added_or_removed_shows() {
  let dirs = Dirs::new();
  let elsewhere = dirs.0.path().join("elsewhere");
  write(&elsewhere, "linked.desktop", &app("Linked", "linked", ""));
  let (mut shell, entries) = shell(dirs.sources());
  run(&mut shell, 50);
  std::os::unix::fs::symlink(&elsewhere, dirs.user().join("foo")).expect("a link");
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["foo-linked"]);
  std::fs::remove_file(dirs.user().join("foo")).expect("the link removed");
  run(&mut shell, 300);
  assert!(ids(&entries).is_empty());
}

#[test]
fn a_link_on_the_way_to_a_root_swapped_for_another_reads_again() {
  // As NixOS links a user's profile: a link to a link to the store, with
  // the middle one swapped for a new generation.
  let dirs = Dirs::new();
  let base = dirs.0.path();
  write(
    &base.join("store/one"),
    "applications/x.desktop",
    &app("X", "x", ""),
  );
  write(
    &base.join("store/two"),
    "applications/y.desktop",
    &app("Y", "y", ""),
  );
  std::fs::create_dir(base.join("profiles")).expect("a directory");
  let profile = base.join("profiles/profile");
  std::os::unix::fs::symlink(base.join("store/one"), &profile).expect("a link");
  std::os::unix::fs::symlink("profiles/profile", base.join("home-profile")).expect("a link");
  // A link to itself ends the walk rather than the shell.
  std::os::unix::fs::symlink("loop", base.join("loop")).expect("a link");
  let sources = Sources {
    dirs: vec![
      base.join("loop/applications"),
      base.join("home-profile/applications"),
    ],
    ..Sources::default()
  };
  let (mut shell, entries) = shell(sources);
  run(&mut shell, 50);
  assert_eq!(ids(&entries), ["x"]);

  let swap = |target: &str| {
    let new = base.join("profiles/profile.new");
    std::os::unix::fs::symlink(base.join(target), &new).expect("a link");
    std::fs::rename(&new, &profile).expect("swapped");
  };
  swap("store/two");
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["y"]);
  swap("store/one");
  run(&mut shell, 300);
  assert_eq!(ids(&entries), ["x"]);
  // Asleep again once read.
  assert!(run(&mut shell, 500) <= 1);
}

#[test]
fn a_fifo_named_like_an_entry_is_skipped() {
  let dirs = Dirs::new();
  write(&dirs.user(), "a.desktop", &app("A", "a", ""));
  let fifo = dirs.user().join("trap.desktop");
  let made = std::process::Command::new("mkfifo")
    .arg(&fifo)
    .status()
    .expect("mkfifo runs");
  assert!(made.success());
  let (mut shell, entries) = shell(dirs.sources());
  // Opening the FIFO would block here for good.
  run(&mut shell, 50);
  assert_eq!(ids(&entries), ["a"]);
}
