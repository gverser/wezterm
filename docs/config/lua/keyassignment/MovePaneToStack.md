# `MovePaneToStack`

{{since('nightly')}}

Moves the current terminal into the stack of the adjacent split in the given
direction, placing it right after the terminal shown there and making it the
one that is shown. The direction is a
[PaneDirection](ActivatePaneDirection.md) such as `'Left'`.

If the terminal was the only one in its stack, the split it leaves is removed.

The window tab must have a tab strip, which needs
[enable_pane_tab_bar](../config/enable_pane_tab_bar.md) to have been on when it
was created. Without a strip this action does nothing beyond writing a line to
the log. It also does nothing while the pane is
[zoomed](TogglePaneZoomState.md), when there is no adjacent split that way, or
when the direction is `'Next'` or `'Prev'`, which are not spatial.

There is no default key assignment. The commands for the four directions are
available from the command palette, or you can bind them yourself:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action
local config = {}

config.enable_pane_tab_bar = true
config.keys = {
  {
    key = 'LeftArrow',
    mods = 'CTRL|SHIFT|ALT',
    action = act.MovePaneToStack 'Left',
  },
  {
    key = 'RightArrow',
    mods = 'CTRL|SHIFT|ALT',
    action = act.MovePaneToStack 'Right',
  },
}
return config
```

You can also drag a tab onto the strip of another split; see
[enable_pane_tab_bar](../config/enable_pane_tab_bar.md).

See also:

 * [enable_pane_tab_bar](../config/enable_pane_tab_bar.md)
 * [SpawnCommandInPaneStack](SpawnCommandInPaneStack.md)
 * [ActivatePaneStackRelative](ActivatePaneStackRelative.md)
 * [ActivatePaneDirection](ActivatePaneDirection.md)
