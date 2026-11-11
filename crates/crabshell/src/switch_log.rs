//! Which commit shows a new workspace list, for the log line Phase 3's exit
//! check reads: "workspace switch on screen in 4.2 ms".

use std::time::Instant;

/// Follows a workspace change from Sway's event until a commit shows it.
#[derive(Default)]
pub(crate) struct SwitchLog {
    /// The event behind the last list seen, so each is logged once.
    seen: Option<Instant>,
    /// The change still to reach the screen: its event, and the commit
    /// count before the dispatch that brought it.
    waiting: Option<(Instant, u64)>,
}

impl SwitchLog {
    /// Called after each dispatch with the event behind the current list
    /// (`None` for a list no event led to), the commit counts before and
    /// after the dispatch, and whether a shown window has damage left to
    /// draw. Returns the event of the change this dispatch finished
    /// showing: a frame was committed since it came and no shown window has
    /// more to draw, so with several windows it is the last one's commit.
    /// A change that drew nothing is dropped unlogged.
    pub(crate) fn after_dispatch(
        &mut self,
        changed_at: Option<Instant>,
        before: u64,
        after: u64,
        drawing: impl FnOnce() -> bool,
    ) -> Option<Instant> {
        if let Some(event) = changed_at
            && changed_at != self.seen
        {
            self.seen = changed_at;
            self.waiting = Some((event, before));
        }
        let (event, commits) = self.waiting?;
        if drawing() {
            return None;
        }
        self.waiting = None;
        (after > commits).then_some(event)
    }
}
