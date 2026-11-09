---
tags:
  - appearance
  - session_sidebar
---
# `session_sidebar_enabled = true`

{{since('nightly')}}

Draws a session sidebar down the left edge of the window, listing saved sessions
in a tree that you can organize into folders. A column of page tabs runs down
its left edge: **会话** (sessions) shows the tree, **文件** (files) browses the
server of the active ssh pane; see [The file page](#the-file-page) below.

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
| Right click | New session, new SSH session, new folder, rename, edit, delete |
| Drag an entry | Move it, and a folder with everything in it: a line marks the gap it will go into, a box the folder it will go inside. Below the last entry is the end of the top level. Not while searching |
| Drag the right edge | Change the sidebar's width |
| Click the chevron | Collapse the sidebar to its tab column, and back |
| Click a page tab | Switch to that page; on the page already showing, collapse; while collapsed, open on that page |

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

### SSH sessions

*New SSH Session* in the right click menu opens a form for the host, port, user
name and either a password or a private key file; *Edit...* on an ssh entry
opens the same form, and *Edit...* on any other entry a form for its domain,
directory and command. Fields can be filled in any order; <kbd>Tab</kbd> moves
between them, <kbd>Enter</kbd> saves and <kbd>Esc</kbd> cancels. ssh entries
show `⇄` in front of their name.

Double clicking an ssh entry connects to it. All the tabs opened from one entry
share a single connection. It stays open after the last of them closes, so
that opening the entry again is quick, and goes when wezterm exits.

The first time a password is typed at the prompt and accepted, the terminal
asks whether to save it; once saved, later connections log in without asking.
If the server rejects a saved password, the terminal says so, and the next
connection asks for it by hand and then offers to update the saved one.

The connection details are kept in `ssh_hosts.json` next to `sessions.json`,
**with passwords in plain text** so that the file can be copied to another
machine as it is. Keep it private. Deleting an entry deletes its details too.

Tabs opened from an ssh entry also get the
[ssh status bar](ssh_status_bar_enabled.md).

### The file page

The **文件** page browses the server that the active pane is connected to, over
sftp on the same connection; with a local pane active it just says so. Each
connection remembers where it was, so switching between panes on different
servers switches the page too.

Along the top are the current path (click it to type another one; relative
paths are taken from the current directory and `~` from the home directory),
buttons for up, refresh, home, new folder and upload, and a **跟随终端**
(follow the terminal) checkbox, on by default. While it is ticked, the page goes
to the directory of the active pane's shell within a second or two of a `cd`.
Browsing elsewhere by hand is left alone until the shell changes directory
again. The directory is read from `/proc` on the server, which needs nothing
installed there; on servers without it, following does nothing.

| Action | Result |
|--------|--------|
| Double click a folder | Go into it |
| Double click a file | Download it |
| Right click | Download, rename, delete, copy path, new folder, refresh |
| Drop files from Explorer onto the page | Upload them to the directory shown |

Only files and empty folders can be deleted, after a confirmation; a folder
with anything in it is refused with 目录不为空. Folders can't be uploaded.

Downloads ask where to save, starting in the Downloads folder. Uploads that
would replace a file on the server ask first. Transfers run one at a time in
the background, with a line at the bottom of the page showing progress and
speed and a ✕ to cancel; a cancelled or failed transfer doesn't leave a partial
file behind. (An upload that was replacing a file removes it entirely in that
case: the old copy is already gone once the upload starts.) Failures show the
server's own message.

The sidebar takes its colors from the
[tab_bar](../../appearance.md#tab-bar-appearance-colors) color settings.

See also:

 * [session_sidebar_width](session_sidebar_width.md)
 * [ssh_status_bar_enabled](ssh_status_bar_enabled.md)
 * [ToggleSessionSidebar](../keyassignment/ToggleSessionSidebar.md)
 * [FocusSessionSidebarSearch](../keyassignment/FocusSessionSidebarSearch.md)
