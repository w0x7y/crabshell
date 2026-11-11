//! `per_output`'s bookkeeping with a stand-in for the platform that
//! numbers the windows it is given: what is built for which output, and
//! what is dropped when an output goes or a window is closed.

use std::cell::{Cell, RefCell};
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crabshell_reactive::{Runtime, Signal};
use crabshell_ui::{Cx, Output, PanelWindow, Text, Window};

use super::Register;
use crate::per_output::{self, Build, Host, IntoPanels, PerOutput};

/// Holds windows by number, as the platform holds them by id. Removing
/// one drops it, which disposes its owner.
#[derive(Default)]
struct Fake {
    windows: Vec<(u32, Window)>,
    next: u32,
}

impl Host for Fake {
    type Id = u32;

    fn add_window(&mut self, window: Window) -> u32 {
        self.next += 1;
        self.windows.push((self.next, window));
        self.next
    }

    fn remove_window(&mut self, id: u32) {
        self.windows.retain(|(held, _)| *held != id);
    }
}

impl Fake {
    fn outputs(&self) -> Vec<String> {
        self.windows
            .iter()
            .map(|(_, window)| window.output().name())
            .collect()
    }

    fn on(&self, output: &str) -> usize {
        self.windows
            .iter()
            .filter(|(_, window)| window.output().name() == output)
            .count()
    }

    fn id_on(&self, output: &str) -> u32 {
        self.windows
            .iter()
            .find(|(_, window)| window.output().name() == output)
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("no window on {output}"))
    }
}

/// A bar for `output` with a signal of its own, so each output's owner
/// holds a node.
fn bar(cx: &Cx, output: &Output) -> PanelWindow {
    let label = cx.signal(output.name());
    PanelWindow::new(output).child(Text::new(move || label.get()))
}

/// A stored closure, as `cx.per_output` makes one: one window or several.
fn build<W: IntoPanels>(f: impl Fn(&Cx, &Output) -> W + 'static) -> Build {
    Rc::new(move |cx: &Cx, output: &Output| f(cx, output).into_panels())
}

#[test]
fn every_output_gets_its_windows_now_and_later() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    let builder = state.borrow_mut().add(root.clone(), build(bar));
    // The outputs there when per_output was called, then one more.
    per_output::existing(&state, &mut host, builder, &["DP-1".into(), "DP-2".into()]);
    per_output::added(&state, &mut host, "HDMI-A-1");
    assert_eq!(host.outputs(), ["DP-1", "DP-2", "HDMI-A-1"]);
    // An output announced again while its windows are up gets no more.
    per_output::added(&state, &mut host, "DP-1");
    per_output::existing(&state, &mut host, builder, &["DP-2".into()]);
    assert_eq!(host.windows.len(), 3);
}

#[test]
fn a_removed_output_drops_everything_built_for_it() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    state.borrow_mut().add(root.clone(), build(bar));
    per_output::added(&state, &mut host, "DP-1");
    runtime.flush();
    let nodes = runtime.node_count();

    per_output::added(&state, &mut host, "DP-2");
    runtime.flush();
    assert!(runtime.node_count() > nodes);
    per_output::removed(&state, &mut host, "DP-2", &[]);
    runtime.flush();
    assert_eq!(host.outputs(), ["DP-1"]);
    assert_eq!(runtime.node_count(), nodes, "its owner was disposed");

    // Plugged back in, it is built for again.
    per_output::added(&state, &mut host, "DP-2");
    assert_eq!(host.outputs(), ["DP-1", "DP-2"]);
}

#[test]
fn a_closed_window_drops_its_outputs_windows_until_the_output_comes_back() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    // Two windows per output: a top and a bottom bar.
    state.borrow_mut().add(
        root.clone(),
        build(|cx, output| vec![bar(cx, output), bar(cx, output)]),
    );
    per_output::added(&state, &mut host, "DP-1");
    runtime.flush();
    let nodes = runtime.node_count();
    per_output::added(&state, &mut host, "DP-2");
    let closed = host.id_on("DP-2");
    // The platform drops the closed window itself, then reports it.
    host.remove_window(closed);
    let logs = capture_logs(|| per_output::closed(&state, &mut host, closed));
    assert!(logs.contains("the compositor closed a window"), "{logs}");
    assert_eq!(
        host.outputs(),
        ["DP-1", "DP-1"],
        "the other DP-2 window went too"
    );
    runtime.flush();
    assert_eq!(runtime.node_count(), nodes, "DP-2's owner was disposed");

    // Not rebuilt for DP-2 on another output's event, or when asked again.
    per_output::added(&state, &mut host, "HDMI-A-1");
    per_output::existing(&state, &mut host, 0, &["DP-2".into()]);
    assert_eq!(host.outputs(), ["DP-1", "DP-1", "HDMI-A-1", "HDMI-A-1"]);
    // Once DP-2 is removed and announced again, it is.
    per_output::removed(&state, &mut host, "DP-2", &[]);
    per_output::added(&state, &mut host, "DP-2");
    assert_eq!(
        host.outputs().iter().filter(|name| *name == "DP-2").count(),
        2
    );
    // A window per_output didn't build is none of its business.
    per_output::closed(&state, &mut host, 999);
    assert_eq!(host.windows.len(), 6);
}

