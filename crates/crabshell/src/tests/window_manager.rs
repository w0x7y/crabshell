//! `cx.window_manager()` without a shell.

use crabshell_reactive::Runtime;
use crabshell_ui::Cx;

use super::capture_logs;
use crate::ShellExt;

#[test]
fn a_handle_whose_context_went_lists_nothing() {
  let cx = Cx::new(Runtime::current().new_owner());
  let mut manager = None;
  capture_logs(|| manager = Some(cx.window_manager()));
  let manager = manager.expect("a handle");
  assert_eq!(manager.windowsets_on("DP-1"), []);
  cx.owner().dispose();
  assert_eq!(manager.windowsets_on("DP-1"), [], "gone, not a panic");
}
