-- Portable config. On Windows WezTerm looks next to the executable first and
-- returns the first config file it finds, so this file belongs to this install
-- alone.
--
-- Its presence also makes this a portable install: the session tree, the
-- bridge tokens and the other files wezterm writes for itself go into the
-- `data` directory beside it, created on first start, rather than into
-- %APPDATA%\wezterm.
--
-- wezterm-gui.exe and wezterm-mux-server.exe both live in this directory and so
-- read this same file. That is what makes bridge_enabled mean the same thing on
-- both sides of the connection.

local wezterm = require 'wezterm'
local config = wezterm.config_builder()

-- Panes live in the mux server rather than inside the GUI process. This is the
-- foundation of the whole thing: closing a window leaves its panes running, and
-- the bridge -- which runs inside the mux server -- can only see panes that
-- live there.
config.unix_domains = {
  {
    name = 'main',

    -- Deliberately NOT connect_automatically. That path attaches with no window
    -- of its own (dom.attach(None)), so the "Connecting..." UI opens a window
    -- just for itself, and that window is destroyed the moment the connection
    -- finishes -- leaving the GUI showing a dead frame that still says
    -- Connecting. Letting default_domain drive the attach instead means it
    -- happens inside the window we actually keep.

    -- The mux server next to this file, wherever the install has been put.
    -- config_dir rather than executable_dir: a development build started from
    -- one of this install's panes inherits WEZTERM_CONFIG_FILE, and has to talk
    -- to this install's server instead of starting one of its own.
    serve_command = {
      wezterm.config_dir .. '/wezterm-mux-server.exe',
      '--daemonize',
    },
  },
}

-- Makes 'main' what SpawnTabDomain::DefaultDomain resolves to, so sessions
-- created in the sidebar open inside the mux server.
config.default_domain = 'main'

-- Start with an empty window showing only the session sidebar, rather than a
-- shell nobody asked for. Only affects a cold start: if panes are already
-- running in the mux server, the GUI attaches to those instead.
config.spawn_initial_tab = false

config.bridge_enabled = true
config.bridge_port = 8788

-- How to start the Feishu service. It is launched when the first pane asks to
-- be mirrored and stopped when the last one stops, so nothing runs while
-- nobody is watching. Left unset, asking for Feishu only logs a warning.
-- Absolute paths on purpose: the mux server's working directory is not
-- something to rely on.
-- config.feishu_command = { 'C:/path/to/node.exe', 'C:/path/to/remote-controll/src/main.js' }

return config