#[test]
fn a_panic_for_one_output_is_logged_and_the_others_are_built() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    let made = Rc::new(Cell::new(None));
    let kept = Rc::clone(&made);
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            if output.name() == "DP-2" {
                // Something created before the panic is freed with it.
                kept.set(Some(cx.signal(1)));
                panic!("no bar for DP-2");
            }
            bar(cx, output)
        }),
    );
    let logs = capture_logs(|| {
        per_output::added(&state, &mut host, "DP-1");
        per_output::added(&state, &mut host, "DP-2");
        per_output::added(&state, &mut host, "DP-3");
    });
    assert!(
        logs.contains("per_output[DP-2] (called at crates/crabshell/src/tests/per_output.rs:"),
        "the path names the call site: {logs}"
    );
    assert!(
        logs.contains("panicked, so nothing is shown on DP-2: no bar for DP-2"),
        "{logs}"
    );
    assert_eq!(host.outputs(), ["DP-1", "DP-3"]);
    let signal = made.get().expect("the closure ran for DP-2");
    assert_eq!(signal.try_get(), None, "DP-2's owner was disposed");
    // It isn't retried until DP-2 is announced again.
    per_output::added(&state, &mut host, "DP-2");
    assert_eq!(host.windows.len(), 2);
}

#[test]
fn a_closure_may_call_per_output_again() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = Rc::new(RefCell::new(PerOutput::new()));
    let mut host = Fake::default();
    let inner = Rc::clone(&state);
    let once = Cell::new(false);
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            // No borrow of the state is held while the closure runs.
            if !once.replace(true) {
                inner.borrow_mut().add(cx.clone(), build(bar));
            }
            bar(cx, output)
        }),
    );
    per_output::added(&state, &mut host, "DP-1");
    per_output::added(&state, &mut host, "DP-2");
    // The second builder, registered while the first ran for DP-1, builds
    // for DP-2 when it is announced. For DP-1, which was there already,
    // cx.per_output asks for it after this event, as here.
    assert_eq!(host.outputs(), ["DP-1", "DP-2", "DP-2"]);
    per_output::existing(&state, &mut host, 1, &["DP-1".into(), "DP-2".into()]);
    assert_eq!(host.outputs(), ["DP-1", "DP-2", "DP-2", "DP-1"]);
}

#[test]
fn per_output_without_a_shell_logs_and_builds_nothing() {
    use crate::ShellExt;

    let root = Cx::new(Runtime::current().new_owner());
    let built = Rc::new(Cell::new(false));
    let ran = Rc::clone(&built);
    let logs = capture_logs(|| {
        root.per_output(move |cx, output| {
            ran.set(true);
            bar(cx, output)
        });
        root.set_icon_theme("Adwaita");
    });
    assert!(
        logs.contains("cx.per_output at crates/crabshell/src/tests/per_output.rs:"),
        "the log names the call site: {logs}"
    );
    assert!(
        logs.contains("needs the shell crabshell::run starts"),
        "{logs}"
    );
    assert!(logs.contains("cx.set_icon_theme needs the shell"), "{logs}");
    assert!(!built.get());
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("log buffer lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = logs.0.lock().expect("log buffer lock").clone();
    String::from_utf8(bytes).expect("log output is UTF-8")
}

/// `f` calls `cx.per_output` (what ShellExt::per_output does: register with
/// f's own context, then build for the outputs there now from an idle
/// callback), then panics. `build` disposes f's context, so the idle
/// callback's build for the new builder panics on `cx.child()`, outside
/// build's catch_unwind. In the shell that panic leaves the calloop idle
/// callback and Shell::dispatch: the process dies.
#[test]
fn a_builder_whose_context_died_is_skipped_without_a_panic() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = Rc::new(RefCell::new(PerOutput::new()));
    let mut host = Fake::default();
    let inner = Rc::clone(&state);
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            inner.borrow_mut().add(cx.clone(), build(bar));
            if output.name() == "DP-1" {
                panic!("f fails after calling per_output");
            }
            bar(cx, output)
        }),
    );
    per_output::added(&state, &mut host, "DP-1");
    // The idle callback cx.per_output queued for builder 1.
    let escaped = panic::catch_unwind(AssertUnwindSafe(|| {
        per_output::existing(&state, &mut host, 1, &["DP-1".into()]);
    }));
    assert!(
        escaped.is_ok(),
        "a panic escaped per_output's bookkeeping (it would kill the shell from the idle callback)"
    );
}

/// A builder registered from inside f for DP-1 also builds on DP-2. When
/// DP-1 goes, that builder's context is disposed, but its window on DP-2 is
/// still held (and shown) with every node in it freed. Then a new output
/// makes the stale builder panic, and builders after it never build there.
/// Each output's name and the label signal the nested builder made there.
type Labels = Vec<(String, Signal<String>)>;

