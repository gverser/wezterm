wezterm uses an embedded ssh library to provide an integrated SSH client.  The
client can be used to make ad-hoc SSH connections to remote hosts
by invoking the client like this:

```console
$ wezterm ssh wez@my.server
```

(checkout `wezterm ssh -h` for more options).

When invoked in this way, wezterm may prompt you for SSH authentication
and once a connection is established, open a new terminal window with
your requested command, or your shell if you didn't specify one.

Creating new tabs or panes will each create a new channel in your existing
session so you won't need to re-authenticate for additional tabs that you
create.

SSH sessions created in this way are non-persistent and all associated
tabs will die if your network connection is interrupted.

Take a look at [the multiplexing section](multiplexing.md) for an
alternative configuration that connects to a remote wezterm instance
and preserves your tabs.

The [ssh_backend](config/lua/config/ssh_backend.md) configuration can
be used to specify which ssh library is used.

{{since('20210404-112810-b63a949d')}}

wezterm is now able to parse `~/.ssh/config` and `/etc/ssh/ssh_config`
and respects the following options:

* `IdentityAgent`
* `IdentityFile`
* `Hostname`
* `User`
* `Port`
* `ProxyCommand`
* `Host` (including wildcard matching)
* `UserKnownHostsFile`
* `IdentitiesOnly`
* `BindAddress`

All other options are parsed but have no effect.  Notably, neither `Match` or
`Include` will do anything.

{{since('20210502-154244-3f7122cb:')}}

`Match` is now recognized but currently supports only single-phase (`final`,
`canonical` are not supported) configuration parsing for `Host` and
`LocalUser`.  `Exec` based matches are recognized but not supported.

{{since('20210814-124438-54e29167:')}}

`Include` is now supported.

{{since('nightly')}}

`ProxyUseFDpass` is now supported. (But not on Microsoft Windows).

`ServerAliveInterval` is now supported by the `libssh` backend.  Setting it to
a non-zero value will cause wezterm to send a keepalive on that interval.

`ServerAliveCountMax` is now supported by the `libssh` backend too.  Each
keepalive asks the server for a reply, and once more than
`ServerAliveCountMax` (3 by default) keepalives in a row have had nothing at
all come back, the session is ended, as OpenSSH does.  A request (opening a
tab, listing a directory) is given as long as those keepalives would take,
`ServerAliveInterval` times one more than `ServerAliveCountMax` (at most 30
minutes): if nothing at all has come back by then the session is ended too,
and otherwise just that request fails.  Without `ServerAliveInterval`, requests
wait for their reply as long as it takes, as before.

Counting what comes back needs the system to say how much has arrived on the
socket, which it does for a TCP socket on Windows and on Linux with glibc,
including one handed over by `ProxyUseFDpass`.  Elsewhere (macOS, musl, a
plain `ProxyCommand` on Linux) the keepalives are still sent and requests
still time out, but nothing counts as unanswered, so the session isn't ended
for it.  The `ssh2` backend sends its keepalives without asking for a reply and
doesn't count them: libssh2 would keep every reply for the life of the
session.  Its requests time out the same way (or after libssh2's own 60
seconds for most replies, if that is sooner), and one that does ends the
session whatever came back meanwhile, since libssh2 would carry on with the
abandoned request at the next call.

### CLI Overrides

`wezterm ssh` CLI allows overriding config settings via the command line.  This
example shows how to specify the private key to use when connecting to
`some-host`:

```bash
wezterm ssh -oIdentityFile=/secret/id_ed25519 some-host
```

