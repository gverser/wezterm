# `ActivatePaneStackRelative`

{{since('nightly')}}

Shows the terminal that many places along from the current one in the current
split's stack, wrapping around at either end. `1` is the next tab on the strip
and `-1` the previous one. A stack with only one terminal is left alone.

The window tab must have a tab strip, which needs
[enable_pane_tab_bar](../config/enable_pane_tab_bar.md) to have been on when it
was created. Without a strip this action does nothing beyond writing a line to
the log.

There is no default key assignment. The commands for `1` and `-1` are available
from the command palette, or you can bind them yourself:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = {}

config.enable_pane_tab_bar = true
config.keys = {
  {
    key = 'PageDown',
    mods = 'CTRL|ALT',
    action = act.ActivatePaneStackRelative(1),
  },
  {
    key = 'PageUp',
    mods = 'CTRL|ALT',
    action = act.ActivatePaneStackRelative(-1),
  },
}
return config
```

See also:

 * [enable_pane_tab_bar](../config/enable_pane_tab_bar.md)
 * [SpawnCommandInPaneStack](SpawnCommandInPaneStack.md)
 * [MovePaneToStack](MovePaneToStack.md)
