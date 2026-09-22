use clap::Parser;
use wezterm_client::client::Client;

#[derive(Debug, Parser, Clone)]
pub struct KillServer {}

impl KillServer {
    pub async fn run(&self, client: Client) -> anyhow::Result<()> {
        // The server answers and then exits, so the connection can go away
        // before, during or after the reply reaches us. Only a refusal from a
        // server that is still there is a real failure.
        match client.kill_server().await {
            Ok(_) => Ok(()),
            Err(err) if connection_ended(&err) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// Whether this error is the server going away, which is what success looks
/// like from this side.
///
/// Both strings below are the literal text a caller can actually observe;
/// verified by tracing every place these errors are produced:
///
/// - `"Client was destroyed"` — `Drop for Promises` in
///   wezterm-client/src/client.rs (~line 404) fails every promise still
///   waiting on a reply with this exact string via a plain `anyhow!(...)`
///   when the reader thread's connection loop exits for any reason. (This is
///   distinct from the `NotReconnectableError::ClientWasDestroyed` enum a few
///   lines above it in that file, which is only used to decide whether the
///   reader thread should try to reconnect and is never delivered to an
///   in-flight caller.)
/// - `"while decoding response pdu"` — the `.context("Error while decoding
///   response pdu")` at wezterm-client/src/client.rs:460, hit when the
///   connection closes (including a clean EOF) while a reply is being read.
///
/// `std::io::ErrorKind::UnexpectedEof`/`BrokenPipe` were checked here in an
/// earlier draft but were removed: everywhere those kinds occur in this
/// codebase (wezterm-client/src/client.rs ~line 1097, which only
/// `log::error!`s "server closed connection (...)" and never returns it to a
/// caller; wezterm-mux-server-impl/src/dispatch.rs:77,104,115, which are
/// server-side and only decide whether to exit quietly) the code matches on
/// `.kind()` and then formats an unrelated message — the literal tokens
/// "UnexpectedEof"/"BrokenPipe" never appear in any text a `kill-server`
/// caller can see, so matching them here was dead.
fn connection_ended(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    text.contains("Client was destroyed") || text.contains("while decoding response pdu")
}

/// Whether this error means there was no server to connect to in the first
/// place, as opposed to a server that is there but refused the kill.
///
/// `wezterm-client`'s `unix_connect` reports "no auto-start and nothing to
/// connect to" by formatting the underlying io error into a fresh `bail!`
/// string rather than chaining it as a typed source, so there is no
/// `std::io::Error`/`ErrorKind` to downcast to here either; text matching is
/// the only option, same as `connection_ended` above.
pub(crate) fn no_server_to_kill(err: &anyhow::Error) -> bool {
    format!("{err:#}").contains("failed to connect to")
}
