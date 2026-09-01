# `FocusSessionSidebarSearch`

{{since('nightly')}}

Puts the keyboard focus in the session sidebar's search box, expanding the
sidebar first if it is collapsed. Typing filters the tree to matching entries,
reaching into collapsed folders as it goes.

```lua
config.keys = {
  {
    key = 'f',
    mods = 'CTRL|SHIFT|ALT',
    action = wezterm.action.FocusSessionSidebarSearch,
  },
}
```

There is no default key assignment; the command is available from the command
palette and the View menu.

### Key Assignments

| Action | Key Assignment |
|--------|----------------|
|Open the first match| <kbd>Enter</kbd> |
|Clear the filter and return focus to the terminal| <kbd>Esc</kbd> |
|Clear the filter| <kbd>CTRL</kbd> + <kbd>u</kbd> |

While the search box has focus the terminal does not receive keystrokes; the
search box shows a cursor to make that visible. <kbd>Esc</kbd> hands the
keyboard back.

See also:

 * [session_sidebar_enabled](../config/session_sidebar_enabled.md)
 * [ToggleSessionSidebar](ToggleSessionSidebar.md)
