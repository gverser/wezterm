# `ToggleKeywordHighlight`

{{since('nightly')}}

Turns [keyword highlighting](../config/keyword_highlight_enabled.md) on or off
for the current window, without changing the configuration. The command is in
the command palette and the View menu; there is no default key assignment.

```lua
config.keys = {
  {
    key = 'h',
    mods = 'CTRL|SHIFT|ALT',
    action = wezterm.action.ToggleKeywordHighlight,
  },
}
```
