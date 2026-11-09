---
tags:
  - appearance
  - session_sidebar
---
# `ssh_status_bar_enabled = true`

{{since('nightly')}}

When the active pane was opened from an ssh session in the
[session sidebar](session_sidebar_enabled.md), a one row status bar under the
terminal shows the server's host name, CPU use, memory, network throughput,
uptime, logged in users and disk usage, refreshed every three seconds. CPU,
memory and disk turn yellow above 80% and red above 90%. When the window is
too narrow, the disks fold into one entry and then the less important entries
are left out; CPU and memory always stay.

The figures come from a background command on the same ssh connection as the
pane, reading `/proc`, `who` and `df`; nothing is installed or written on the
server. Servers without `/proc` show "仅支持 Linux 服务器" instead.

The bar is only present while such a pane is active, so local panes keep
their full height.

```lua
config.ssh_status_bar_enabled = false
```
