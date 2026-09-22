# `wezterm cli kill-server`

Stops the mux server.

Where this command connects depends on, in order of precedence: the
`WEZTERM_UNIX_SOCKET` environment variable (set inside every pane to point at
whatever spawned it), then `--prefer-mux`, then GUI discovery.

- From a pane hosted by the mux server, `WEZTERM_UNIX_SOCKET` already points
  at the mux server, so plain `wezterm cli kill-server` just works.
- From a pane inside a local GUI window, `WEZTERM_UNIX_SOCKET` points at the
  GUI instead, and `WEZTERM_UNIX_SOCKET` wins over `--prefer-mux`, so the GUI
  refuses the request even with the flag. Run the command outside wezterm, or
  `unset WEZTERM_UNIX_SOCKET` first.
- From an ordinary terminal with no `WEZTERM_UNIX_SOCKET` set, pass
  `--prefer-mux` if a GUI is also running, otherwise the GUI is discovered
  and refuses the request:

```console
wezterm cli --prefer-mux kill-server
```

The server exits as it answers, so the connection to it goes away around the
reply; that is treated as success, not an error. This subcommand also never
starts a server just to kill it: if no server is running, it succeeds
quietly and does nothing.

## Synopsis

```console
{% include "../../examples/cmd-synopsis-wezterm-cli-kill-server--help.txt" %}
```
