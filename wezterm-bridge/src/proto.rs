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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: PaneId,
    pub workspace: String,
    pub title: String,
    pub cwd: Option<String>,
}

/// Bridge -> client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Outbound {
    AuthOk,
    PaneAdded {
        pane: PaneId,
        workspace: String,
        title: String,
        cwd: Option<String>,
    },
    PaneRemoved {
        pane: PaneId,
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
}
