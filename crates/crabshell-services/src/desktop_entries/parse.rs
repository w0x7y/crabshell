//! The Desktop Entry Specification's file format, version 1.5: groups,
//! escapes, lists, localized keys, and the quoting and field codes of
//! `Exec`.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{DesktopAction, DesktopEntry, Sources};

/// The longest line read. A longer one is skipped.
pub(super) const MAX_LINE: usize = 4096;

/// The characters an `Exec` argument must quote, besides the space that
/// separates them.
const RESERVED: &str = "\t\n\"'\\><~|&;$*?#()`";

/// The groups of a desktop file that count, `[Desktop Entry]` and each
/// `[Desktop Action <id>]`, each mapping a key as written, such as
/// `Name[de]`, to its value as written.
pub(super) type Groups = HashMap<String, HashMap<String, String>>;

/// Reads `text`'s groups, and says whether a line was too long and left
/// out.
pub(super) fn groups(text: &str) -> (Groups, bool) {
  let mut groups = Groups::new();
  let mut long = false;
  let mut current = None;
  for line in text.lines() {
    if line.len() > MAX_LINE {
      long = true;
      continue;
    }
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    if let Some(header) = line.trim_end().strip_prefix('[') {
      let name = header.strip_suffix(']');
      let counts = |name: &&str| *name == "Desktop Entry" || name.starts_with("Desktop Action ");
      current = name.filter(counts).map(str::to_owned);
      continue;
    }
    if let (Some(group), Some((key, value))) = (&current, line.split_once('=')) {
      let group = groups.entry(group.clone()).or_default();
      group.insert(key.trim_end().to_owned(), value.trim_start().to_owned());
    }
  }
  (groups, long)
}

/// A string value with its escapes decoded: `\s`, `\n`, `\t`, `\r` and
/// `\\`. Any other backslash stays as written.
pub(super) fn unescape(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  let mut chars = value.chars();
  while let Some(c) = chars.next() {
    if c != '\\' {
      out.push(c);
      continue;
    }
    match chars.next() {
      Some('s') => out.push(' '),
      Some('n') => out.push('\n'),
      Some('t') => out.push('\t'),
      Some('r') => out.push('\r'),
      Some('\\') => out.push('\\'),
      Some(other) => out.extend(['\\', other]),
      None => out.push('\\'),
    }
  }
  out
}

/// A list value split at each `;` that isn't written `\;`, each item
/// unescaped. Empty items, as after the last `;`, are left out.
pub(super) fn list(value: &str) -> Vec<String> {
  let (mut items, mut item) = (Vec::new(), String::new());
  let mut chars = value.chars();
  while let Some(c) = chars.next() {
    match c {
      ';' => items.push(std::mem::take(&mut item)),
      '\\' => match chars.next() {
        Some(';') => item.push(';'),
        // Kept for `unescape`, as `\\;` is a backslash, then the end.
        Some(next) => item.extend(['\\', next]),
        None => item.push('\\'),
      },
      c => item.push(c),
    }
  }
  items.push(item);
  let items = items.iter().map(|item| unescape(item));
  items.filter(|item| !item.is_empty()).collect()
}

/// The names a localized key is looked up by, best first, for a locale
/// such as `de_DE.UTF-8@euro`, whose encoding doesn't count: `de_DE@euro`,
/// `de_DE`, `de@euro`, then `de`. None for `C`, `POSIX` or no locale.
pub(super) fn locales(locale: &str) -> Vec<String> {
  let (rest, modifier) = match locale.split_once('@') {
    Some((rest, modifier)) => (rest, Some(modifier)),
    None => (locale, None),
  };
  let rest = rest.split_once('.').map_or(rest, |(rest, _)| rest);
  let (lang, country) = match rest.split_once('_') {
    Some((lang, country)) => (lang, Some(country)),
    None => (rest, None),
  };
  if matches!(lang, "" | "C" | "POSIX") {
    return Vec::new();
  }
  let mut names = Vec::new();
  if let (Some(country), Some(modifier)) = (country, modifier) {
    names.push(format!("{lang}_{country}@{modifier}"));
  }
  if let Some(country) = country {
    names.push(format!("{lang}_{country}"));
  }
  if let Some(modifier) = modifier {
    names.push(format!("{lang}@{modifier}"));
  }
  names.push(lang.to_owned());
  names
}

