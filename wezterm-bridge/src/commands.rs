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
            message: "已经认证过了".to_string(),
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
                    message: format!("发送到窗格 {pane} 失败：{err:#}"),
                }],
            },
        },

        Inbound::Key {
            pane,
            key,
            modifiers,
        } => match lookup(pane) {
            None => vec![no_such_pane(pane)],
            Some(p) => match p.send_key(&key, &modifiers) {
                Ok(()) => vec![],
                Err(err) => vec![Outbound::Error {
                    message: format!("向窗格 {pane} 发送按键 {key} 失败：{err:#}"),
                }],
            },
        },

        Inbound::List => {
            let panes = state
                .pane_ids()
                .into_iter()
                .filter_map(|id| lookup(id).map(|pane| describe(&pane, state)))
                .collect();
            vec![Outbound::Panes { panes }]
        }

        Inbound::Spawn { .. } | Inbound::Kill { .. } => vec![Outbound::Error {
            message: "spawn and kill are handled by the mux layer".to_string(),
        }],
    }
}

/// Describe one pane, the single way the client is ever told about one.
///
/// A pane arriving and a pane being listed are the same pane. Building the
/// description in two places is how the list ends up saying something the
/// notification did not.
pub fn describe<P: PaneAccess>(pane: &P, state: &BridgeState) -> PaneInfo {
    let id = pane.pane_id();
    PaneInfo {
        pane: id,
        workspace: pane.workspace(),
        title: pane.title(),
        cwd: pane.cwd(),
        session: pane.session(),
        subscribed: state.is_subscribed(id),
    }
}

fn no_such_pane(pane: PaneId) -> Outbound {
    Outbound::Error {
        message: format!("没有这个窗格：{pane}"),
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
        keys: Arc<Mutex<Vec<(String, Vec<String>)>>>,
    }

    impl FakePane {
        fn new(id: PaneId) -> Self {
            Self {
                id,
                seqno: 100,
                lines: vec!["first".to_string(), "second".to_string()],
                sent: Arc::new(Mutex::new(vec![])),
                keys: Arc::new(Mutex::new(vec![])),
            }
        }
    }

    impl PaneAccess for FakePane {
        fn pane_id(&self) -> PaneId {
            self.id
        }
        fn title(&self) -> String {
            format!("fake-{}", self.id)
        }
        fn workspace(&self) -> String {
            format!("session:{}", self.id)
        }
        fn cwd(&self) -> Option<String> {
            Some(format!("D:\\fake\\{}", self.id))
        }
        fn session(&self) -> Option<String> {
            Some(format!("session-{}", self.id))
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
        fn send_key(&self, key: &str, modifiers: &[String]) -> anyhow::Result<()> {
            if crate::keys::parse_key(key).is_none() {
                anyhow::bail!("不认识的按键：{key}");
            }
            self.keys.lock().push((key.to_string(), modifiers.to_vec()));
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
                message: "没有这个窗格：99".to_string()
            }],
            "a stale pane id from the client must not take the service down"
        );
    }

    #[test]
    fn a_key_reaches_the_pane() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let keys = pane.keys.clone();
        let out = dispatch(
            Inbound::Key {
                pane: 7,
                key: "down".to_string(),
                modifiers: vec![],
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert!(out.is_empty(), "a key that landed needs no reply");
        assert_eq!(*keys.lock(), vec![("down".to_string(), vec![])]);
    }

    #[test]
    fn a_key_carries_its_modifiers() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let keys = pane.keys.clone();
        dispatch(
            Inbound::Key {
                pane: 7,
                key: "c".to_string(),
                modifiers: vec!["ctrl".to_string()],
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            *keys.lock(),
            vec![("c".to_string(), vec!["ctrl".to_string()])],
            "ctrl-c is the whole point of being able to send modifiers"
        );
    }

    #[test]
    fn a_key_nobody_recognises_is_an_error_rather_than_a_guess() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let out = dispatch(
            Inbound::Key {
                pane: 7,
                key: "banana".to_string(),
                modifiers: vec![],
            },
            &state,
            |_| Some(pane.clone()),
        );
        match &out[0] {
            Outbound::Error { message } => assert!(message.contains("banana"), "{message}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert!(
            pane.keys.lock().is_empty(),
            "sending some other key into a confirmation dialog answers a question nobody read"
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
                assert_eq!(panes[1].pane, 9);
                assert_eq!(
                    panes[0],
                    describe(&FakePane::new(7), &state),
                    "list must describe a pane the same way pane_added did"
                );
            }
            other => panic!("expected a Panes reply, got {other:?}"),
        }
    }

    #[test]
    fn a_described_pane_says_where_it_is() {
        // A bare id is unreadable on a phone. These three fields are the whole
        // reason the client can say "casb - cdeb-svc" instead of "#2".
        let info = describe(&FakePane::new(7), &BridgeState::new());
        assert_eq!(info.workspace, "session:7");
        assert_eq!(info.cwd, Some("D:\\fake\\7".to_string()));
        assert_eq!(info.session, Some("session-7".to_string()));
    }

    #[test]
    fn list_carries_the_subscription_state() {
        // The client reconnects to panes that were already being mirrored.
        // PaneMirror only fires on change, so without this the client comes
        // back believing nothing is being watched.
        let state = BridgeState::new();
        state.add_pane(7);
        state.add_pane(9);
        state.set_subscribed(9, true);
        let out = dispatch(Inbound::List, &state, |id| Some(FakePane::new(id)));
        match &out[0] {
            Outbound::Panes { panes } => {
                assert!(!panes[0].subscribed, "pane 7 was never turned on");
                assert!(panes[1].subscribed, "pane 9 was");
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
