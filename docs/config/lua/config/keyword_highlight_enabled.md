---
tags:
  - appearance
  - keyword_highlight
---
# `keyword_highlight_enabled = true`

{{since('nightly')}}

Colours keywords in terminal output the way MobaXterm's syntax highlighting
does: words like `error`, `failed` and `denied` in red, `warning` in yellow,
`ok` and `success` in green, and IPv4 addresses in cyan.

Only text that the program left in the default foreground colour is coloured;
anything a program coloured itself (`ls --color`, a shell prompt, vim) is left
alone. The highlighting is applied when drawing, so copying, searching and
quick select still see the original text.

```lua
config.keyword_highlight_enabled = false
```

The rules are set by
[keyword_highlight_rules](keyword_highlight_rules.md); full screen programs are
covered by [keyword_highlight_alt_screen](keyword_highlight_alt_screen.md).
[ToggleKeywordHighlight](../keyassignment/ToggleKeywordHighlight.md) turns it
on or off for the current window.