/// `key` in the best of `locales` the group has, else unlocalized.
fn localized<'a>(
  group: &'a HashMap<String, String>,
  key: &str,
  locales: &[String],
) -> Option<&'a str> {
  let local = locales
    .iter()
    .find_map(|locale| group.get(&format!("{key}[{locale}]")));
  local.or_else(|| group.get(key)).map(String::as_str)
}

fn text(group: &HashMap<String, String>, key: &str) -> String {
  group
    .get(key)
    .map_or_else(String::new, |value| unescape(value))
}

fn flag(group: &HashMap<String, String>, key: &str) -> bool {
  group.get(key).is_some_and(|value| value == "true")
}

fn items(group: &HashMap<String, String>, key: &str) -> Vec<String> {
  group.get(key).map_or_else(Vec::new, |value| list(value))
}

/// The application a desktop file describes, if it is one to list: of
/// `Type=Application`, with a `Name` and an `Exec` or `DBusActivatable`,
/// not `Hidden`, and with its `TryExec` installed.
pub(super) fn entry(
  id: String,
  path: PathBuf,
  groups: &Groups,
  sources: &Sources,
  locales: &[String],
) -> Option<DesktopEntry> {
  let main = groups.get("Desktop Entry")?;
  let local = |key| localized(main, key, locales).map_or_else(String::new, unescape);
  let name = local("Name");
  let exec = text(main, "Exec");
  let startable = !exec.is_empty() || flag(main, "DBusActivatable");
  let application = main.get("Type").is_some_and(|kind| kind == "Application");
  if flag(main, "Hidden") || !application || name.is_empty() || !startable {
    return None;
  }
  let try_exec = text(main, "TryExec");
  if !try_exec.is_empty() && find(&try_exec, &sources.path).is_none() {
    return None;
  }
  let here = |key| {
    items(main, key)
      .iter()
      .any(|desktop| sources.desktops.contains(desktop))
  };
  let only_elsewhere = main.contains_key("OnlyShowIn") && !here("OnlyShowIn");
  let actions = items(main, "Actions").into_iter().filter_map(|id| {
    let group = groups.get(&format!("Desktop Action {id}"))?;
    let name = localized(group, "Name", locales).map(unescape)?;
    if name.is_empty() {
      return None;
    }
    let icon = localized(group, "Icon", locales).map_or_else(String::new, unescape);
    let exec = text(group, "Exec");
    Some(DesktopAction {
      id,
      name,
      icon,
      exec,
    })
  });
  Some(DesktopEntry {
    id,
    name,
    generic_name: local("GenericName"),
    comment: local("Comment"),
    icon: local("Icon"),
    exec,
    terminal: flag(main, "Terminal"),
    categories: items(main, "Categories"),
    keywords: localized(main, "Keywords", locales).map_or_else(Vec::new, list),
    no_display: flag(main, "NoDisplay") || only_elsewhere || here("NotShowIn"),
    startup_wm_class: text(main, "StartupWMClass"),
    working_dir: Some(text(main, "Path"))
      .filter(|dir| !dir.is_empty())
      .map(PathBuf::from),
    actions: actions.collect(),
    path,
  })
}

/// Where `program` is, as an executable file: at its path when that is
/// absolute, and otherwise in the first of `path`'s directories with it.
pub(super) fn find(program: &str, path: &[PathBuf]) -> Option<PathBuf> {
  let executable = |file: &Path| {
    file
      .metadata()
      .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
  };
  if Path::new(program).is_absolute() {
    return executable(Path::new(program)).then(|| program.into());
  }
  path
    .iter()
    .map(|dir| dir.join(program))
    .find(|file| executable(file))
}

/// The arguments of an `Exec` value, already unescaped, with its quotes
/// undone, or what makes it malformed. An argument is quoted whole, in
/// double quotes where `"`, `` ` ``, `$` and `\` are escaped with a
/// backslash, or it holds none of the reserved characters.
pub(super) fn split_exec(exec: &str) -> Result<Vec<String>, String> {
  let mut args = Vec::new();
  let mut chars = exec.chars().peekable();
  loop {
    while chars.next_if_eq(&' ').is_some() {}
    let Some(first) = chars.next() else {
      return Ok(args);
    };
    let mut arg = String::new();
    if first == '"' {
      loop {
        match chars.next() {
          None => return Err("a quote isn't closed".into()),
          Some('"') => break,
          Some('\\') => match chars.next() {
            Some(c @ ('"' | '`' | '$' | '\\')) => arg.push(c),
            _ => return Err("a backslash in quotes escapes only \", `, $ and \\".into()),
          },
          Some(c @ ('`' | '$')) => return Err(format!("a {c} in quotes isn't escaped")),
          Some(c) => arg.push(c),
        }
      }
      if chars.peek().is_some_and(|c| *c != ' ') {
        return Err("a quote ends inside an argument".into());
      }
    } else {
      let mut c = first;
      loop {
        if RESERVED.contains(c) {
          return Err(format!("{c:?} isn't quoted"));
        }
        arg.push(c);
        match chars.next_if(|c| *c != ' ') {
          Some(next) => c = next,
          None => break,
        }
      }
    }
    args.push(arg);
  }
}

