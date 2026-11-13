//! The command line and the template, without running anything.

use std::path::{Path, PathBuf};

use super::{FROM_CRATES_IO, TEMPLATE_MANIFEST, Task, parse, toml_string};

fn args(line: &str) -> Vec<String> {
  line.split_whitespace().map(String::from).collect()
}

#[test]
fn commands_parse_and_anything_else_is_refused() {
  assert_eq!(parse(&args("init")), Ok(Task::Init { path: None }));
  assert_eq!(
    parse(&args("init --path /src/crabshell")),
    Ok(Task::Init {
      path: Some(PathBuf::from("/src/crabshell"))
    })
  );
  assert_eq!(parse(&args("check")), Ok(Task::Check));
  assert_eq!(parse(&args("--help")), Ok(Task::Help));
  assert_eq!(
    parse(&args("help init")),
    Err(String::from("can't make sense of `help init`"))
  );
  assert_eq!(parse(&args("")), Err(String::from("no command given")));
  assert_eq!(
    parse(&args("init --force")),
    Err(String::from("can't make sense of `init --force`"))
  );
  assert_eq!(
    parse(&args("build")),
    Err(String::from("unknown command `build`"))
  );
}

#[test]
fn a_path_becomes_a_toml_string() {
  let quoted = |path: &str| toml_string(Path::new(path)).expect("UTF-8");
  assert_eq!(quoted("/home/a b/x"), r#""/home/a b/x""#);
  assert_eq!(quoted(r#"/odd"\name"#), r#""/odd\"\\name""#);
  assert_eq!(quoted("/new\nline\ttab"), "\"/new\\u000Aline\ttab\"");
  use std::os::unix::ffi::OsStrExt;
  let not_utf8 = Path::new(std::ffi::OsStr::from_bytes(b"/src/\xff"));
  assert_eq!(toml_string(not_utf8), None);
}

#[test]
fn the_template_has_the_line_path_replaces() {
  assert_eq!(TEMPLATE_MANIFEST.matches(FROM_CRATES_IO).count(), 1);
}
