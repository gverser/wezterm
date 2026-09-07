use mux::pane::PaneId;
use serde::{Deserialize, Serialize};
use termwiz::surface::SequenceNo;

fn default_true() -> bool {
    true
}

fn default_max_lines() -> usize {
    200
}

/// A pane as described to the client.
///
/// Everything here answers the same question -- "which window is this?" -- and
/// a bare pane id answers it badly. What reads these is a person looking at
/// their phone, with no terminal in front of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: PaneId,
    pub workspace: String,
    pub title: String,
    pub cwd: Option<String>,
    /// The sidebar's name for the session this pane belongs to, when it
    /// belongs to one -- "casb" rather than "session:2".
    pub session: Option<String>,
    /// Whether the user has asked for this pane to be mirrored. Every pane is
    /// described to the client so it can offer them as targets, so without
    /// this the client cannot tell the ones being watched from the ones that
    /// merely exist.
    pub subscribed: bool,
}

/// Bridge -> client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Outbound {
    AuthOk,
    /// A newtype rather than its own copy of the fields: a pane that was added
    /// and a pane that was listed are the same pane, and describing it twice is
    /// how the two descriptions drift apart. Serde's internal tagging keeps the
    /// wire format flat, so the client still reads `msg.pane` and `msg.title`
    /// straight off the message.
    PaneAdded(PaneInfo),
    PaneRemoved {
        pane: PaneId,
    },
    /// A pane's title changed.
    ///
    /// Claude Code writes what it is working on into the title, and rewrites it
    /// when the user renames the session. The title captured at pane creation
    /// says "cmd.exe" forever, which tells nobody anything.
    PaneTitle {
        pane: PaneId,
        title: String,
    },
    /// Mirroring was turned on or off for a pane.
    ///
    /// This, not `pane_added`, is the moment worth announcing: it is the user
    /// saying "this one I want to watch", rather than a shell they opened to
    /// run one command in.
    PaneMirror {
        pane: PaneId,
        enabled: bool,
    },
    Dirty {
        pane: PaneId,
        seqno: SequenceNo,
    },
    Lines {
        pane: PaneId,
        first_row: isize,
        lines: Vec<String>,
        seqno: SequenceNo,
    },
    Alert {
        pane: PaneId,
        kind: String,
    },
    Panes {
        panes: Vec<PaneInfo>,
    },
    Error {
        message: String,
    },
}

/// Client -> bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Inbound {
    Auth {
        token: String,
    },
    Send {
        pane: PaneId,
        text: String,
        #[serde(default = "default_true")]
        submit: bool,
    },
    Read {
        pane: PaneId,
        #[serde(default = "default_max_lines")]
        max_lines: usize,
    },
    /// Press a key. Text alone cannot answer a selection list or a
    /// confirmation box, and those are what a CLI puts up when it most needs
    /// an answer.
    Key {
        pane: PaneId,
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Spawn {
        node_id: u64,
    },
    Kill {
        pane: PaneId,
    },
    List,
}

#[cfg(test)]
mod test {
    use super::*;

    fn info(pane: PaneId) -> PaneInfo {
        PaneInfo {
            pane,
            workspace: "session:6".to_string(),
            title: "Claude Code".to_string(),
            cwd: Some("D:\\dev\\remote-controll".to_string()),
            session: Some("casb".to_string()),
            subscribed: true,
        }
    }

    #[test]
    fn inbound_send_parses() {
        let json = r#"{"t":"send","pane":7,"text":"继续","submit":true}"#;
        let msg: Inbound = serde_json::from_str(json).expect("to parse");
        assert_eq!(
            msg,
            Inbound::Send {
                pane: 7,
                text: "继续".to_string(),
                submit: true,
            }
        );
    }

    #[test]
    fn submit_defaults_to_true() {
        let json = r#"{"t":"send","pane":7,"text":"hi"}"#;
        let msg: Inbound = serde_json::from_str(json).expect("to parse");
        assert_eq!(
            msg,
            Inbound::Send {
                pane: 7,
                text: "hi".to_string(),
                submit: true,
            },
            "a message without an explicit submit is meant to be sent"
        );
    }

