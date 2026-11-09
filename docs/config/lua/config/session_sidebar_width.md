---
tags:
  - appearance
  - session_sidebar
---
# `session_sidebar_width = "18cell"`

{{since('nightly')}}

Controls how wide the session sidebar is when
[session_sidebar_enabled](session_sidebar_enabled.md) is set.

This is the width of the panel that shows the sessions or files page. The
column of page tabs down its left edge (2 cells wide) comes on top of it, and
is all that remains when the panel is collapsed.

The value is a [Dimension](../../../config/lua/config/index.md), so it can be
expressed in cells, pixels, points or as a percentage of the window width:

```lua
config.session_sidebar_width = '24cell'
config.session_sidebar_width = { Pixels = 260 }
config.session_sidebar_width = '20%'
```

The width is clamped to at most half the window width, and to at least 8 cells,
so that the terminal always keeps a usable amount of space. Dragging the
sidebar's right edge overrides this setting for the current window; the drag is
applied when you release the mouse button rather than continuously, so that
resizing doesn't churn every pty in the workspace.

See also:

 * [session_sidebar_enabled](session_sidebar_enabled.md)
