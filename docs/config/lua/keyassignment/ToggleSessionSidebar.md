# `ToggleSessionSidebar`

{{since('nightly')}}

Shows or hides the session sidebar. The sidebar must be turned on with
[session_sidebar_enabled](../config/session_sidebar_enabled.md) first; this
action collapses and restores it without changing that setting.

There is no default key assignment, so that enabling the sidebar doesn't take a
chord away from your existing bindings. The command is available from the
command palette and the View menu, or you can bind it yourself:

```lua
config.keys = {
  {
    key = 'b',
    mods = 'CTRL|SHIFT',
    action = wezterm.action.ToggleSessionSidebar,
  },
}
```

Collapsing the sidebar returns the full window width to the terminal; the tab
bar and panes reflow to match.

See also:

 * [session_sidebar_enabled](../config/session_sidebar_enabled.md)
 * [FocusSessionSidebarSearch](FocusSessionSidebarSearch.md)