#[test]
fn a_builders_windows_go_with_its_context() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = Rc::new(RefCell::new(PerOutput::new()));
    let mut host = Fake::default();
    let inner = Rc::clone(&state);
    let nested_labels: Rc<RefCell<Labels>> = Rc::default();
    let labels = Rc::clone(&nested_labels);
    // Builder 0: a bar everywhere; on DP-1 it also asks for an extra bar
    // on every output (builder 1, under DP-1's context).
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            if output.name() == "DP-1" {
                let labels = Rc::clone(&labels);
                inner.borrow_mut().add(
                    cx.clone(),
                    build(move |cx: &Cx, output: &Output| {
                        let label = cx.signal(output.name());
                        labels.borrow_mut().push((output.name(), label));
                        PanelWindow::new(output).child(Text::new(move || label.get()))
                    }),
                );
            }
            bar(cx, output)
        }),
    );
    per_output::added(&state, &mut host, "DP-1");
    per_output::existing(&state, &mut host, 1, &["DP-1".into()]);
    per_output::added(&state, &mut host, "DP-2");
    assert_eq!((host.on("DP-1"), host.on("DP-2")), (2, 2));
    // Builder 2, at the root, registered later.
    let ran_on = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = Rc::clone(&ran_on);
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            seen.borrow_mut().push(output.name());
            bar(cx, output)
        }),
    );

    per_output::removed(&state, &mut host, "DP-1", &["DP-2".into()]);
    let dp2_label = nested_labels
        .borrow()
        .iter()
        .find(|(name, _)| name == "DP-2")
        .map(|(_, label)| *label)
        .expect("builder 1 built on DP-2");
    let stale = host.on("DP-2") == 2 && dp2_label.try_get().is_none();

    let panicked = Cell::new(false);
    if panic::catch_unwind(AssertUnwindSafe(|| {
        per_output::added(&state, &mut host, "DP-3")
    }))
    .is_err()
    {
        panicked.set(true);
    }
    let built_on_dp3 = ran_on.borrow().iter().any(|name| name == "DP-3");
    assert!(
        !stale && !panicked.get() && built_on_dp3,
        "stale window still held on DP-2 with its nodes freed: {stale}; \
         added(DP-3) panicked: {}; builder 2 built on DP-3: {built_on_dp3}",
        panicked.get()
    );
}

/// The same name announced again before the old output's removal arrives
/// (two wl_outputs called DP-1 at once): the new one gets nothing, then
/// the old one's removal drops what was built, and DP-1 has no bar.
#[test]
fn a_name_announced_again_before_the_old_ones_removal_keeps_its_bar() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    state.borrow_mut().add(root.clone(), build(bar));
    per_output::added(&state, &mut host, "DP-1"); // the old wl_output
    per_output::added(&state, &mut host, "DP-1"); // the new one, same name
    per_output::removed(&state, &mut host, "DP-1", &["DP-1".into()]); // the old one goes
    assert_eq!(host.on("DP-1"), 1, "the DP-1 still there has a bar");
}

/// per_output from a scoped effect, whose owner is new on every run: each
/// run's builder ends with that run, and its windows go. (From a plain
/// effect, per_output's documentation says not to.)
#[test]
fn a_builder_from_a_scoped_effect_ends_with_each_run() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = Rc::new(RefCell::new(PerOutput::new()));
    let host = Rc::new(RefCell::new(Fake::default()));
    let trigger = root.signal(0);
    let (inner, inner_host) = (Rc::clone(&state), Rc::clone(&host));
    root.owner().effect_scoped(move |scope| {
        trigger.get();
        let builder = inner.borrow_mut().add(Cx::new(scope), build(bar));
        per_output::existing(
            &inner,
            &mut *inner_host.borrow_mut(),
            builder,
            &["DP-1".into()],
        );
    });
    runtime.flush();
    for n in 1..=5 {
        trigger.set(n);
        runtime.flush();
    }
    per_output::sweep(&state, &mut *host.borrow_mut());
    assert_eq!(
        host.borrow().on("DP-1"),
        1,
        "only the last run's bar is left"
    );
}

/// A builder whose context goes while an output is being announced, here
/// disposed by the builder before it, isn't run and logs nothing alarming.
#[test]
fn a_builder_whose_context_goes_mid_announcement_is_skipped_quietly() {
    let runtime = Runtime::current();
    let root = Cx::new(runtime.new_owner());
    let state = RefCell::new(PerOutput::new());
    let mut host = Fake::default();
    let doomed = root.child();
    let doomed_owner = doomed.owner();
    state.borrow_mut().add(
        root.clone(),
        build(move |cx, output| {
            if output.name() == "DP-2" {
                doomed_owner.dispose();
            }
            bar(cx, output)
        }),
    );
    state.borrow_mut().add(doomed, build(bar));
    per_output::added(&state, &mut host, "DP-1");
    assert_eq!(host.on("DP-1"), 2);
    let logs = capture_logs(|| per_output::added(&state, &mut host, "DP-2"));
    assert!(!logs.contains("panicked"), "{logs}");
    // The second builder's DP-1 bar went with its context too.
    assert_eq!((host.on("DP-1"), host.on("DP-2")), (1, 1));
}
