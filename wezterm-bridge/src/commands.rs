use crate::proto::{Inbound, Outbound, PaneInfo};
use crate::state::{BridgeState, PaneAccess};
use mux::pane::PaneId;

/// Turn one client message into the replies it warrants.
///
/// `lookup` resolves a pane id. Returning None means the pane is gone, which
/// is expected at any time: the client is always working from a list that can
/// go stale between one message and the next.
///
/// Spawn and Kill are not handled here. They need the live mux and are async,
/// so they are dealt with a layer up; the arm below exists to make a wiring
/// mistake obvious rather than silent.
pub fn dispatch<P, F>(msg: Inbound, state: &BridgeState, lookup: F) -> Vec<Outbound>
where
    P: PaneAccess,
    F: Fn(PaneId) -> Option<P>,
{
    match msg {
        Inbound::Auth { .. } => vec![Outbound::Error {
            message: "already authenticated".to_string(),
        }],

        Inbound::Read { pane, max_lines } => match lookup(pane) {
            None => vec![no_such_pane(pane)],
            Some(p) => {
                let seqno = p.current_seqno();
                let (first_row, lines) = p.read_lines(max_lines);
                state.set_seqno(pane, seqno);
                vec![Outbound::Lines {
                    pane,
                    first_row,
                    lines,
                    seqno,
                }]
            }
        },

        Inbound::Send { pane, text, submit } => match lookup(pane) {
            None => vec![no_such_pane(pane)],
            Some(p) => match p.send_text(&text, submit) {
                Ok(()) => vec![],
                Err(err) => vec![Outbound::Error {
                    message: format!("send to pane {pane} failed: {err:#}"),
                }],
            },
        },

        Inbound::List => {
            let panes = state
                .pane_ids()
                .into_iter()
                .filter_map(|id| {
                    lookup(id).map(|_| PaneInfo {
                        pane: id,
                        workspace: String::new(),
                        title: String::new(),
                        cwd: None,
                    })
                })
                .collect();
            vec![Outbound::Panes { panes }]
        }

        Inbound::Spawn { .. } | Inbound::Kill { .. } => vec![Outbound::Error {
            message: "spawn and kill are handled by the mux layer".to_string(),
        }],
    }
}

fn no_such_pane(pane: PaneId) -> Outbound {
    Outbound::Error {
        message: format!("no such pane: {pane}"),
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use termwiz::surface::SequenceNo;

    #[derive(Clone)]
    struct FakePane {
        id: PaneId,
        seqno: SequenceNo,
        lines: Vec<String>,
        sent: Arc<Mutex<Vec<(String, bool)>>>,
    }

    impl FakePane {
        fn new(id: PaneId) -> Self {
            Self {
                id,
                seqno: 100,
                lines: vec!["first".to_string(), "second".to_string()],
                sent: Arc::new(Mutex::new(vec![])),
            }
        }
    }

    impl PaneAccess for FakePane {
        fn pane_id(&self) -> PaneId {
            self.id
        }
        fn current_seqno(&self) -> SequenceNo {
            self.seqno
        }
        fn changed_since(&self, seqno: SequenceNo) -> bool {
            seqno < self.seqno
        }
        fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>) {
            let start = self.lines.len().saturating_sub(max_lines);
            (start as isize, self.lines[start..].to_vec())
        }
        fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()> {
            self.sent.lock().push((text.to_string(), submit));
            Ok(())
        }
    }

    #[test]
    fn read_returns_the_lines_and_the_watermark() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let out = dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 10,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            out,
            vec![Outbound::Lines {
                pane: 7,
                first_row: 0,
                lines: vec!["first".to_string(), "second".to_string()],
                seqno: 100,
            }]
        );
    }

    #[test]
    fn read_advances_the_watermark() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 10,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            state.seqno(7),
            Some(100),
            "the next read must not resend what this one already returned"
        );
    }

    #[test]
    fn read_honours_max_lines() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let out = dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 1,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            out,
            vec![Outbound::Lines {
                pane: 7,
                first_row: 1,
                lines: vec!["second".to_string()],
                seqno: 100,
            }]
        );
    }

    #[test]
    fn send_reaches_the_pane() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let sent = pane.sent.clone();
        dispatch(
            Inbound::Send {
                pane: 7,
                text: "继续".to_string(),
                submit: true,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(*sent.lock(), vec![("继续".to_string(), true)]);
    }

    #[test]
    fn a_command_for_an_unknown_pane_reports_an_error() {
        let state = BridgeState::new();
        let out = dispatch(
            Inbound::Read {
                pane: 99,
                max_lines: 10,
            },
            &state,
            |_| None::<FakePane>,
        );
        assert_eq!(
            out,
            vec![Outbound::Error {
                message: "no such pane: 99".to_string()
            }],
            "a stale pane id from the client must not take the service down"
        );
    }

    #[test]
    fn list_reports_the_known_panes() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.add_pane(9);
        let out = dispatch(Inbound::List, &state, |id| Some(FakePane::new(id)));
        match &out[0] {
            Outbound::Panes { panes } => {
                assert_eq!(panes.len(), 2);
                assert_eq!(panes[0].pane, 7);
                assert_eq!(panes[1].pane, 9);
            }
            other => panic!("expected a Panes reply, got {other:?}"),
        }
    }

    #[test]
    fn list_drops_panes_the_mux_no_longer_has() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.add_pane(9);
        // 9 has gone away without us hearing about it yet.
        let out = dispatch(Inbound::List, &state, |id| {
            if id == 9 {
                None
            } else {
                Some(FakePane::new(id))
            }
        });
        match &out[0] {
            Outbound::Panes { panes } => {
                assert_eq!(panes.len(), 1, "a pane the mux has lost is not reported");
                assert_eq!(panes[0].pane, 7);
            }
            other => panic!("expected a Panes reply, got {other:?}"),
        }
    }
}
