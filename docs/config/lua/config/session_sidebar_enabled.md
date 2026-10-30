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
| Click a session | Select it, and switch to it if it is already running |
| Double click a session | Open it: start it if it isn't running, otherwise add a tab |
| Click a folder | Expand or collapse it |
| Right click | New session, new folder, rename, edit, delete |
| Drag an entry | Move it, and a folder with everything in it: a line marks the gap it will go into, a box the folder it will go inside. Below the last entry is the end of the top level. Not while searching |
| Drag the right edge | Change the sidebar's width |
| Click the chevron | Collapse the sidebar to a rail, and back |

A single click never starts anything. With a tree of any size, selecting an
entry to look at it would otherwise leave a shell running behind every one you
touched.

Entries that are running show how many terminals they hold, on the right of the
row. Sessions stay alive in the background once opened, so this is the only
indication of what is still running behind the tree.

Renaming happens in place: choosing *Rename* turns the row into an input field
that takes the keyboard until you press <kbd>Enter</kbd> or <kbd>Esc</kbd>.
Renaming never disturbs terminals that are already running under the entry,
because the workspace is keyed on the entry's identity rather than its name.

The tree is stored as `sessions.json` in wezterm's data directory
(`~/.local/share/wezterm` on unix, `%APPDATA%\wezterm` on Windows) and is
written out whenever you change it. If that file can't be parsed it is preserved
as `sessions.json.bad` and the sidebar starts empty rather than overwriting it.

Removing an entry from the tree does not terminate anything running under it;
the workspace survives and remains reachable from
[ShowLauncher](../keyassignment/ShowLauncher.md).

Closing the window asks first when sessions are running out of sight, naming
them and their terminal counts, and closes them all on confirmation. Without
that, closing the window would kill only the session on screen and leave the
others running with nothing to reach them.

The sidebar takes its colors from the
[tab_bar](../../appearance.md#tab-bar-appearance-colors) color settings.

See also:

 * [session_sidebar_width](session_sidebar_width.md)
 * [ToggleSessionSidebar](../keyassignment/ToggleSessionSidebar.md)
 * [FocusSessionSidebarSearch](../keyassignment/FocusSessionSidebarSearch.md)
