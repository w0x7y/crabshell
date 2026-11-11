//! `CRABSHELL_LOG`: a level, or nothing.

use tracing::level_filters::LevelFilter;

use crate::shell::log_level;

#[test]
fn crabshell_log_names_a_level() {
  assert_eq!(log_level(None), (LevelFilter::INFO, None));
  assert_eq!(log_level(Some("debug")), (LevelFilter::DEBUG, None));
  assert_eq!(log_level(Some(" WARN ")), (LevelFilter::WARN, None));
  assert_eq!(log_level(Some("off")), (LevelFilter::OFF, None));
}

#[test]
fn an_empty_crabshell_log_is_unset_and_a_wrong_one_is_reported() {
  assert_eq!(log_level(Some("")), (LevelFilter::INFO, None));
  assert_eq!(log_level(Some("  ")), (LevelFilter::INFO, None));
  let (level, problem) = log_level(Some("verbose"));
  assert_eq!(level, LevelFilter::INFO);
  assert_eq!(
    problem.as_deref(),
    Some(
      "CRABSHELL_LOG=\"verbose\" isn't a level (error, warn, info, debug, trace or off); logging at info"
    )
  );
}
