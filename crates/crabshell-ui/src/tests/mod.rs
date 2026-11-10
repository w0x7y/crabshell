//! Tests for the widget tree, by area. Each test runs on its own thread, so
//! each starts with an empty reactive runtime.

mod style;
mod text;
mod window;

use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crabshell_reactive::Runtime;

use crate::{Cx, NodeView, Output, PanelWindow, Size, Window};

/// A runtime and a root context, with helpers to build windows the way the
/// platform does.
struct Harness {
    runtime: Runtime,
    cx: Cx,
}

impl Harness {
    fn new() -> Harness {
        let runtime = Runtime::current();
        Harness {
            runtime,
            cx: Cx::new(runtime.new_owner()),
        }
    }

    /// A panel on output "DP-1", for `panel` to finish.
    fn panel(&self) -> PanelWindow {
        PanelWindow::new(&Output::new("DP-1"))
    }

    /// Builds `panel` at `width` by `height` and runs the bindings.
    fn window(&mut self, panel: PanelWindow, width: f32, height: f32) -> Window {
        let window = Window::new(&self.cx, panel);
        window.set_size(Size::new(width, height));
        self.frame(&window);
        window
    }

    /// One event-loop iteration.
    fn frame(&mut self, _window: &Window) {
        self.runtime.flush();
    }
}

/// The node at `path`, relative to the window's root, as a list of child
/// indexes.
fn node_at<'a>(root: NodeView<'a>, path: &[usize]) -> NodeView<'a> {
    path.iter().fold(root, |node, &index| {
        node.children()
            .nth(index)
            .unwrap_or_else(|| panic!("{} has no child {index}", node.path()))
    })
}

/// The widget path of every node, depth first.
fn paths(window: &Window) -> Vec<String> {
    fn walk(node: NodeView<'_>, out: &mut Vec<String>) {
        out.push(node.path().to_owned());
        for child in node.children() {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(window.view().root(), &mut out);
    out
}

/// Counts how many times a closure ran.
#[derive(Clone, Default)]
struct Counter(Rc<Cell<u32>>);

impl Counter {
    fn hit(&self) {
        self.0.set(self.0.get() + 1);
    }

    fn get(&self) -> u32 {
        self.0.get()
    }
}

/// Runs `f` and returns everything it logged through `tracing`.
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buffer {
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

    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buffer.0.lock().expect("log buffer lock").clone();
    String::from_utf8(bytes).expect("log output is UTF-8")
}
