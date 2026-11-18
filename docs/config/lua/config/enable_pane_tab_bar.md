---
tags:
  - appearance
  - pane
---
# `enable_pane_tab_bar = false`

{{since('nightly')}}

Gives every split of a window tab a one-row tab strip along its top edge, so
that a single split can hold more than one terminal. Only the terminal whose
tab is selected is shown; the others keep running, stacked behind it.

```lua
config.enable_pane_tab_bar = true
```

The setting is read when a window tab is created. Window tabs that already
exist keep what they had, and changing the configuration does not add or remove
the strips on them. There are two exceptions:

 * A window tab made by taking a pane out of another one, such as with
   [wezterm cli move-pane-to-new-tab](../../../cli/cli/move-pane-to-new-tab.md)
   or the `MoveToNewTab` and `MoveToNewWindow` modes of
   [PaneSelect](../keyassignment/PaneSelect.md), takes the setting of the
   window tab it came from, whatever the option says now.
 * A tmux window tab never has a tab strip.

When you connect to a mux server, the server's configuration decides. The side
that owns the window tab decides whether the strip takes a row, because both
sides must agree on how tall each pane is.

| Action | Result |
|--------|--------|
| Click a tab | Show that terminal and focus it |
| Click the `×` on a tab, or middle click the tab | Close that terminal, asking first just as [CloseCurrentPane](../keyassignment/CloseCurrentPane.md) does |
| Drag a tab along its strip | Reorder the stack. A line marks the gap the tab will go into, and the terminal moves when you release the button |
| Drag a tab onto the strip of another split | Move that terminal into the other split's stack, in the gap the line marks, when you release the button. Releasing anywhere that is not a strip cancels the drag |
| Double click empty space on a strip | Start a new terminal in that split's stack |

After a tab is closed or a new terminal is started, every press on a strip
within the double click interval is ignored, so that a double click on a `×`
closes one terminal rather than two. After a click on a tab, only a close that
follows straight away is ignored: the click can re-lay out the strip (the
unseen-output marker disappears) and slide another `×` under the pointer.
Clicking another tab, or empty space, still works. Nothing is moved while the
button is held down, and a drag only works within the window tab it started in.

Moving the last terminal out of a split removes that split.

If the pane is [zoomed](../keyassignment/TogglePaneZoomState.md), the strip
stays: the split that is zoomed is shown with its strip along the top, and
switching tabs within it keeps the zoom.

The same operations are available from the keyboard, and from the command
palette once this option is on. None of them has a default key assignment:

 * [SpawnCommandInPaneStack](../keyassignment/SpawnCommandInPaneStack.md)
 * [ActivatePaneStackRelative](../keyassignment/ActivatePaneStackRelative.md)
 * [MovePaneToStack](../keyassignment/MovePaneToStack.md)

The strip takes its colors from the
[tab_bar](../../appearance.md#tab-bar-appearance-colors) color settings.

See also:

 * [SpawnCommandInPaneStack](../keyassignment/SpawnCommandInPaneStack.md)
 * [ActivatePaneStackRelative](../keyassignment/ActivatePaneStackRelative.md)
 * [MovePaneToStack](../keyassignment/MovePaneToStack.md)
