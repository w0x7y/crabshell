//! Building the config with cargo, and finding the shell it made.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The cargo to run: `CARGO` when set, as under `cargo run`, else the one
/// on `PATH`.
pub(crate) fn cargo() -> OsString {
  std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// The program a build made.
#[derive(Debug, PartialEq)]
pub(crate) struct Built {
  pub(crate) program: PathBuf,
  /// Nothing changed since the last build, so the program is the same.
  pub(crate) fresh: bool,
}

/// What cargo's JSON messages say about a build.
#[derive(Debug, PartialEq)]
pub(crate) struct Report {
  /// The compiler's warnings and errors, as it would print them.
  pub(crate) rendered: String,
  /// The first error's one-line message.
  pub(crate) first_error: Option<String>,
  /// The one program built.
  pub(crate) built: Result<Built, String>,
}

/// Builds the config in `config` into `target`. Cargo's progress goes to
/// stderr as it runs, the compiler's messages once it ends. An error is
/// one line, short enough for a notification.
pub(crate) fn build(config: &Path, target: &Path) -> Result<Built, String> {
  // From the config's directory, so another project's cargo settings
  // don't apply.
  let output = Command::new(cargo())
    .current_dir(config)
    .args(["build", "--message-format=json", "--manifest-path"])
    .arg(config.join("Cargo.toml"))
    .env("CARGO_TARGET_DIR", target)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit())
    .output()
    .map_err(|error| format!("can't run cargo ({error}); is Rust installed?"))?;
  let report = read(&String::from_utf8_lossy(&output.stdout));
  // Not `eprint!`, which panics when stderr is closed.
  let _ = std::io::Write::write_all(&mut std::io::stderr(), report.rendered.as_bytes());
  if !output.status.success() {
    return Err(
      report
        .first_error
        .unwrap_or_else(|| String::from("cargo failed; its message is above")),
    );
  }
  report.built
}

/// Reads cargo's JSON `messages`.
pub(crate) fn read(messages: &str) -> Report {
  let mut rendered = String::new();
  let mut first_error = None;
  let mut built = Vec::new();
  for line in messages.lines() {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    match message["reason"].as_str() {
      Some("compiler-message") => {
        let diagnostic = &message["message"];
        rendered.push_str(diagnostic["rendered"].as_str().unwrap_or_default());
        if diagnostic["level"] == "error" && first_error.is_none() {
          first_error = diagnostic["message"].as_str().map(String::from);
        }
      }
      // Libraries and build scripts have no `executable`.
      Some("compiler-artifact") => {
        if let Some(path) = message["executable"].as_str() {
          built.push(Built {
            program: PathBuf::from(path),
            fresh: message["fresh"] == true,
          });
        }
      }
      _ => {}
    }
  }
  let built = match built.len() {
    1 => Ok(built.remove(0)),
    0 => Err(String::from("the config built no program")),
    _ => Err(String::from(
      "the config builds more than one program; keep one, in src/main.rs",
    )),
  };
  Report {
    rendered,
    first_error,
    built,
  }
}
