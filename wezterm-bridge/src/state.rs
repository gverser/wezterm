use mux::pane::PaneId;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use termwiz::surface::SequenceNo;

/// The subset of `mux::pane::Pane` that the bridge needs.
///
/// `Pane` has dozens of methods, and depending on all of them would leave the
/// state machine untestable. Production wraps a real `Pane`; tests use a fake.
pub trait PaneAccess {
    fn pane_id(&self) -> PaneId;
    fn title(&self) -> String;
    /// The mux workspace this pane lives in, or "" if it cannot be resolved.
    fn workspace(&self) -> String;
    /// The directory the pane is sitting in, as a plain path.
    fn cwd(&self) -> Option<String>;
    /// The name the sidebar gives this pane's session, for panes started from
    /// it. A pane opened by hand belongs to no session and has none.
    fn session(&self) -> Option<String>;
    fn current_seqno(&self) -> SequenceNo;
    fn changed_since(&self, seqno: SequenceNo) -> bool;
    /// The last `max_lines` lines of the pane as plain text, together with the
    /// stable row index of the first line returned.
    fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>);
    fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()>;
    /// Press a named key, with optional modifiers. Refuses a name it does not
    /// recognise rather than guessing.
    fn send_key(&self, key: &str, modifiers: &[String]) -> anyhow::Result<()>;
}

/// What the bridge remembers about one pane: about 60 bytes, however much
/// that pane has printed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneState {
    pub pane_id: PaneId,
    pub last_seqno: SequenceNo,
    pub dirty: bool,
    /// Whether the user asked for this pane to be mirrored. Panes are tracked
    /// whether or not they are subscribed -- the client wants to know they
    /// exist, so it can offer them -- but only subscribed ones have their
    /// output read and sent.
    pub subscribed: bool,
}

impl PaneState {
    fn new(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            last_seqno: 0,
            subscribed: false,
            dirty: false,
        }
    }
}

#[derive(Default)]
pub struct BridgeState {
    panes: Mutex<BTreeMap<PaneId, PaneState>>,
}

impl BridgeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pane(&self, pane_id: PaneId) {
        self.panes
            .lock()
            .entry(pane_id)
            .or_insert_with(|| PaneState::new(pane_id));
    }

    pub fn remove_pane(&self, pane_id: PaneId) {
        self.panes.lock().remove(&pane_id);
    }

    /// Note that a pane produced output.
    ///
    /// Called from the mux notification thread and deliberately cheap: it
    /// sets a bit and returns. Reading what was printed happens later, on the
    /// flush timer, straight out of the scrollback.
    pub fn mark_dirty(&self, pane_id: PaneId) {
        if let Some(entry) = self.panes.lock().get_mut(&pane_id) {
            entry.dirty = true;
        }
    }

    /// The subscribed panes with pending output, clearing every pane's flag.
    ///
    /// Flags are cleared for unsubscribed panes too, on purpose: turning a pane
    /// on should start mirroring from that moment, not replay whatever it
    /// happened to print while nobody was listening.
    pub fn take_dirty(&self) -> Vec<PaneId> {
        let mut panes = self.panes.lock();
        let mut out = vec![];
        for (id, entry) in panes.iter_mut() {
            if entry.dirty {
                entry.dirty = false;
                if entry.subscribed {
                    out.push(*id);
                }
            }
        }
        out
    }

    /// Turn mirroring on or off for one pane.
    ///
    /// Silently does nothing for a pane the bridge does not know about: the
    /// notification can arrive after the pane is gone.
    pub fn set_subscribed(&self, pane_id: PaneId, subscribed: bool) {
        if let Some(entry) = self.panes.lock().get_mut(&pane_id) {
            entry.subscribed = subscribed;
        }
    }

    pub fn is_subscribed(&self, pane_id: PaneId) -> bool {
        self.panes
            .lock()
            .get(&pane_id)
            .map_or(false, |e| e.subscribed)
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.panes.lock().keys().copied().collect()
    }

    pub fn set_seqno(&self, pane_id: PaneId, seqno: SequenceNo) {
        if let Some(entry) = self.panes.lock().get_mut(&pane_id) {
            entry.last_seqno = seqno;
        }
    }

    pub fn seqno(&self, pane_id: PaneId) -> Option<SequenceNo> {
        self.panes.lock().get(&pane_id).map(|e| e.last_seqno)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Pane 7, present and mirrored -- the ordinary case most of these tests
    /// are about.
    fn subscribed_state() -> BridgeState {
        let state = BridgeState::new();
        state.add_pane(7);
        state.set_subscribed(7, true);
        state
    }

    #[test]
    fn a_new_pane_starts_clean() {
        let state = subscribed_state();
        assert_eq!(state.take_dirty(), Vec::<PaneId>::new());
    }

    #[test]
    fn a_new_pane_starts_unsubscribed() {
        let state = BridgeState::new();
        state.add_pane(7);
        assert!(
            !state.is_subscribed(7),
            "mirroring is opt-in; a pane the user never asked for must not be sent"
        );
    }

    #[test]
    fn output_marks_the_pane_dirty() {
        let state = subscribed_state();
        state.mark_dirty(7);
        assert_eq!(state.take_dirty(), vec![7]);
    }

    #[test]
    fn an_unsubscribed_pane_is_never_handed_over() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "its output is nobody's business until the user asks for it"
        );
    }

    #[test]
    fn output_from_before_subscribing_is_not_replayed() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        state.take_dirty(); // the flush that happened while nobody was listening
        state.set_subscribed(7, true);
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "turning a pane on starts mirroring from that moment, not from its history"
        );
    }

    #[test]
    fn unsubscribing_stops_the_flow() {
        let state = subscribed_state();
        state.set_subscribed(7, false);
        state.mark_dirty(7);
        assert_eq!(state.take_dirty(), Vec::<PaneId>::new());
    }

    #[test]
    fn taking_dirty_clears_the_flag() {
        let state = subscribed_state();
        state.mark_dirty(7);
        state.take_dirty();
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "a flush consumes the flag; the next flush has nothing to do"
        );
    }

    #[test]
    fn repeated_output_does_not_accumulate() {
        let state = subscribed_state();
        for _ in 0..10_000 {
            state.mark_dirty(7);
        }
        assert_eq!(
            state.take_dirty(),
            vec![7],
            "ten thousand output events collapse into one unit of work"
        );
    }

    #[test]
    fn output_from_an_unknown_pane_is_ignored() {
        let state = BridgeState::new();
        state.mark_dirty(99);
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "a pane we were never told about has no thread to send to"
        );
    }

    #[test]
    fn removing_a_pane_drops_its_entry() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        state.remove_pane(7);
        assert_eq!(state.pane_ids(), Vec::<PaneId>::new());
        assert_eq!(state.take_dirty(), Vec::<PaneId>::new());
    }

    #[test]
    fn the_watermark_survives_a_flush() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.set_seqno(7, 1182);
        state.mark_dirty(7);
        state.take_dirty();
        assert_eq!(state.seqno(7), Some(1182));
    }
}