    #[test]
    fn inbound_list_parses_without_fields() {
        let msg: Inbound = serde_json::from_str(r#"{"t":"list"}"#).expect("to parse");
        assert_eq!(msg, Inbound::List);
    }

    #[test]
    fn an_unknown_tag_is_an_error_not_a_panic() {
        let result: Result<Inbound, _> = serde_json::from_str(r#"{"t":"launch_missiles"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn outbound_dirty_serializes_to_one_line() {
        let json = serde_json::to_string(&Outbound::Dirty {
            pane: 7,
            seqno: 1182,
        })
        .expect("to serialize");
        assert_eq!(json, r#"{"t":"dirty","pane":7,"seqno":1182}"#);
        assert!(!json.contains('\n'), "NDJSON framing needs one line");
    }

    #[test]
    fn outbound_lines_round_trips() {
        let msg = Outbound::Lines {
            pane: 7,
            first_row: 3140,
            lines: vec!["a".to_string(), "b".to_string()],
            seqno: 1182,
        };
        let json = serde_json::to_string(&msg).expect("to serialize");
        let back: Outbound = serde_json::from_str(&json).expect("to parse");
        assert_eq!(back, msg);
    }

    #[test]
    fn a_line_containing_a_newline_stays_one_json_line() {
        // Terminal text can contain anything; NDJSON framing must survive it.
        let msg = Outbound::Lines {
            pane: 7,
            first_row: 0,
            lines: vec!["oops\nsecond".to_string()],
            seqno: 1,
        };
        let json = serde_json::to_string(&msg).expect("to serialize");
        assert!(
            !json.contains('\n'),
            "the newline must be escaped, not emitted raw: {json}"
        );
    }

    #[test]
    fn pane_added_stays_flat_on_the_wire() {
        // The client reads msg.pane and msg.title off the message itself. If
        // the newtype ever nests the PaneInfo under a key instead, every one
        // of these fields silently becomes undefined on the other side.
        let json = serde_json::to_string(&Outbound::PaneAdded(info(7))).expect("to serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("to parse back");
        assert_eq!(value["t"], "pane_added");
        assert_eq!(value["pane"], 7);
        assert_eq!(value["title"], "Claude Code");
        assert_eq!(value["session"], "casb");
        assert_eq!(value["subscribed"], true);
        assert!(!json.contains('\n'), "NDJSON framing needs one line");
    }

    #[test]
    fn pane_added_round_trips() {
        let msg = Outbound::PaneAdded(info(7));
        let json = serde_json::to_string(&msg).expect("to serialize");
        let back: Outbound = serde_json::from_str(&json).expect("to parse");
        assert_eq!(back, msg);
    }

    #[test]
    fn a_pane_outside_the_sidebar_has_no_session_name() {
        // A pane opened with ctrl-shift-t belongs to no saved session. The
        // absent name has to survive the round trip rather than being
        // invented on either side.
        let mut bare = info(7);
        bare.session = None;
        bare.cwd = None;
        let json = serde_json::to_string(&Outbound::PaneAdded(bare.clone())).expect("to serialize");
        let value: serde_json::Value = serde_json::from_str(&json).expect("to parse back");
        assert!(value["session"].is_null());
        assert_eq!(
            serde_json::from_str::<Outbound>(&json).expect("to parse"),
            Outbound::PaneAdded(bare)
        );
    }

    #[test]
    fn outbound_pane_title_serializes_to_one_line() {
        let json = serde_json::to_string(&Outbound::PaneTitle {
            pane: 7,
            title: "fixing the numbering".to_string(),
        })
        .expect("to serialize");
        assert_eq!(
            json,
            r#"{"t":"pane_title","pane":7,"title":"fixing the numbering"}"#
        );
    }

    #[test]
    fn outbound_pane_mirror_serializes_to_one_line() {
        let json = serde_json::to_string(&Outbound::PaneMirror {
            pane: 7,
            enabled: true,
        })
        .expect("to serialize");
        assert_eq!(json, r#"{"t":"pane_mirror","pane":7,"enabled":true}"#);
    }
}
