//! Which commit shows a new workspace list, so the log times the switch.

use std::time::{Duration, Instant};

use crate::switch_log::SwitchLog;

#[test]
fn a_switch_drawn_in_the_dispatch_that_brought_it_is_logged_once() {
    let mut log = SwitchLog::default();
    let event = Instant::now();
    assert_eq!(log.after_dispatch(Some(event), 5, 6, || false), Some(event));
    assert_eq!(log.after_dispatch(Some(event), 6, 7, || false), None);
}

#[test]
fn with_several_windows_the_last_ones_commit_shows_it() {
    let mut log = SwitchLog::default();
    let event = Instant::now();
    // One window drew at once; another waits for its frame callback.
    assert_eq!(log.after_dispatch(Some(event), 5, 6, || true), None);
    assert_eq!(log.after_dispatch(Some(event), 6, 6, || true), None);
    assert_eq!(log.after_dispatch(Some(event), 6, 7, || false), Some(event));
}

#[test]
fn a_switch_that_draws_nothing_is_not_logged() {
    let mut log = SwitchLog::default();
    let event = Instant::now();
    assert_eq!(log.after_dispatch(Some(event), 5, 5, || false), None);
    // A later commit, such as the clock's, isn't taken for it.
    assert_eq!(log.after_dispatch(Some(event), 5, 6, || false), None);
}

#[test]
fn a_list_no_event_led_to_is_not_logged_or_waited_for() {
    let mut log = SwitchLog::default();
    let drawing = || panic!("nothing waits, so nothing asks");
    assert_eq!(log.after_dispatch(None, 0, 1, drawing), None);
}

#[test]
fn a_newer_switch_replaces_one_still_drawing() {
    let mut log = SwitchLog::default();
    let first = Instant::now();
    let second = first + Duration::from_millis(5);
    assert_eq!(log.after_dispatch(Some(first), 5, 5, || true), None);
    assert_eq!(
        log.after_dispatch(Some(second), 5, 6, || false),
        Some(second)
    );
}
