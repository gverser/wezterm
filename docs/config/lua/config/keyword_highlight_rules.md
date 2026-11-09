---
tags:
  - appearance
  - keyword_highlight
---
# `keyword_highlight_rules`

{{since('nightly')}}

The rules used by [keyword highlighting](keyword_highlight_enabled.md). Each
rule has a `regex` in [Rust regex syntax](https://docs.rs/regex/latest/regex/#syntax),
a `color` and an optional `bold`. Where two rules match the same text, the one
listed first wins. Setting this option replaces the defaults entirely.

A colour can be a palette entry, which follows your colour scheme, or a fixed
colour:

```lua
config.keyword_highlight_rules = {
  -- (?i) makes the match case insensitive; \b keeps it to whole words
  { regex = [[(?i)\b(?:error|failed|denied)\b]], color = { AnsiColor = 'Red' } },
  { regex = [[(?i)\bwarn(?:ing)?\b]], color = { AnsiColor = 'Yellow' } },
  { regex = [[(?i)\b(?:ok|done)\b]], color = { AnsiColor = 'Lime' }, bold = true },
  { regex = [[\bTODO\b]], color = { Color = '#ff8800' } },
}
```

The defaults colour these, in this order:

| Colour | Matches |
|---|---|
| `Red` | error(s), fail, fails, failed, failure, fatal, critical, denied, refused, panic, panicked, exception(s), abort, aborted, unreachable |
| `Yellow` | warn, warns, warning(s), deprecated |
| `Lime` | ok, success, successful, successfully, succeeded, done, passed, complete, completed |
| `Aqua` | IPv4 addresses, optionally followed by `:port` |

All of the word rules are case insensitive and match whole words only.

A rule whose regex doesn't compile is skipped and reported in the log.