/// What `exec`, `entry`'s or one of its actions', runs when launched with
/// no files or URLs: the program, then its arguments, or what makes it
/// malformed. `%f`, `%F`, `%u`, `%U` and the deprecated codes go, `%i`
/// is `--icon` and the icon, `%c` the name, `%k` the file and `%%` a `%`.
pub(super) fn command_line(exec: &str, entry: &DesktopEntry) -> Result<Vec<String>, String> {
  let mut argv = Vec::new();
  for arg in split_exec(exec)? {
    if arg == "%i" {
      if !entry.icon.is_empty() {
        argv.extend(["--icon".to_owned(), entry.icon.clone()]);
      }
      continue;
    }
    let mut expanded = String::new();
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
      if c != '%' {
        expanded.push(c);
        continue;
      }
      match chars.next() {
        Some('%') => expanded.push('%'),
        Some('c') => expanded.push_str(&entry.name),
        Some('k') => expanded.push_str(&entry.path.to_string_lossy()),
        // Files and URLs, of which there are none, an `%i` inside an
        // argument, and the deprecated codes.
        Some('f' | 'F' | 'u' | 'U' | 'i' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {}
        Some(code) => return Err(format!("%{code} isn't a field code")),
        None => return Err("a % ends it".into()),
      }
    }
    // An argument of only such codes goes, while a quoted `""` stays.
    if !expanded.is_empty() || arg.is_empty() {
      argv.push(expanded);
    }
  }
  if argv.is_empty() {
    return Err("it names no program".into());
  }
  Ok(argv)
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use super::{command_line, entry, groups, list, locales, localized, split_exec, unescape};
  use crate::desktop_entries::{DesktopEntry, Sources};

  #[test]
  fn escapes_are_decoded_and_others_kept() {
    assert_eq!(unescape(r"a\sb\nc\td\re\\f"), "a b\nc\td\re\\f");
    assert_eq!(unescape(r"\q\"), r"\q\");
  }

  #[test]
  fn lists_split_at_unescaped_semicolons() {
    assert_eq!(list("a;b;"), ["a", "b"]);
    assert_eq!(list(r"a\;b;c"), ["a;b", "c"]);
    // An escaped backslash, then the separator.
    assert_eq!(list(r"a\\;b\s"), ["a\\", "b "]);
    assert_eq!(list(";;"), Vec::<String>::new());
  }

  #[test]
  fn locales_match_in_the_specs_order() {
    assert_eq!(
      locales("de_DE.UTF-8@euro"),
      ["de_DE@euro", "de_DE", "de@euro", "de"]
    );
    assert_eq!(locales("sr@latin"), ["sr@latin", "sr"]);
    assert_eq!(locales("pt_BR.UTF-8"), ["pt_BR", "pt"]);
    assert_eq!(locales("C.UTF-8"), Vec::<String>::new());
    assert_eq!(locales("POSIX"), Vec::<String>::new());
    assert_eq!(locales(""), Vec::<String>::new());

    let (groups, _) = groups(
      "[Desktop Entry]\nName=Files\nName[de]=Dateien\nName[de_AT]=Dateien AT\n\
       Name[de@euro]=Dateien Euro\n",
    );
    let main = &groups["Desktop Entry"];
    let name = |locale| localized(main, "Name", &locales(locale));
    assert_eq!(name("de_AT.UTF-8@euro"), Some("Dateien AT"));
    assert_eq!(name("de_CH@euro"), Some("Dateien Euro"));
    assert_eq!(name("de_CH"), Some("Dateien"));
    assert_eq!(name("fr_FR"), Some("Files"));
  }

  #[test]
  fn only_the_entry_and_action_groups_count() {
    let (groups, long) = groups(
      "# a comment\nName=Before any group\n[Desktop Entry]\nName = Spaced\n\
       \n  # indented comment\n[Desktop Action new]\nExec=new\n[Other]\nName=Other\n",
    );
    assert!(!long);
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups["Desktop Entry"]["Name"], "Spaced");
    assert_eq!(groups["Desktop Action new"]["Exec"], "new");
  }

  #[test]
  fn a_long_line_is_left_out_and_said() {
    let text = format!("[Desktop Entry]\nComment={}\nName=Kept\n", "x".repeat(5000));
    let (groups, long) = groups(&text);
    assert!(long);
    assert_eq!(groups["Desktop Entry"].get("Comment"), None);
    assert_eq!(groups["Desktop Entry"]["Name"], "Kept");
  }

  #[test]
  fn exec_undoes_quotes() {
    let exec = r#"  "/opt/My App/run"  --flag "a\"b" "c\$d" "e\`f" "g\\h" """#;
    assert_eq!(
      split_exec(exec).expect("valid"),
      [
        "/opt/My App/run",
        "--flag",
        "a\"b",
        "c$d",
        "e`f",
        "g\\h",
        ""
      ]
    );
    // The string escapes come first, so a file writes a quoted
    // backslash as four.
    let line = unescape(r#"prog "a\\\\b" \s"#);
    assert_eq!(split_exec(&line).expect("valid"), ["prog", "a\\b"]);
  }

  #[test]
  fn a_malformed_exec_says_why() {
    for (exec, why) in [
      (r#"prog "open"#, "a quote isn't closed"),
      ("prog a|b", "'|' isn't quoted"),
      ("prog ~/file", "'~' isn't quoted"),
      ("sh -c 'echo hi'", "'\\'' isn't quoted"),
      ("prog a\tb", "'\\t' isn't quoted"),
      (r"prog a\b", "'\\\\' isn't quoted"),
      (
        r#"prog "\n""#,
        "a backslash in quotes escapes only \", `, $ and \\",
      ),
      (r#"prog "$HOME""#, "a $ in quotes isn't escaped"),
      (r#"prog "a"b"#, "a quote ends inside an argument"),
      (r#"prog a"b""#, "'\"' isn't quoted"),
    ] {
      assert_eq!(split_exec(exec), Err(why.to_owned()), "{exec}");
    }
  }

  /// The entry `body` makes under `[Desktop Entry]`, for `sources`.
  fn read(body: &str, sources: &Sources) -> Option<DesktopEntry> {
    let text = format!("[Desktop Entry]\n{body}");
    let path = PathBuf::from("/apps/app.desktop");
    let locales = locales(&sources.locale);
    entry("app".into(), path, &groups(&text).0, sources, &locales)
  }

  const APP: &str = "Type=Application\nName=My App\nExec=app\n";

  fn app(icon: &str) -> DesktopEntry {
    read(&format!("{APP}Icon={icon}\n"), &Sources::default()).expect("an app")
  }

  #[test]
  fn field_codes_expand_for_a_launch_with_no_files() {
    let app = app("app-icon");
    let run = |exec| command_line(exec, &app);
    assert_eq!(
      run("prog %f %F %u %U --file=%f %d %D %n %N %v %m").expect("valid"),
      ["prog", "--file="]
    );
    assert_eq!(
      run("prog %i %c %k 100%%").expect("valid"),
      [
        "prog",
        "--icon",
        "app-icon",
        "My App",
        "/apps/app.desktop",
        "100%"
      ]
    );
    let no_icon = self::app("");
    assert_eq!(command_line("prog %i", &no_icon).expect("valid"), ["prog"]);
    // A quoted name keeps its spaces; a quoted empty argument stays.
    assert_eq!(run(r#""%c" """#).expect("valid"), ["My App", ""]);
    assert_eq!(run("prog %x"), Err("%x isn't a field code".to_owned()));
    assert_eq!(run("prog 100%"), Err("a % ends it".to_owned()));
    assert_eq!(run("%f %U"), Err("it names no program".to_owned()));
    assert_eq!(run(""), Err("it names no program".to_owned()));
  }

  #[test]
  fn only_named_applications_that_start_count() {
    let none = Sources::default();
    assert!(read(APP, &none).is_some());
    assert!(read("Type=Link\nName=App\nExec=app\n", &none).is_none());
    assert!(read("Name=App\nExec=app\n", &none).is_none());
    assert!(read("Type=Application\nExec=app\n", &none).is_none());
    assert!(read("Type=Application\nName=App\n", &none).is_none());
    assert!(read(&format!("{APP}Hidden=true\n"), &none).is_none());
    let bus = read("Type=Application\nName=App\nDBusActivatable=true\n", &none);
    assert_eq!(bus.expect("listed").exec, "");
  }

  #[test]
  fn no_display_follows_the_desktop() {
    let sway = Sources {
      desktops: vec!["sway".into(), "wlroots".into()],
      ..Sources::default()
    };
    let hidden = |extra: &str| {
      let app = read(&format!("{APP}{extra}\n"), &sway);
      app.expect("listed").no_display
    };
    assert!(!hidden(""));
    assert!(hidden("NoDisplay=true"));
    assert!(hidden("OnlyShowIn=GNOME;KDE;"));
    assert!(!hidden("OnlyShowIn=GNOME;wlroots;"));
    assert!(hidden("NotShowIn=sway;"));
    assert!(!hidden("NotShowIn=KDE;"));
    let nowhere = read(&format!("{APP}OnlyShowIn=GNOME;\n"), &Sources::default());
    assert!(nowhere.expect("listed").no_display);
  }

  #[test]
  fn try_exec_must_be_an_installed_program() {
    use std::os::unix::fs::PermissionsExt;

    let bin = crate::testing::TempDir::new();
    let program = bin.path().join("prog");
    std::fs::write(&program, "#!/bin/sh\n").expect("a program");
    let mode = std::fs::Permissions::from_mode(0o755);
    std::fs::set_permissions(&program, mode).expect("its mode");
    let plain = bin.path().join("plain");
    std::fs::write(&plain, "").expect("a file");
    let sources = Sources {
      path: vec![bin.path().join("missing"), bin.path().to_owned()],
      ..Sources::default()
    };
    let listed = |try_exec: &str| {
      let app = read(&format!("{APP}TryExec={try_exec}\n"), &sources);
      app.is_some()
    };
    assert!(listed("prog"));
    assert!(listed(&program.display().to_string()));
    assert!(!listed("plain"));
    assert!(!listed(&plain.display().to_string()));
    assert!(!listed("missing-program"));
    assert!(!listed(&bin.path().display().to_string()));
  }

  #[test]
  fn an_entry_reads_its_keys() {
    let sources = Sources {
      locale: "de_DE.UTF-8".into(),
      ..Sources::default()
    };
    let app = read(
      "Type=Application\nName=Files\nName[de]=Dateien\nGenericName=File Manager\n\
       Comment=Browse\\sfiles\nIcon=org.gnome.Nautilus\nExec=nautilus --new-window %U\n\
       Terminal=true\nCategories=GNOME;Utility;\nKeywords=folder;\nKeywords[de]=Ordner;Datei;\n\
       StartupWMClass=Nautilus\nPath=/tmp\nActions=new;missing;unnamed;window;\n\
       [Desktop Action window]\nName=Window\nName[de]=Fenster\nExec=nautilus --window\n\
       Icon=window\n[Desktop Action unnamed]\nName=\nExec=nautilus\n[Desktop Action new]\n\
       Name=New\nExec=nautilus --new\n[Desktop Action unlisted]\nName=Unlisted\nExec=x\n",
      &sources,
    )
    .expect("listed");
    assert_eq!(app.name, "Dateien");
    assert_eq!(app.generic_name, "File Manager");
    assert_eq!(app.comment, "Browse files");
    assert_eq!(app.icon, "org.gnome.Nautilus");
    assert_eq!(app.exec, "nautilus --new-window %U");
    assert!(app.terminal);
    assert_eq!(app.categories, ["GNOME", "Utility"]);
    assert_eq!(app.keywords, ["Ordner", "Datei"]);
    assert_eq!(app.startup_wm_class, "Nautilus");
    assert_eq!(app.working_dir, Some(PathBuf::from("/tmp")));
    let actions: Vec<_> = (app.actions.iter())
      .map(|action| (&*action.id, &*action.name, &*action.icon, &*action.exec))
      .collect();
    assert_eq!(
      actions,
      [
        ("new", "New", "", "nautilus --new"),
        ("window", "Fenster", "window", "nautilus --window"),
      ]
    );
  }
}
