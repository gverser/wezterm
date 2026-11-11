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
| Right click | 新建会话, 新建 SSH 会话, 新建文件夹, 重命名, 编辑…, 删除 |
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

Renaming happens in place: choosing *重命名* turns the row into an input field
that takes the keyboard until you press <kbd>Enter</kbd> or <kbd>Esc</kbd>.
Renaming never disturbs terminals that are already running under the entry,
because the workspace is keyed on the entry's identity rather than its name.

The tree is stored as `sessions.json` in wezterm's data directory
(`~/.local/share/wezterm` on unix, `%APPDATA%\wezterm` on Windows, and for a
portable install -- a `wezterm.lua` beside `wezterm.exe` -- the `data` folder
beside them) and is written out whenever you change it. If that file can't be parsed it is preserved
as `sessions.json.bad` and the sidebar starts empty rather than overwriting it.

Removing an entry from the tree does not terminate anything running under it;
the workspace survives and remains reachable from
[ShowLauncher](../keyassignment/ShowLauncher.md).

Closing the window asks first when sessions are running out of sight, naming
them and their terminal counts, and closes them all on confirmation. Without
that, closing the window would kill only the session on screen and leave the
others running with nothing to reach them.

### SSH sessions

*新建 SSH 会话* in the right click menu opens a form for the host, port, user
name and either a password or a private key file; *编辑…* on an ssh entry
opens the same form, and *编辑…* on any other entry a form for its domain,
directory and command. Fields can be filled in any order; <kbd>Tab</kbd> moves
between them, <kbd>Enter</kbd> saves and <kbd>Esc</kbd> cancels. A blank port
means the one `~/.ssh/config` gives the host, or 22. ssh entries show `⇄` in
front of their name.

Double clicking an ssh entry connects to it. All the tabs opened from one entry
share a single connection. It stays open after the last of them closes, so
that opening the entry again is quick, and goes when wezterm exits.

While connecting, the tab says **正在连接 user@host:port …**. A host that
doesn't answer within 10 seconds, a refused connection or a failed login ends
with **连接失败：** and the reason, and the tab stays open with
**⚠️ SSH 连接失败，再次打开这个会话会重试。** at the bottom.

The connection sends a keepalive every 30 seconds, so one that has dropped
(wifi, VPN, sleep, the server going away) is noticed even while nobody is
typing. Its tabs then stay open with
**⚠️ SSH 连接已断开，再次打开这个会话会重新连接。** at the bottom instead of
disappearing; double clicking the entry again connects afresh, whether or not
those tabs have been closed yet. A tab whose shell exits normally closes as
before.

`ServerAliveInterval` and `ConnectTimeout` in `~/.ssh/config` take precedence
over the 30 and 10 seconds for the hosts they are set for.

The first time a password is typed at the prompt and accepted, the terminal
asks whether to save it; once saved, later connections log in without asking.
If the server rejects a saved password, the terminal says so, and the next
connection asks for it by hand and then offers to update the saved one.

The connection details are kept in `ssh_hosts.json` next to `sessions.json`
(so in a portable install, in its `data` folder),
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
| Drop files from Explorer onto the page | Upload them to the directory shown; dropped before the page has read its first directory, they are not uploaded and the bottom line says so |

Only files and empty folders can be deleted, after a confirmation; a folder
with anything in it is refused with 目录不为空. Renaming onto a name that is
already taken is refused. Folders can't be uploaded.

Names that aren't UTF-8 are shown as GBK (bytes that aren't GBK either show as
�), and can be opened, downloaded, renamed and deleted like any other;
renaming one stores the new name in UTF-8. This needs the default libssh
[ssh_backend](ssh_backend.md); with ssh2 such names are left out of the list.
What is typed into the path bar is UTF-8, so a GBK directory is reached by
double clicking it or by following the terminal there.

Downloads ask where to save, starting in the Downloads folder. Uploads that
would replace a file on the server ask first; that question ignores
<kbd>Enter</kbd> for its first second, since it appears by itself and may
catch typing meant for the terminal. Transfers run one at a time in the
background, with a line at the bottom of the page showing progress and speed
and a ✕ to cancel.

Each transfer writes to a partial file first — `name.part` for a download,
`name.wezterm-part` beside the file on the server for an upload — and puts it
in place only once complete, so a cancelled or failed transfer leaves nothing
half-written and an existing file as it was. A file an upload replaces keeps
its permissions, and its owner where the server allows that; uploading to a
symlink replaces the file it points at. Closing the window while a transfer
is running asks first, then cancels it and cleans up the same way; if wezterm
itself exits in the middle of a transfer, the partial file stays behind.
Failures show the server's own message.

The sidebar takes its colors from the
[tab_bar](../../appearance.md#tab-bar-appearance-colors) color settings.

See also:

 * [session_sidebar_width](session_sidebar_width.md)
 * [ssh_status_bar_enabled](ssh_status_bar_enabled.md)
 * [ToggleSessionSidebar](../keyassignment/ToggleSessionSidebar.md)
 * [FocusSessionSidebarSearch](../keyassignment/FocusSessionSidebarSearch.md)
