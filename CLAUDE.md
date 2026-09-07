# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & test

MSRV is 1.71 (`ci/check-rust-version.sh`). Submodules must be initialized (`deps/freetype/*`, `deps/harfbuzz/*`) or the build fails with confusing zlib errors.

```bash
cargo check -p wezterm-gui        # fastest iteration loop; type-check only
cargo build -p wezterm-gui        # debug build
cargo run --release --bin wezterm-gui
cargo build --release             # full workspace
```

`make` targets wrap the canonical invocations: `make check`, `make build`, `make test`, `make fmt`, `make docs`.

Note that `check`/`build` in the Makefile deliberately enumerate crates rather than using `--all`: `wezterm-escape-parser` is `no_std` by default and only its standalone `cargo check -p wezterm-escape-parser` catches `std` leakage.

### Tests

CI and the Makefile use `cargo nextest run --all --no-fail-fast`. Plain cargo works too:

```bash
cargo test --all
cargo test -p wezterm-term                     # one crate
cargo test -p wezterm-term -- --nocapture name_of_test   # one test
cargo nextest run -p wezterm-escape-parser     # must pass separately; no_std by default
```

Terminal-behavior tests live in `term/src/test/` and use helpers there to assert on rendered screen contents — follow the existing patterns rather than driving `Terminal` by hand.

### Formatting

`cargo +nightly fmt --all` — nightly is required because `.rustfmt.toml` uses `imports_granularity`, which is unstable. CI (`.github/workflows/fmt.yml`) runs `--check` and will fail the build on unformatted code.

### Windows build environment

This is the platform-specific trap on Windows, and it is not optional:

- **Build from PowerShell, not Git Bash.** `.cargo/config.toml` forces `-C target-feature=+crt-static` for `x86_64-pc-windows-msvc`, which makes `openssl-src` build OpenSSL from source, which needs Strawberry Perl (`C:\Strawberry\perl\bin\perl.exe`). Git Bash puts msys perl (`/usr/bin/perl`) first on `PATH`; OpenSSL's `./Configure` then dies with `Can't locate Locale/Maketext/Simple.pm`.
- Changing shells changes `PATH`, which invalidates the `openssl-sys` build script and triggers a full OpenSSL + dependency rebuild (~25 min). Stay in one shell.
- MSVC toolchain only. `LNK4099: PDB 'ossl_static.pdb' not found` warnings during linking are expected and harmless (vendored OpenSSL ships no debug symbols).

## Architecture

wezterm is a layered stack; a change usually belongs to exactly one layer. Note that **directory names often differ from crate names**: `term/` = `wezterm-term`, `pty/` = `portable-pty`, `window/` = `window`.

Bottom to top:

1. **`wezterm-escape-parser/`, `vtparse/`** — escape/control sequence parsing. `no_std`-capable, dependency-light.
2. **`termwiz/`** — standalone cross-platform terminal toolkit (also published for external users; has its own CI workflow). Includes line editing, widgets, and the `Surface`/`Change` rendering model.
3. **`wezterm-cell/`, `wezterm-surface/`** — the cell/line/attribute data model.
4. **`term/` (`wezterm-term`)** — the terminal emulation state machine. Completely windowing-system agnostic. **Escape sequence behavior belongs here**, and the target for compatibility is xterm (see <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html>).
5. **`mux/`** — the session model shared by the GUI and the server. Three key abstractions:
   - `Domain` (`mux/src/domain.rs`) — where panes come from: local pty, ssh, tls/mux client, WSL, `ExecDomain`. Async trait, `Downcast`.
   - `Pane` (`mux/src/pane.rs`) — a scrollback + cursor + renderable surface. `LocalPane` wraps a `wezterm-term` `Terminal`; remote panes are backed by the client.
   - `Mux` — process-wide singleton (`Mux::get()`) owning windows → tabs → panes.
6. **`window/`** — platform windowing, input events, and GPU context. Backends in `window/src/os/{windows,macos,x11,wayland}`.
7. **`wezterm-gui/`** — the renderer and all UI. This is where GUI changes go, *not* `src/` (CONTRIBUTING.md is stale on this point). Notable areas:
   - `termwindow/` — the per-window controller; `mod.rs` holds `perform_key_assignment`, the dispatch point for every user action. `render/` is the draw path; `box_model.rs` is the layout primitive used by UI chrome.
   - `overlay/` — full-pane modal UIs (copy mode, quick select, launcher, prompts).
   - `commands.rs` — `CommandDef` table: brief/doc text, default keys, menubar placement, icon. Drives both the command palette and the menubar.
   - `inputmap.rs` — key/mouse binding resolution.
   - `glyphcache.rs`, `shapecache.rs`, `renderstate.rs`, `webgpu.rs`, `*.glsl`/`shader.wgsl` — text shaping and GPU rendering.
8. **`config/`** — configuration, backed by Lua (mlua). `wezterm-dynamic` provides `FromDynamic`/`ToDynamic`, the bridge between Lua tables and Rust structs.
9. **`codec/`, `wezterm-client/`, `wezterm-mux-server/`** — the wire protocol and client/server halves of remote multiplexing. A protocol change must be made in `codec/` and honored by both sides.

`lua-api-crates/*` each register one `wezterm.<module>` Lua namespace (battery, time, url, mux, window, serde, …); add new Lua-visible functionality as a crate there rather than growing `config/src/lua.rs`.

### Binaries

- `wezterm` — CLI (`wezterm/`). `wezterm start` locates and execs `wezterm-gui`; it is not itself the GUI.
- `wezterm-gui` — the actual GUI process.
- `wezterm-mux-server` — headless multiplexer.

Useful when debugging the GUI: `wezterm-gui start --always-new-process` keeps mux logs in the foreground process, and `wezterm-gui --config-file ./test-conf.lua` loads a scratch config.

## Common change recipes

### Adding a config option

Follow commit `5c09609` as the template. Four places:

1. `config/src/config.rs` — add the field to `struct Config` with a `#[dynamic(default = "...")]` attribute, plus an optional `validate = "..."` fn. Defaults are named functions, not literals.
2. Consume it, typically in `wezterm-gui/`.
3. `docs/config/lua/config/<option_name>.md` — new file, with `tags:` frontmatter and a heading of the form `option_name = <default>`.
4. `docs/changelog.md` — entry under `#### New`, linking the doc page.

### Adding a key assignment / command

1. `config/src/keyassignment.rs` — new `KeyAssignment` enum variant.
2. `wezterm-gui/src/termwindow/mod.rs` — handle it in `perform_key_assignment`.
3. `wezterm-gui/src/commands.rs` — add a `CommandDef` so it appears in the command palette/menubar and can carry a default keybinding.
4. `docs/config/lua/keyassignment/<Name>.md` and a changelog entry.

## Generated files — do not edit directly

- `.github/workflows/gen_*.yml` are emitted by `ci/generate-workflows.py`, which deletes and regenerates all of them. Edit the generator.
- `assets/shell-completion/*` and `docs/examples/cmd-synopsis-*.txt` are produced by `ci/update-derived-files.sh` from a debug build; re-run it after changing CLI arguments or key tables.
- `docs/examples/default-copy-mode-key-table.markdown` and the search-mode equivalent come from the same script.

## Docs

Docs are mkdocs-based under `docs/`. Build and live-reload with `ci/build-docs.sh serve`; arguments pass through to `mkdocs`.
