//! Which windows go when an output goes, with fake outputs: numbers stand
//! in for `wl_output` objects, and `()` for surfaces.

use crabshell_reactive::Runtime;
use crabshell_ui::{Cx, Text, Window};

use crate::windows::Windows;

/// A window for output `name` under `cx`.
fn window(cx: &Cx, name: &str) -> Window {
    Window::new(cx, super::panel(name).child(Text::new(name.to_owned())))
}

/// Gives the window at `index` a surface on fake output `output`.
fn show(windows: &mut Windows<(), u32>, index: usize, output: u32) {
    let entry = &mut windows.entries[index];
    entry.surface = Some(());
    entry.output = Some(output);
}

fn outputs_left(windows: &Windows<(), u32>) -> Vec<String> {
    windows
        .entries
        .iter()
        .map(|entry| entry.window.output().name())
        .collect()
}

#[test]
fn a_removed_output_drops_its_windows_and_their_nodes() {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let mut windows: Windows<(), u32> = Windows::new();
    windows.add(window(&cx, "DP-1"));
    show(&mut windows, 0, 1);
    runtime.flush();
    let nodes = runtime.node_count();

    // Two windows on DP-2, output 2: one shown, one not shown yet.
    windows.add(window(&cx, "DP-2"));
    show(&mut windows, 1, 2);
    windows.add(window(&cx, "DP-2"));
    runtime.flush();
    assert!(runtime.node_count() > nodes);

    assert_eq!(windows.drop_output(&2, "DP-2"), 2);
    runtime.flush();
    assert_eq!(outputs_left(&windows), ["DP-1"]);
    assert_eq!(runtime.node_count(), nodes, "their owners were disposed");
}

#[test]
fn windows_are_dropped_by_the_output_they_were_shown_on_not_its_name() {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let mut windows: Windows<(), u32> = Windows::new();
    // DP-2 was unplugged and plugged back in as output 3 before output 2's
    // removal arrived: the new window stays.
    windows.add(window(&cx, "DP-2"));
    show(&mut windows, 0, 2);
    windows.add(window(&cx, "DP-2"));
    show(&mut windows, 1, 3);
    windows.add(window(&cx, "HDMI-A-1"));
    assert_eq!(windows.drop_output(&2, "DP-2"), 1);
    assert_eq!(windows.entries[0].output, Some(3));
    assert_eq!(outputs_left(&windows), ["DP-2", "HDMI-A-1"]);
    // A window waiting for an output of another name stays too.
    assert_eq!(windows.drop_output(&3, "DP-2"), 1);
    assert_eq!(outputs_left(&windows), ["HDMI-A-1"]);
}

#[test]
fn windows_are_added_and_removed_by_id() {
    let runtime = Runtime::current();
    let cx = Cx::new(runtime.new_owner());
    let mut windows: Windows<(), u32> = Windows::new();
    let first = windows.add(window(&cx, "DP-1"));
    let second = windows.add(window(&cx, "DP-1"));
    assert_ne!(first, second);
    assert!(windows.remove(first));
    assert!(!windows.remove(first), "already gone");
    assert!(windows.get(first).is_none());
    assert_eq!(windows.get(second).map(|entry| entry.id), Some(second));
}
