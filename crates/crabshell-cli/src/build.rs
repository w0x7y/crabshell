//! Building the config with cargo, and finding the shell it made.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The cargo to run: `CARGO` when set, as under `cargo run`, else the one
/// on `PATH`.
pub(crate) fn cargo() -> OsString {
  std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// Builds the config in `config` into `target`, its errors shown as cargo
/// prints them, and returns the program it built.
pub(crate) fn build(config: &Path, target: &Path) -> Result<PathBuf, String> {
  // From the config's directory, so another project's cargo settings
  // don't apply.
  let output = Command::new(cargo())
    .current_dir(config)
    .args([
      "build",
      "--message-format=json-render-diagnostics",
      "--manifest-path",
    ])
    .arg(config.join("Cargo.toml"))
    .env("CARGO_TARGET_DIR", target)
    .stdin(Stdio::null())
    .stderr(Stdio::inherit())
    .output()
    .map_err(|error| format!("can't run cargo ({error}); is Rust installed?"))?;
  if !output.status.success() {
    return Err(String::from("the config didn't build"));
  }
  executable(&String::from_utf8_lossy(&output.stdout))
}

/// The one program cargo's JSON `messages` say it built.
pub(crate) fn executable(messages: &str) -> Result<PathBuf, String> {
  let mut built = Vec::new();
  for line in messages.lines() {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
      continue;
    };
    // Libraries and build scripts have no `executable`.
    if message["reason"] == "compiler-artifact"
      && let Some(path) = message["executable"].as_str()
    {
      built.push(PathBuf::from(path));
    }
  }
  match built.as_slice() {
    [one] => Ok(one.clone()),
    [] => Err(String::from("the config built no program")),
    _ => Err(String::from(
      "the config builds more than one program; keep one, in src/main.rs",
    )),
  }
}
