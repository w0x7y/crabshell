//! Unit tests: everything that can be checked without a compositor.

mod buffers;
mod outputs;
mod pacing;
mod scale;
mod surface;

use crabshell_ui::{Output, PanelWindow};

/// A panel for output `name`.
fn panel(name: &str) -> PanelWindow {
    PanelWindow::new(&Output::new(name))
}
