# `SpawnCommandInPaneStack`

{{since('nightly')}}

Spawns a new terminal as a member of the current split's stack, next to the
terminal that is showing now, and makes it the one that is shown. It takes a
[SpawnCommand](../SpawnCommand.md) like
[SpawnCommandInNewTab](SpawnCommandInNewTab.md) does.

The window tab must have a tab strip, which needs
[enable_pane_tab_bar](../config/enable_pane_tab_bar.md) to have been on when it
was created. Without a strip this action does nothing beyond writing a line to
the log.

The new terminal can only be started in the domain of the current pane, since
it shares the pane's split. A `domain` in the `SpawnCommand` that names another
domain makes the action fail with an error in the log and spawns nothing.

There is no default key assignment. The command is available from the command
palette, or you can bind it yourself:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = {}

config.enable_pane_tab_bar = true
config.keys = {
  {
    key = 't',
    mods = 'CTRL|SHIFT|ALT',
    action = act.SpawnCommandInPaneStack {},
  },
}
return config
```

See also:

 * [enable_pane_tab_bar](../config/enable_pane_tab_bar.md)
 * [ActivatePaneStackRelative](ActivatePaneStackRelative.md)
 * [MovePaneToStack](MovePaneToStack.md)
