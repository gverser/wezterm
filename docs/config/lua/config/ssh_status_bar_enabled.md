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
uptime, logged in users and disk usage, refreshed every three seconds. A dot
at the start shows how the connection is: green while the figures are fresh,
yellow while connecting or when they are out of date, red when the connection
failed or dropped. CPU, memory and disk turn yellow above 80% and red above
90%; otherwise the figures are plain, their names greyed. When the window is
too narrow, the disks fold into one entry and then the less important entries
are left out; CPU and memory always stay.

The figures come from a background command on the same ssh connection as the
pane, reading `/proc`, `who` and `df`; nothing is installed or written on the
server. Servers without `/proc` show "仅支持 Linux 服务器" instead.

Before there are figures the bar says where things stand: 正在连接… until
the login is done, then 正在获取服务器状态…; 连接失败 when the connection
could not be made, and 连接已断开 once it has dropped. When the background
command has sent nothing for five seconds, a stalled connection say, the
figures are greyed out and followed by 数据已过期（N 秒前）; after fifteen
seconds the command is given up on and started again half a minute later.
Only windows showing the connection are redrawn, and only when something on
the bar has changed.

The bar is only present while such a pane is active, so local panes keep
their full height.

```lua
config.ssh_status_bar_enabled = false
```
