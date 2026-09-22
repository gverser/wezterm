---
tags:
  - multiplexing
  - spawn
---
# `mux_env_remove`

{{since('20211204-082213-a66c61ee9')}}

Specifies a list of environment variables that should be removed
from the environment in the multiplexer server.

The intent is to clean up environment variables that might give the wrong
impression of their operating environment to the various terminal sessions
spawned by the multiplexer server.

This matters more than it looks like it should, because the multiplexer server
is a daemon: it keeps whatever environment it was started with for as long as
it runs, and hands that environment to every pane it builds a command line for.
A variable that described the shell you launched it from is still describing
that shell days later, to a pane that has nothing to do with it.

An entry ending in `*` matches by prefix, so that a tool which names its
variables `FOO_ONE`, `FOO_TWO` and adds more with each release can be covered
without maintaining a list by hand. Any other entry is a plain variable name.

Note that setting this option *replaces* the default list rather than adding to
it, so include the entries you still want.

The default value for this is:

```lua
config.mux_env_remove = {
  'SSH_AUTH_SOCK',
  'SSH_CLIENT',
  'SSH_CONNECTION',
  -- colour preferences belong to the shell that started the server, not to
  -- the panes it will still be creating days later
  'NO_COLOR',
  'FORCE_COLOR',
  'CLICOLOR',
  'CLICOLOR_FORCE',
  -- a coding agent's own markers for the session it launched the server from
  'CLAUDECODE',
  'CLAUDE_CODE_*',
  'CLAUDE_PID',
}
```
