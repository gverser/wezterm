use anyhow::Result;
use clap::Parser;
use codec::SetPaneFeishu;
use mux::pane::PaneId;
use wezterm_client::client::Client;

/// Turn Feishu mirroring on or off for a pane.
///
/// The GUI has a tab bar button for this, but having it on the command line
/// makes the feature scriptable -- and testable without a human at the mouse.
#[derive(Debug, Parser, Clone)]
pub struct SetPaneFeishuCommand {
    /// Specify the target pane.
    /// The default is to use the current pane based on the
    /// environment variable WEZTERM_PANE.
    #[arg(long)]
    pane_id: Option<PaneId>,

    /// Stop mirroring rather than start.
    #[arg(long)]
    off: bool,
}

impl SetPaneFeishuCommand {
    pub async fn run(&self, client: Client) -> Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;
        client
            .set_pane_feishu(SetPaneFeishu {
                pane_id,
                enabled: !self.off,
            })
            .await?;
        Ok(())
    }
}
