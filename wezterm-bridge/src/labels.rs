//! Turning a mux workspace name back into the name the user gave it.
//!
//! The sidebar launches a session into a workspace called `session:6`, which
//! is a fine key and a terrible label. The tree in `sessions.json` is the only
//! place that remembers it was called "casb", and the client has no way to
//! read that file itself -- it may not even be on the same machine.

use wezterm_sessions::store::SessionStore;
use wezterm_sessions::tree::NodeId;

/// The saved session whose workspace this is, if any.
///
/// Reloads the tree on each call rather than caching it. This runs when a pane
/// appears and when a client lists, never in the output path, and a cache here
/// would go stale the moment the user renamed a session in the sidebar.
pub fn session_for_workspace(workspace: &str) -> Option<String> {
    let store = SessionStore::load();
    name_for_workspace(&store.tree().sessions_with_names(), workspace)
}

/// The lookup itself, kept apart from loading the file so it can be tested.
fn name_for_workspace(sessions: &[(NodeId, String)], workspace: &str) -> Option<String> {
    sessions
        .iter()
        .find(|(id, _)| id.workspace_name() == workspace)
        .map(|(_, name)| name.clone())
}

#[cfg(test)]
mod test {
    use super::*;

    fn sessions() -> Vec<(NodeId, String)> {
        vec![
            (NodeId(2), "casb".to_string()),
            (NodeId(5), "wezterm".to_string()),
            (NodeId(7), "auto-forge-gear".to_string()),
        ]
    }

    #[test]
    fn a_workspace_resolves_to_its_session_name() {
        assert_eq!(
            name_for_workspace(&sessions(), "session:7"),
            Some("auto-forge-gear".to_string())
        );
    }

    #[test]
    fn the_default_workspace_belongs_to_no_session() {
        // Panes opened with ctrl-shift-t live here. They are real panes worth
        // offering, they just have no name of their own.
        assert_eq!(name_for_workspace(&sessions(), "default"), None);
    }

    #[test]
    fn a_workspace_whose_session_was_deleted_resolves_to_nothing() {
        // The workspace outlives the tree entry: closing the sidebar row does
        // not close the terminal that was started from it.
        assert_eq!(name_for_workspace(&sessions(), "session:99"), None);
    }

    #[test]
    fn the_node_id_is_not_matched_as_a_bare_number() {
        // "7" is a node id, not a workspace. Accepting it would let an
        // unrelated workspace called "7" borrow another session's name.
        assert_eq!(name_for_workspace(&sessions(), "7"), None);
    }
}
