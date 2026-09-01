---
tags:
  - appearance
  - session_sidebar
---
# `session_sidebar_enabled = true`

{{since('nightly')}}

Draws a session sidebar down the left edge of the window, listing saved sessions
in a tree that you can organize into folders.

```lua
config.session_sidebar_enabled = false
```

Set it to `false` to reclaim the space; the sidebar can also be hidden for the
current window with
[ToggleSessionSidebar](../keyassignment/ToggleSessionSidebar.md).

Each entry in the tree is bound to its own
[workspace](../../../recipes/workspaces.md), so every session gets its own set
of tabs: clicking an entry switches the tab bar to that session's tabs, leaving
whatever was running under the previous one alive in the background.

| Action | Result |
|--------|--------|
| Click a session | Switch to its workspace, opening a terminal if it has none yet |
| Double click a session | Switch to it and open an additional tab |
| Click a folder | Expand or collapse it |
| Right click | New session, new folder, rename, edit, delete |
| Drag the right edge | Change the sidebar's width |

Renaming happens in place: choosing *Rename* turns the row into an input field
that takes the keyboard until you press <kbd>Enter</kbd> or <kbd>Esc</kbd>.
Renaming never disturbs terminals that are already running under the entry,
because the workspace is keyed on the entry's identity rather than its name.

The tree is stored as `sessions.json` in wezterm's data directory
(`~/.local/share/wezterm` on unix, `%LOCALAPPDATA%\wezterm` on Windows) and is
written out whenever you change it. If that file can't be parsed it is preserved
as `sessions.json.bad` and the sidebar starts empty rather than overwriting it.

Removing an entry from the tree does not terminate anything running under it;
the workspace survives and remains reachable from
[ShowLauncher](../keyassignment/ShowLauncher.md).

The sidebar takes its colors from the
[tab_bar](../../appearance.md#tab-bar-appearance-colors) color settings.

See also:

 * [session_sidebar_width](session_sidebar_width.md)
 * [ToggleSessionSidebar](../keyassignment/ToggleSessionSidebar.md)
 * [FocusSessionSidebarSearch](../keyassignment/FocusSessionSidebarSearch.md)
