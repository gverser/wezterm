---
tags:
  - exit_behavior
  - multiplexing
---
# `mux_server_close_confirmation = "AlwaysPrompt"`

Whether to offer to stop the mux server when closing the last GUI window
attached to it.

The prompt only appears when closing the last window of a mux session: a
window connected to a `ClientDomain` (unix domain, ssh, or tls) that is the
only GUI window remaining for that connection. Closing that window has
always detached and left the session's server process running in the
background; this option additionally asks whether to stop that server too,
ending every session it hosts.

```lua
config.mux_server_close_confirmation = 'AlwaysPrompt'
```

Set this to `"NeverPrompt"` to keep the old, silent behaviour: the window
closes and detaches, and the server is left running, with no prompt at all.

```lua
config.mux_server_close_confirmation = 'NeverPrompt'
```

See also [window_close_confirmation](window_close_confirmation.md), which
governs the separate prompt for ending the sessions local to this window.
