---
tags:
  - appearance
  - keyword_highlight
---
# `keyword_highlight_alt_screen = true`

{{since('nightly')}}

Whether [keyword highlighting](keyword_highlight_enabled.md) also applies to
full screen programs such as `less`, `vim` and `top`, which draw on the
terminal's alternate screen. Reading a log in `less` is where highlighting
helps most, so this is on by default.

```lua
config.keyword_highlight_alt_screen = false
```

Panes of a multiplexer domain (a `unix_domains` or `tls_clients` connection,
including a `default_domain` that names one) can't tell the GUI when a program
switches to the alternate screen, so for them this setting has no effect and
full screen programs are highlighted as long as
[keyword_highlight_enabled](keyword_highlight_enabled.md) is on. Use
[ToggleKeywordHighlight](../keyassignment/ToggleKeywordHighlight.md) to turn
highlighting off in the window while one is running there.
