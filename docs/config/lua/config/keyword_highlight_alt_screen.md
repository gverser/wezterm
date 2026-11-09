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
