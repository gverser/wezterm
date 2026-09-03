# WezTerm 飞书桥 · Rust 侧实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `wezterm-mux-server` 内提供一个本地 TCP 服务，让外部进程能订阅 mux 的 pane 事件、读取输出增量、向 pane 发送输入、按预设条目起停 session。

**Architecture:** 新增两个 crate。`wezterm-sessions` 把 session 树的数据模型与持久化从 GUI 中提取出来，供 GUI 与桥共用。`wezterm-bridge` 订阅 `MuxNotification` 维护每 pane 的 dirty 标记与 seqno 水位线，通过 NDJSON over TCP 对外提供事件流与指令接口，并以窄 trait `PaneAccess` 隔离对 `mux::Pane` 的依赖以保证可测性。

**Tech Stack:** Rust 2021，仅用已在 workspace 中的依赖（`std::net`、`serde`、`serde_json`、`anyhow`、`log`、`parking_lot`、`mux`、`config`）。不引入任何新的第三方 crate。

**Spec:** `docs-internal/2026-09-03-feishu-bridge-design.md`

## Global Constraints

- **分支**：`feishu-bridge`（已基于 `session-sidebar` 创建）
- **MSRV 1.71**（`ci/check-rust-version.sh`）
- **Windows 上必须在 PowerShell 中构建，不能用 Git Bash**。`.cargo/config.toml` 对 `x86_64-pc-windows-msvc` 强制 `-C target-feature=+crt-static`，导致 `openssl-src` 从源码编译，需要 `C:\Strawberry\perl\bin\perl.exe`；Git Bash 的 msys perl 会让 OpenSSL 的 `./Configure` 报 `Can't locate Locale/Maketext/Simple.pm`。**切换 shell 会使 `openssl-sys` 的 build script 失效并触发约 25 分钟全量重建 —— 全程只用一个 shell。**
- **零新依赖**：IPC 用 `std::net::TcpListener`，不引入 tokio/hyper/tungstenite
- **IPC token 绝不导出为环境变量**（spec §3.3）
- **格式化**：`cargo +nightly fmt --all`（`.rustfmt.toml` 用了 unstable 的 `imports_granularity`，CI 会 `--check`）
- **测试**：`cargo nextest run --all --no-fail-fast`；单 crate 用 `cargo test -p <crate>`
- **内部 crate 依赖写法**：根 `Cargo.toml` 的 `[workspace.dependencies]` 声明 `name = { path = "dir" }`，使用方写 `name.workspace = true`
- **测试风格**：跟随仓库惯例，`#[cfg(test)] mod test` 内联在被测文件末尾

---

### Task 1: 提取 `wezterm-sessions` crate

把 session 树的数据模型与持久化从 `wezterm-gui` 中移出，供 mux-server 侧复用。GUI 的布局与文案函数留在原处。

**Files:**
- Create: `wezterm-sessions/Cargo.toml`
- Create: `wezterm-sessions/src/lib.rs`
- Create: `wezterm-sessions/src/tree.rs`（由 `wezterm-gui/src/sessions/tree.rs` 移动而来）
- Create: `wezterm-sessions/src/store.rs`（由 `wezterm-gui/src/sessions/store.rs` 移动而来）
- Modify: `Cargo.toml`（`[workspace.dependencies]` 增加一行）
- Modify: `wezterm-gui/Cargo.toml`（增加依赖）
- Modify: `wezterm-gui/src/sessions/mod.rs`（改为 re-export，保留 GUI 函数）
- Delete: `wezterm-gui/src/sessions/tree.rs`、`wezterm-gui/src/sessions/store.rs`

**Interfaces:**
- Consumes: 无（第一个任务）
- Produces: crate `wezterm_sessions`，公开 `NodeId`、`NodeKind`、`RowKind`、`SessionNode`、`SessionSpawn`、`SessionTree`、`VisibleRow`、`store::SessionStore`、`store::sessions_file_name()`。后续 Task 5 用 `SessionSpawn { domain, cwd, args, env }` 起 pane。

- [ ] **Step 1: 用 git mv 移动文件，保留历史**

```bash
cd /d/dev/wezterm
mkdir -p wezterm-sessions/src
git mv wezterm-gui/src/sessions/tree.rs wezterm-sessions/src/tree.rs
git mv wezterm-gui/src/sessions/store.rs wezterm-sessions/src/store.rs
```

- [ ] **Step 2: 写 `wezterm-sessions/Cargo.toml`**

```toml
[package]
name = "wezterm-sessions"
version = "0.1.0"
edition = "2021"
repository = "https://github.com/wezterm/wezterm"
description = "Saved session tree shared by the gui sidebar and the bridge"
license = "MIT"

[dependencies]
anyhow.workspace = true
config.workspace = true
log.workspace = true
serde = {workspace=true, features = ["derive"]}
serde_json.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 3: 写 `wezterm-sessions/src/lib.rs`**

```rust
//! The saved session tree.
//!
//! `tree` is the data model, `store` is its persistence. Both are free of
//! rendering concerns: the gui sidebar and the mux-server bridge share them.

pub mod store;
pub mod tree;

pub use store::{sessions_file_name, SessionStore};
pub use tree::{NodeId, NodeKind, RowKind, SessionNode, SessionSpawn, SessionTree, VisibleRow};
```

- [ ] **Step 4: 修正 `store.rs` 的内部 import**

原文件第 8 行是 `use crate::sessions::tree::SessionTree;`，在新 crate 里 `sessions` 这一层不存在了。改为：

```rust
use crate::tree::SessionTree;
```

同时检查 `store.rs` 中是否还有其他 `crate::sessions::` 前缀，一并去掉 `sessions::` 这一段。

- [ ] **Step 5: 在根 `Cargo.toml` 的 `[workspace.dependencies]` 中声明**

按字母序插入（在 `wezterm-mux-server-impl` 与 `wezterm-term` 之间）：

```toml
wezterm-sessions = { path = "wezterm-sessions" }
```

- [ ] **Step 6: 在 `wezterm-gui/Cargo.toml` 的 `[dependencies]` 中加入**

```toml
wezterm-sessions.workspace = true
```

- [ ] **Step 7: 把 `wezterm-gui/src/sessions/mod.rs` 改为 re-export + 保留 GUI 函数**

删除文件顶部的 `pub mod store;` / `pub mod tree;` / `pub use tree::{...};`，替换为下面这段；文件其余部分（`clamp_scroll`、`visible_range`、`clamp_width`、`close_prompt` 及其 `mod test`）**原样保留不动**：

```rust
//! Gui-side helpers for the session sidebar.
//!
//! The data model and its persistence live in the `wezterm-sessions` crate so
//! that the mux-server bridge can share them. What stays here is the sidebar's
//! own layout arithmetic and prompt copy, which is a rendering concern.

pub use wezterm_sessions::store;
pub use wezterm_sessions::tree;
pub use wezterm_sessions::{
    NodeId, NodeKind, RowKind, SessionNode, SessionSpawn, SessionTree, VisibleRow,
};

use std::ops::Range;
```

- [ ] **Step 8: 编译检查（PowerShell）**

Run: `cargo check -p wezterm-sessions -p wezterm-gui`
Expected: 通过。若报某个符号找不到，说明 sidebar 里用了 `crate::sessions::tree::X` 这类深路径 —— 上面的 `pub use wezterm_sessions::tree;` 已经把 `tree` 与 `store` 两个模块名转出去了，路径应当保持可用。

- [ ] **Step 9: 跑测试确认行为未变**

Run: `cargo test -p wezterm-sessions -p wezterm-gui`
Expected: PASS。`tree.rs`/`store.rs` 里原有的测试（含 `load_reseeds_the_id_allocator`、`save_overwrites_a_previous_file` 等）随文件移动到新 crate，应全部通过；`mod.rs` 里 `close_prompt`/`visible_range`/`clamp_width` 的测试留在 gui，也应全部通过。

- [ ] **Step 10: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "sessions: extract the tree and its store into wezterm-sessions

The mux-server side needs the saved session list to spawn a session on
request, and it cannot see a module that lives inside wezterm-gui. The
data model and its persistence move out; the sidebar's layout arithmetic
and prompt copy stay behind, because those are rendering concerns that
the bridge has no use for.

wezterm-gui/src/sessions/mod.rs re-exports the moved types so the
sidebar's existing paths keep working."
```

---

### Task 2: `wezterm-bridge` 骨架与 pane 状态机

建立 crate，定义窄 trait `PaneAccess`，实现"只记水位线、不存内容"的状态机。

**Files:**
- Create: `wezterm-bridge/Cargo.toml`
- Create: `wezterm-bridge/src/lib.rs`
- Create: `wezterm-bridge/src/state.rs`
- Modify: `Cargo.toml`（`[workspace.dependencies]` 增加一行）

**Interfaces:**
- Consumes: 无
- Produces:
  - `trait PaneAccess { fn pane_id(&self) -> PaneId; fn current_seqno(&self) -> SequenceNo; fn changed_since(&self, seqno: SequenceNo) -> bool; fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>); fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()>; }`
  - `struct PaneState { pub pane_id: PaneId, pub last_seqno: SequenceNo, pub dirty: bool }`
  - `struct BridgeState`，方法 `mark_dirty(PaneId)`、`add_pane(PaneId)`、`remove_pane(PaneId)`、`take_dirty() -> Vec<PaneId>`、`pane_ids() -> Vec<PaneId>`、`set_seqno(PaneId, SequenceNo)`
  - Task 3 的协议层与 Task 4 的服务层都依赖这些名字。

- [ ] **Step 1: 写 `wezterm-bridge/Cargo.toml`**

```toml
[package]
name = "wezterm-bridge"
version = "0.1.0"
edition = "2021"
repository = "https://github.com/wezterm/wezterm"
description = "Local IPC bridge exposing mux panes to an external client"
license = "MIT"

[dependencies]
anyhow.workspace = true
config.workspace = true
log.workspace = true
mux.workspace = true
parking_lot.workspace = true
serde = {workspace=true, features = ["derive"]}
serde_json.workspace = true
termwiz.workspace = true
wezterm-sessions.workspace = true
```

- [ ] **Step 2: 在根 `Cargo.toml` 的 `[workspace.dependencies]` 中声明**

按字母序插入（在 `wezterm-open-url` 与 `wezterm-sessions` 之间）：

```toml
wezterm-bridge = { path = "wezterm-bridge" }
```

- [ ] **Step 3: 写 `wezterm-bridge/src/lib.rs`**

```rust
//! A local IPC service that exposes mux panes to an external client.
//!
//! The bridge holds no state proportional to output: `PaneOutput` carries
//! only a pane id and the content is already in the scrollback, so a dirty
//! flag and a seqno watermark are all that is kept. Memory is flat whether
//! the client is away for a minute or for hours.

pub mod state;

pub use state::{BridgeState, PaneAccess, PaneState};
```

- [ ] **Step 4: 写 `wezterm-bridge/src/state.rs` 的失败测试**

```rust
use mux::pane::PaneId;
use parking_lot::Mutex;
use std::collections::BTreeMap;
use termwiz::surface::SequenceNo;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_new_pane_starts_clean() {
        let state = BridgeState::new();
        state.add_pane(7);
        assert_eq!(state.take_dirty(), Vec::<PaneId>::new());
    }

    #[test]
    fn output_marks_the_pane_dirty() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        assert_eq!(state.take_dirty(), vec![7]);
    }

    #[test]
    fn taking_dirty_clears_the_flag() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        state.take_dirty();
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "a flush consumes the flag; the next flush has nothing to do"
        );
    }

    #[test]
    fn repeated_output_does_not_accumulate() {
        let state = BridgeState::new();
        state.add_pane(7);
        for _ in 0..10_000 {
            state.mark_dirty(7);
        }
        assert_eq!(
            state.take_dirty(),
            vec![7],
            "ten thousand output events collapse into one unit of work"
        );
    }

    #[test]
    fn output_from_an_unknown_pane_is_ignored() {
        let state = BridgeState::new();
        state.mark_dirty(99);
        assert_eq!(
            state.take_dirty(),
            Vec::<PaneId>::new(),
            "a pane we were never told about has no thread to send to"
        );
    }

    #[test]
    fn removing_a_pane_drops_its_entry() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.mark_dirty(7);
        state.remove_pane(7);
        assert_eq!(state.pane_ids(), Vec::<PaneId>::new());
        assert_eq!(state.take_dirty(), Vec::<PaneId>::new());
    }

    #[test]
    fn the_watermark_survives_a_flush() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.set_seqno(7, 1182);
        state.mark_dirty(7);
        state.take_dirty();
        assert_eq!(state.seqno(7), Some(1182));
    }
}
```

- [ ] **Step 5: 运行测试确认失败**

Run: `cargo test -p wezterm-bridge`
Expected: FAIL，编译错误 `cannot find type BridgeState in this scope`

- [ ] **Step 6: 实现 `state.rs`**

在测试模块之前插入：

```rust
/// The subset of `mux::pane::Pane` that the bridge needs.
///
/// `Pane` has dozens of methods; depending on all of them would make the
/// bridge untestable. Production wraps a real `Pane`; tests use a fake.
pub trait PaneAccess {
    fn pane_id(&self) -> PaneId;
    fn current_seqno(&self) -> SequenceNo;
    fn changed_since(&self, seqno: SequenceNo) -> bool;
    /// The last `max_lines` lines of the pane, as plain text, together with
    /// the stable row index of the first line returned.
    fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>);
    fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()>;
}

/// What the bridge remembers about one pane: roughly 60 bytes, regardless of
/// how much that pane has printed.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneState {
    pub pane_id: PaneId,
    pub last_seqno: SequenceNo,
    pub dirty: bool,
}

impl PaneState {
    fn new(pane_id: PaneId) -> Self {
        Self {
            pane_id,
            last_seqno: 0,
            dirty: false,
        }
    }
}

#[derive(Default)]
pub struct BridgeState {
    panes: Mutex<BTreeMap<PaneId, PaneState>>,
}

impl BridgeState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_pane(&self, pane_id: PaneId) {
        self.panes
            .lock()
            .entry(pane_id)
            .or_insert_with(|| PaneState::new(pane_id));
    }

    pub fn remove_pane(&self, pane_id: PaneId) {
        self.panes.lock().remove(&pane_id);
    }

    /// Note that a pane produced output. Called from the mux notification
    /// thread and deliberately cheap: it sets a bit and returns.
    pub fn mark_dirty(&self, pane_id: PaneId) {
        if let Some(entry) = self.panes.lock().get_mut(&pane_id) {
            entry.dirty = true;
        }
    }

    /// The panes with pending output, clearing their flags.
    pub fn take_dirty(&self) -> Vec<PaneId> {
        let mut panes = self.panes.lock();
        let mut out = vec![];
        for (id, entry) in panes.iter_mut() {
            if entry.dirty {
                entry.dirty = false;
                out.push(*id);
            }
        }
        out
    }

    pub fn pane_ids(&self) -> Vec<PaneId> {
        self.panes.lock().keys().copied().collect()
    }

    pub fn set_seqno(&self, pane_id: PaneId, seqno: SequenceNo) {
        if let Some(entry) = self.panes.lock().get_mut(&pane_id) {
            entry.last_seqno = seqno;
        }
    }

    pub fn seqno(&self, pane_id: PaneId) -> Option<SequenceNo> {
        self.panes.lock().get(&pane_id).map(|e| e.last_seqno)
    }
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test -p wezterm-bridge`
Expected: PASS，7 个测试全部通过

- [ ] **Step 8: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "bridge: pane state that does not grow with output

PaneOutput carries only a pane id, so the bridge keeps a dirty flag and a
seqno watermark and reads the content back out of the scrollback when it
is ready to send. Ten thousand output events collapse into one unit of
work and memory is flat however long the client stays away.

PaneAccess is the slice of mux::Pane the bridge actually needs. Pane has
dozens of methods and depending on all of them would leave the state
machine untestable."
```

---

### Task 3: IPC 协议类型

定义两个方向的消息类型与 JSON 编解码。

**Files:**
- Create: `wezterm-bridge/src/proto.rs`
- Modify: `wezterm-bridge/src/lib.rs`（挂上 `pub mod proto;`）

**Interfaces:**
- Consumes: `PaneId` from Task 2
- Produces:
  - `enum Outbound`：`PaneAdded { pane, workspace, title, cwd }`、`PaneRemoved { pane }`、`Dirty { pane, seqno }`、`Lines { pane, first_row, lines, seqno }`、`Alert { pane, kind }`、`Panes { panes }`、`Error { message }`、`AuthOk`
  - `enum Inbound`：`Auth { token }`、`Send { pane, text, submit }`、`Read { pane, max_lines }`、`Spawn { node_id }`、`Kill { pane }`、`List`
  - `struct PaneInfo { pane, workspace, title, cwd }`
  - Task 4 用 `Inbound::Auth` 做鉴权，Task 5 处理其余变体。

- [ ] **Step 1: 写 `proto.rs` 的失败测试**

```rust
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn inbound_send_parses() {
        let json = r#"{"t":"send","pane":7,"text":"继续","submit":true}"#;
        let msg: Inbound = serde_json::from_str(json).expect("to parse");
        assert_eq!(
            msg,
            Inbound::Send {
                pane: 7,
                text: "继续".to_string(),
                submit: true,
            }
        );
    }

    #[test]
    fn submit_defaults_to_true() {
        let json = r#"{"t":"send","pane":7,"text":"hi"}"#;
        let msg: Inbound = serde_json::from_str(json).expect("to parse");
        assert_eq!(
            msg,
            Inbound::Send {
                pane: 7,
                text: "hi".to_string(),
                submit: true,
            },
            "a message without an explicit submit is meant to be sent"
        );
    }

    #[test]
    fn inbound_list_parses_without_fields() {
        let msg: Inbound = serde_json::from_str(r#"{"t":"list"}"#).expect("to parse");
        assert_eq!(msg, Inbound::List);
    }

    #[test]
    fn an_unknown_tag_is_an_error_not_a_panic() {
        let result: Result<Inbound, _> = serde_json::from_str(r#"{"t":"launch_missiles"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn outbound_dirty_serializes_to_one_line() {
        let json = serde_json::to_string(&Outbound::Dirty {
            pane: 7,
            seqno: 1182,
        })
        .expect("to serialize");
        assert_eq!(json, r#"{"t":"dirty","pane":7,"seqno":1182}"#);
        assert!(!json.contains('\n'), "NDJSON framing needs one line");
    }

    #[test]
    fn outbound_lines_round_trips() {
        let msg = Outbound::Lines {
            pane: 7,
            first_row: 3140,
            lines: vec!["a".to_string(), "b".to_string()],
            seqno: 1182,
        };
        let json = serde_json::to_string(&msg).expect("to serialize");
        let back: Outbound = serde_json::from_str(&json).expect("to parse");
        assert_eq!(back, msg);
    }

    #[test]
    fn a_line_containing_a_newline_stays_one_json_line() {
        // Terminal text can contain anything; NDJSON framing must survive it.
        let msg = Outbound::Lines {
            pane: 7,
            first_row: 0,
            lines: vec!["oops\nsecond".to_string()],
            seqno: 1,
        };
        let json = serde_json::to_string(&msg).expect("to serialize");
        assert!(
            !json.contains('\n'),
            "the newline must be escaped, not emitted raw: {json}"
        );
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p wezterm-bridge proto`
Expected: FAIL，`cannot find type Inbound in this scope`

- [ ] **Step 3: 实现 `proto.rs`**

在测试模块之前插入：

```rust
use mux::pane::PaneId;
use termwiz::surface::SequenceNo;

fn default_true() -> bool {
    true
}

/// A pane as described to the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: PaneId,
    pub workspace: String,
    pub title: String,
    pub cwd: Option<String>,
}

/// Bridge -> client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Outbound {
    AuthOk,
    PaneAdded {
        pane: PaneId,
        workspace: String,
        title: String,
        cwd: Option<String>,
    },
    PaneRemoved {
        pane: PaneId,
    },
    Dirty {
        pane: PaneId,
        seqno: SequenceNo,
    },
    Lines {
        pane: PaneId,
        first_row: isize,
        lines: Vec<String>,
        seqno: SequenceNo,
    },
    Alert {
        pane: PaneId,
        kind: String,
    },
    Panes {
        panes: Vec<PaneInfo>,
    },
    Error {
        message: String,
    },
}

/// Client -> bridge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Inbound {
    Auth {
        token: String,
    },
    Send {
        pane: PaneId,
        text: String,
        #[serde(default = "default_true")]
        submit: bool,
    },
    Read {
        pane: PaneId,
        #[serde(default = "default_max_lines")]
        max_lines: usize,
    },
    Spawn {
        node_id: u64,
    },
    Kill {
        pane: PaneId,
    },
    List,
}

fn default_max_lines() -> usize {
    200
}
```

- [ ] **Step 4: 在 `lib.rs` 中挂上模块**

在 `pub mod state;` 之前加一行（保持字母序）：

```rust
pub mod proto;
```

并在 re-export 中加入：

```rust
pub use proto::{Inbound, Outbound, PaneInfo};
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p wezterm-bridge`
Expected: PASS，Task 2 的 7 个加上本任务的 7 个共 14 个通过

- [ ] **Step 6: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "bridge: the wire protocol

NDJSON in both directions with a t tag, so the client can read it with a
line reader and no framing logic. The tests pin the two properties the
framing depends on: one message is one line, and terminal text carrying a
newline is escaped rather than emitted raw."
```

---

### Task 4: TCP 服务与鉴权

监听 `127.0.0.1`，第一行必须是有效 auth，否则断开。

**Files:**
- Create: `wezterm-bridge/src/server.rs`
- Create: `wezterm-bridge/src/token.rs`
- Modify: `wezterm-bridge/src/lib.rs`

**Interfaces:**
- Consumes: `Inbound`、`Outbound` from Task 3
- Produces:
  - `token::load_or_create(path: &Path) -> anyhow::Result<String>`
  - `token::verify(expected: &str, presented: &str) -> bool`
  - `server::authenticate(first_line: &str, expected_token: &str) -> Result<(), String>`
  - `server::serve(port: u16, token: String, handler: Arc<dyn CommandHandler>)`，Task 5 提供 `CommandHandler`。

- [ ] **Step 1: 写 `token.rs` 与 `server.rs` 的失败测试**

`wezterm-bridge/src/token.rs`：

```rust
use anyhow::Context;
use std::path::Path;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_missing_file_gets_a_fresh_token() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        let token = load_or_create(&path).expect("to create");
        assert_eq!(token.len(), 64, "32 bytes rendered as hex");
        assert!(path.exists());
    }

    #[test]
    fn an_existing_token_is_reused() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        let first = load_or_create(&path).expect("to create");
        let second = load_or_create(&path).expect("to load");
        assert_eq!(
            first, second,
            "regenerating would lock out a client that is already connected"
        );
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let dir = tempfile::tempdir().expect("to make a tempdir");
        let path = dir.path().join("bridge-token");
        std::fs::write(&path, "abc123\n").expect("to write");
        assert_eq!(load_or_create(&path).expect("to load"), "abc123");
    }

    #[test]
    fn verify_rejects_a_wrong_token() {
        assert!(!verify("expected", "presented"));
    }

    #[test]
    fn verify_accepts_the_right_token() {
        assert!(verify("expected", "expected"));
    }

    #[test]
    fn verify_rejects_a_prefix() {
        assert!(
            !verify("expectedlonger", "expected"),
            "a prefix must not pass"
        );
    }
}
```

`wezterm-bridge/src/server.rs`：

```rust
use crate::proto::Inbound;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn a_correct_token_authenticates() {
        let line = r#"{"t":"auth","token":"secret"}"#;
        assert_eq!(authenticate(line, "secret"), Ok(()));
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let line = r#"{"t":"auth","token":"guess"}"#;
        assert!(authenticate(line, "secret").is_err());
    }

    #[test]
    fn the_first_line_must_be_auth() {
        let line = r#"{"t":"send","pane":7,"text":"hi"}"#;
        let err = authenticate(line, "secret").expect_err("to refuse");
        assert!(
            err.contains("auth"),
            "the client needs to be told what was wrong: {err}"
        );
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert!(authenticate("not json at all", "secret").is_err());
    }

    #[test]
    fn an_empty_first_line_is_refused() {
        assert!(authenticate("", "secret").is_err());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p wezterm-bridge`
Expected: FAIL，`cannot find function load_or_create` / `authenticate`

- [ ] **Step 3: 实现 `token.rs`**

在测试模块之前插入。随机数不引入新依赖，用进程与时间熵混合 —— 这个 token 只防同机其他进程误连，不是密码学密钥：

```rust
/// Read the token at `path`, creating one if it is not there yet.
///
/// The token is only ever read from this file. It must never be exported
/// into the environment: it is equivalent to the right to drive the mux,
/// and any child process a session spawns can read the environment.
pub fn load_or_create(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    let token = generate();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, &token).with_context(|| format!("writing {}", path.display()))?;
    restrict_permissions(path);
    Ok(token)
}

fn generate() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let addr = &nanos as *const u128 as usize as u128;

    // Four rounds of splitmix64 over the mixed seed gives 32 bytes.
    let mut seed = nanos ^ (pid << 64) ^ addr;
    let mut out = String::with_capacity(64);
    for _ in 0..4 {
        seed = seed.wrapping_add(0x9E3779B97F4A7C15_9E3779B97F4A7C15);
        let mut z = seed as u64;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.push_str(&format!("{z:016x}"));
    }
    out
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {
    // On Windows the file inherits the user profile's ACL, which already
    // excludes other users.
}

/// Compare in constant time so a wrong token cannot be guessed a byte at a
/// time by timing the reply.
pub fn verify(expected: &str, presented: &str) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(presented.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}
```

- [ ] **Step 4: 实现 `server.rs` 的 `authenticate`**

在测试模块之前插入：

```rust
/// The first line a client sends must be a valid auth message.
pub fn authenticate(first_line: &str, expected_token: &str) -> Result<(), String> {
    let msg: Inbound = serde_json::from_str(first_line)
        .map_err(|err| format!("the first line must be an auth message: {err}"))?;
    match msg {
        Inbound::Auth { token } => {
            if crate::token::verify(expected_token, &token) {
                Ok(())
            } else {
                Err("auth token rejected".to_string())
            }
        }
        _ => Err("the first line must be an auth message".to_string()),
    }
}
```

- [ ] **Step 5: 在 `lib.rs` 中挂上模块**

```rust
pub mod proto;
pub mod server;
pub mod state;
pub mod token;
```

- [ ] **Step 6: 在 `wezterm-bridge/Cargo.toml` 中加 dev-dependency**

```toml
[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test -p wezterm-bridge`
Expected: PASS，共 25 个测试通过

- [ ] **Step 8: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "bridge: local listener and its token

The socket is on loopback, which every process on the machine can reach,
so the first line has to prove the client read a file only this user can
read. Comparison is constant time.

The token is deliberately never exported into the environment: it is the
right to drive the mux, and a session's child processes can read the
environment of the shell they were spawned from."
```

---

### Task 5: 指令执行

把 `Inbound` 变成对 mux 的实际操作。

**Files:**
- Create: `wezterm-bridge/src/commands.rs`
- Create: `wezterm-bridge/src/mux_access.rs`
- Modify: `wezterm-bridge/src/lib.rs`

**Interfaces:**
- Consumes: `PaneAccess`、`BridgeState` from Task 2；`Inbound`、`Outbound`、`PaneInfo` from Task 3
- Produces:
  - `trait CommandHandler { fn handle(&self, msg: Inbound) -> Vec<Outbound>; }`
  - `struct MuxPane(Arc<dyn mux::pane::Pane>)`，实现 `PaneAccess`
  - `fn dispatch<P: PaneAccess>(msg: Inbound, state: &BridgeState, lookup: impl Fn(PaneId) -> Option<P>) -> Vec<Outbound>`

- [ ] **Step 1: 写 `commands.rs` 的失败测试**

```rust
use crate::proto::{Inbound, Outbound};
use crate::state::{BridgeState, PaneAccess};
use mux::pane::PaneId;

#[cfg(test)]
mod test {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;
    use termwiz::surface::SequenceNo;

    #[derive(Clone)]
    struct FakePane {
        id: PaneId,
        seqno: SequenceNo,
        lines: Vec<String>,
        sent: Arc<Mutex<Vec<(String, bool)>>>,
    }

    impl FakePane {
        fn new(id: PaneId) -> Self {
            Self {
                id,
                seqno: 100,
                lines: vec!["first".to_string(), "second".to_string()],
                sent: Arc::new(Mutex::new(vec![])),
            }
        }
    }

    impl PaneAccess for FakePane {
        fn pane_id(&self) -> PaneId {
            self.id
        }
        fn current_seqno(&self) -> SequenceNo {
            self.seqno
        }
        fn changed_since(&self, seqno: SequenceNo) -> bool {
            seqno < self.seqno
        }
        fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>) {
            let start = self.lines.len().saturating_sub(max_lines);
            (start as isize, self.lines[start..].to_vec())
        }
        fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()> {
            self.sent.lock().push((text.to_string(), submit));
            Ok(())
        }
    }

    #[test]
    fn read_returns_the_lines_and_the_watermark() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let out = dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 10,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            out,
            vec![Outbound::Lines {
                pane: 7,
                first_row: 0,
                lines: vec!["first".to_string(), "second".to_string()],
                seqno: 100,
            }]
        );
    }

    #[test]
    fn read_advances_the_watermark() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 10,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            state.seqno(7),
            Some(100),
            "the next read must not resend what this one already returned"
        );
    }

    #[test]
    fn read_honours_max_lines() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let out = dispatch(
            Inbound::Read {
                pane: 7,
                max_lines: 1,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(
            out,
            vec![Outbound::Lines {
                pane: 7,
                first_row: 1,
                lines: vec!["second".to_string()],
                seqno: 100,
            }]
        );
    }

    #[test]
    fn send_reaches_the_pane() {
        let state = BridgeState::new();
        state.add_pane(7);
        let pane = FakePane::new(7);
        let sent = pane.sent.clone();
        dispatch(
            Inbound::Send {
                pane: 7,
                text: "继续".to_string(),
                submit: true,
            },
            &state,
            |_| Some(pane.clone()),
        );
        assert_eq!(*sent.lock(), vec![("继续".to_string(), true)]);
    }

    #[test]
    fn a_command_for_an_unknown_pane_reports_an_error() {
        let state = BridgeState::new();
        let out = dispatch(
            Inbound::Read {
                pane: 99,
                max_lines: 10,
            },
            &state,
            |_| None::<FakePane>,
        );
        assert_eq!(
            out,
            vec![Outbound::Error {
                message: "no such pane: 99".to_string()
            }],
            "a stale pane id from the client must not take the service down"
        );
    }

    #[test]
    fn list_reports_the_known_panes() {
        let state = BridgeState::new();
        state.add_pane(7);
        state.add_pane(9);
        let out = dispatch(Inbound::List, &state, |id| Some(FakePane::new(id)));
        match &out[0] {
            Outbound::Panes { panes } => {
                assert_eq!(panes.len(), 2);
                assert_eq!(panes[0].pane, 7);
                assert_eq!(panes[1].pane, 9);
            }
            other => panic!("expected a Panes reply, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p wezterm-bridge commands`
Expected: FAIL，`cannot find function dispatch`

- [ ] **Step 3: 实现 `dispatch`**

`Spawn` 与 `Kill` 需要真实的 Mux，无法在纯函数里完成，因此 `dispatch` 只处理可测的四种，另两种在 Task 6 的 mux 层处理。在测试模块之前插入：

```rust
use crate::proto::PaneInfo;

/// Turn one client message into the replies it warrants.
///
/// `lookup` resolves a pane id; returning None means the pane is gone, which
/// is expected whenever the client is working from a stale list.
pub fn dispatch<P, F>(msg: Inbound, state: &BridgeState, lookup: F) -> Vec<Outbound>
where
    P: PaneAccess,
    F: Fn(PaneId) -> Option<P>,
{
    match msg {
        Inbound::Auth { .. } => vec![Outbound::Error {
            message: "already authenticated".to_string(),
        }],

        Inbound::Read { pane, max_lines } => match lookup(pane) {
            None => vec![no_such_pane(pane)],
            Some(p) => {
                let seqno = p.current_seqno();
                let (first_row, lines) = p.read_lines(max_lines);
                state.set_seqno(pane, seqno);
                vec![Outbound::Lines {
                    pane,
                    first_row,
                    lines,
                    seqno,
                }]
            }
        },

        Inbound::Send {
            pane,
            text,
            submit,
        } => match lookup(pane) {
            None => vec![no_such_pane(pane)],
            Some(p) => match p.send_text(&text, submit) {
                Ok(()) => vec![],
                Err(err) => vec![Outbound::Error {
                    message: format!("send to pane {pane} failed: {err:#}"),
                }],
            },
        },

        Inbound::List => {
            let panes = state
                .pane_ids()
                .into_iter()
                .filter_map(|id| {
                    lookup(id).map(|_| PaneInfo {
                        pane: id,
                        workspace: String::new(),
                        title: String::new(),
                        cwd: None,
                    })
                })
                .collect();
            vec![Outbound::Panes { panes }]
        }

        Inbound::Spawn { .. } | Inbound::Kill { .. } => vec![Outbound::Error {
            message: "spawn and kill are handled by the mux layer".to_string(),
        }],
    }
}

fn no_such_pane(pane: PaneId) -> Outbound {
    Outbound::Error {
        message: format!("no such pane: {pane}"),
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p wezterm-bridge`
Expected: PASS，共 31 个测试通过

- [ ] **Step 5: 实现 `mux_access.rs`，把真实 Pane 接进来**

这一层没有单元测试 —— 它是纯粹的委托，验证在 Task 6 的端到端清单里：

```rust
//! Production `PaneAccess`: a thin wrapper over a real mux pane.

use crate::state::PaneAccess;
use mux::pane::{Pane, PaneId};
use std::sync::Arc;
use termwiz::surface::SequenceNo;

pub struct MuxPane(pub Arc<dyn Pane>);

impl PaneAccess for MuxPane {
    fn pane_id(&self) -> PaneId {
        self.0.pane_id()
    }

    fn current_seqno(&self) -> SequenceNo {
        self.0.get_current_seqno()
    }

    fn changed_since(&self, seqno: SequenceNo) -> bool {
        let dims = self.0.get_dimensions();
        let range = dims.physical_top..dims.physical_top + dims.viewport_rows as isize;
        !self.0.get_changed_since(range, seqno).is_empty()
    }

    fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>) {
        let dims = self.0.get_dimensions();
        let end = dims.physical_top + dims.viewport_rows as isize;
        let start = end.saturating_sub(max_lines as isize);
        let (first_row, lines) = self.0.get_lines(start..end);
        (
            first_row,
            lines.iter().map(|l| l.as_str().trim_end().to_string()).collect(),
        )
    }

    fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()> {
        self.0.send_paste(text)?;
        if submit {
            self.0
                .key_down(termwiz::input::KeyCode::Enter, termwiz::input::Modifiers::NONE)?;
        }
        Ok(())
    }
}
```

- [ ] **Step 6: 在 `lib.rs` 中挂上模块并编译**

```rust
pub mod commands;
pub mod mux_access;
pub mod proto;
pub mod server;
pub mod state;
pub mod token;
```

Run: `cargo check -p wezterm-bridge`
Expected: 通过。`Line::as_str()` 是取纯文本的既有方法，`wezterm/src/cli/get_text.rs:89` 与 `mux/src/localpane.rs:698` 都在用它。

- [ ] **Step 7: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "bridge: turn client messages into mux operations

dispatch is a pure function over PaneAccess, so read, send and list are
covered by tests with a fake pane rather than a live mux. A stale pane id
comes back as an error message instead of taking the connection down: the
client is working from a list that can go out of date at any moment.

MuxPane is the production side of PaneAccess and is deliberately nothing
but delegation."
```

---

### Task 6: 挂载到 mux-server 并接线

加配置项，在 mux 就绪后启动订阅与监听。

**Files:**
- Modify: `config/src/config.rs`
- Modify: `wezterm-mux-server/Cargo.toml`
- Modify: `wezterm-mux-server/src/main.rs:233`（`spawn_listener()` 调用处附近）
- Create: `wezterm-bridge/src/service.rs`
- Modify: `wezterm-bridge/src/lib.rs`

**Interfaces:**
- Consumes: 前五个任务的全部产出
- Produces: `service::start(port: u16, token_path: PathBuf) -> anyhow::Result<()>`，由 mux-server 调用

- [ ] **Step 1: 在 `config/src/config.rs` 中加三个配置项**

跟随 `session_sidebar_enabled` 的写法，加在其附近：

```rust
    /// Whether the mux server exposes the local bridge service
    #[dynamic(default)]
    pub bridge_enabled: bool,

    /// The loopback port the bridge listens on. This is the IPC service, not
    /// the hook service that the feishu client runs.
    #[dynamic(default = "default_bridge_port")]
    pub bridge_port: u16,

    /// Where the bridge's IPC token is kept. Defaults to
    /// $DATA_DIR/bridge-token.
    #[dynamic(default)]
    pub bridge_token_path: Option<std::path::PathBuf>,
```

并在同文件的默认值函数区加入：

```rust
fn default_bridge_port() -> u16 {
    8788
}
```

- [ ] **Step 2: 写 `service.rs`**

```rust
//! Wiring: subscribe to the mux, accept one client, pump both directions.

use crate::commands::dispatch;
use crate::mux_access::MuxPane;
use crate::proto::{Inbound, Outbound};
use crate::state::BridgeState;
use anyhow::Context;
use mux::{Mux, MuxNotification};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

pub fn start(port: u16, token_path: PathBuf) -> anyhow::Result<()> {
    let token = crate::token::load_or_create(&token_path)
        .with_context(|| format!("loading the bridge token from {}", token_path.display()))?;

    let state = Arc::new(BridgeState::new());
    let (tx, rx) = channel::<Outbound>();

    subscribe(state.clone(), tx.clone());

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding the bridge to 127.0.0.1:{port}"))?;
    log::info!("bridge listening on 127.0.0.1:{port}");

    let rx = Arc::new(parking_lot::Mutex::new(rx));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = serve_client(stream, &token, &state, &rx) {
                        log::warn!("bridge client ended: {err:#}");
                    }
                }
                Err(err) => log::warn!("bridge accept failed: {err:#}"),
            }
        }
    });

    Ok(())
}

fn subscribe(state: Arc<BridgeState>, tx: Sender<Outbound>) {
    Mux::get().subscribe(move |notif| {
        match notif {
            MuxNotification::PaneAdded(pane_id) => {
                state.add_pane(pane_id);
                let _ = tx.send(Outbound::PaneAdded {
                    pane: pane_id,
                    workspace: String::new(),
                    title: String::new(),
                    cwd: None,
                });
            }
            MuxNotification::PaneRemoved(pane_id) => {
                state.remove_pane(pane_id);
                let _ = tx.send(Outbound::PaneRemoved { pane: pane_id });
            }
            MuxNotification::PaneOutput(pane_id) => {
                state.mark_dirty(pane_id);
            }
            MuxNotification::Alert { pane_id, alert } => {
                let _ = tx.send(Outbound::Alert {
                    pane: pane_id,
                    kind: format!("{alert:?}"),
                });
            }
            _ => {}
        }
        true
    });
}

fn serve_client(
    stream: TcpStream,
    token: &str,
    state: &Arc<BridgeState>,
    rx: &Arc<parking_lot::Mutex<std::sync::mpsc::Receiver<Outbound>>>,
) -> anyhow::Result<()> {
    let mut writer = stream.try_clone().context("cloning the client stream")?;
    let mut reader = BufReader::new(stream);

    let mut first = String::new();
    reader.read_line(&mut first).context("reading the first line")?;
    if let Err(err) = crate::server::authenticate(first.trim(), token) {
        let _ = writeln!(writer, "{}", json(&Outbound::Error { message: err.clone() }));
        anyhow::bail!("{err}");
    }
    writeln!(writer, "{}", json(&Outbound::AuthOk))?;

    // Drain mux notifications to the client on its own thread.
    let rx = rx.clone();
    let mut notify_writer = writer.try_clone().context("cloning for notifications")?;
    std::thread::spawn(move || {
        let rx = rx.lock();
        while let Ok(msg) = rx.recv() {
            if writeln!(notify_writer, "{}", json(&msg)).is_err() {
                break;
            }
        }
    });

    // Dirty flags are flushed as notifications on a timer.
    let flush_state = state.clone();
    let mut flush_writer = writer.try_clone().context("cloning for flush")?;
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        for pane_id in flush_state.take_dirty() {
            let seqno = Mux::get()
                .get_pane(pane_id)
                .map(|p| p.get_current_seqno())
                .unwrap_or(0);
            let msg = Outbound::Dirty {
                pane: pane_id,
                seqno,
            };
            if writeln!(flush_writer, "{}", json(&msg)).is_err() {
                return;
            }
        }
    });

    for line in reader.lines() {
        let line = line.context("reading a client line")?;
        let msg: Inbound = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(err) => {
                writeln!(
                    writer,
                    "{}",
                    json(&Outbound::Error {
                        message: format!("bad message: {err}")
                    })
                )?;
                continue;
            }
        };

        for reply in dispatch(msg, state, |id| Mux::get().get_pane(id).map(MuxPane)) {
            writeln!(writer, "{}", json(&reply))?;
        }
    }

    Ok(())
}

fn json(msg: &Outbound) -> String {
    serde_json::to_string(msg).unwrap_or_else(|err| {
        format!(r#"{{"t":"error","message":"failed to serialize a reply: {err}"}}"#)
    })
}
```

- [ ] **Step 3: 在 `lib.rs` 中挂上模块**

```rust
pub mod service;
```

- [ ] **Step 4: 在 `wezterm-mux-server/Cargo.toml` 的 `[dependencies]` 中加入**

```toml
wezterm-bridge.workspace = true
```

- [ ] **Step 5: 在 `wezterm-mux-server/src/main.rs` 中挂载**

在第 233 行 `spawn_listener()` 调用**之后**插入（此时 `Mux::set_mux` 已经执行，订阅才有意义）：

```rust
    {
        let cfg = configuration();
        if cfg.bridge_enabled {
            let token_path = cfg
                .bridge_token_path
                .clone()
                .unwrap_or_else(|| config::DATA_DIR.join("bridge-token"));
            if let Err(err) = wezterm_bridge::service::start(cfg.bridge_port, token_path) {
                log::error!("failed to start the bridge: {err:#}");
            }
        }
    }
```

- [ ] **Step 6: 编译整个 workspace**

Run: `cargo build -p wezterm-mux-server -p wezterm-gui`
Expected: 通过

- [ ] **Step 7: 端到端手动验证**

在 `.wezterm.lua` 中加入：

```lua
config.bridge_enabled = true
config.unix_domains = { { name = 'main', connect_automatically = true } }
config.default_domain = 'main'
```

然后：

1. 启动 `wezterm-mux-server --daemonize`，确认日志出现 `bridge listening on 127.0.0.1:8788`
2. 读出 token：`Get-Content $env:USERPROFILE\.local\share\wezterm\bridge-token`
3. 用一个最小客户端连上去（PowerShell）：

```powershell
$token = (Get-Content "$env:USERPROFILE\.local\share\wezterm\bridge-token").Trim()
$client = New-Object System.Net.Sockets.TcpClient("127.0.0.1", 8788)
$stream = $client.GetStream()
$writer = New-Object System.IO.StreamWriter($stream)
$reader = New-Object System.IO.StreamReader($stream)
$writer.AutoFlush = $true
$writer.WriteLine("{`"t`":`"auth`",`"token`":`"$token`"}")
$reader.ReadLine()      # 期望 {"t":"auth_ok"}
$writer.WriteLine('{"t":"list"}')
$reader.ReadLine()      # 期望一条 panes 回复
```

4. 验证清单：
   - [ ] 错误 token 被拒绝并断开
   - [ ] `list` 能列出 mux 中的 pane
   - [ ] 在某个 pane 里敲字，客户端收到 `dirty` 通知
   - [ ] `{"t":"read","pane":<id>}` 返回该 pane 的最后若干行
   - [ ] `{"t":"send","pane":<id>,"text":"echo hello"}` 让该 pane 真的执行了命令
   - [ ] 关闭一个 pane，客户端收到 `pane_removed`

- [ ] **Step 8: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -m "mux-server: start the bridge when it is configured

Off by default. The subscription is installed after Mux::set_mux, since
subscribing before there is a mux to subscribe to would silently observe
nothing.

Dirty flags are flushed on a 200ms timer rather than sent per event: a
pane that is printing produces PaneOutput hundreds of times a second and
the client only needs to know that there is something new to read."
```

---

### Task 7: 关窗口默认 detach，全关改为显式动作

`close_requested()` 当前把关窗口等同于杀掉全部 tab。挂到 mux domain 后这不再成立 —— 那些 pane 属于 server，是手机正在对话的对象。

采用终端复用器的标准心智模型：**关窗口 = detach；结束 session 是另一个动作**。

**Files:**
- Modify: `config/src/keyassignment.rs`（新增枚举变体）
- Modify: `wezterm-gui/src/commands.rs`（新增 `CommandDef` 与列表项）
- Modify: `wezterm-gui/src/termwindow/mod.rs:524`（`close_requested`）与 `:3311` 附近（动作分发）

**Interfaces:**
- Consumes: 无
- Produces: `KeyAssignment::CloseAllSessions`；`TermWindow::close_all_sessions()`

- [ ] **Step 1: 在 `config/src/keyassignment.rs` 中新增变体**

在 `ToggleSessionSidebar,` / `FocusSessionSidebarSearch,` 旁边加入：

```rust
    CloseAllSessions,
```

- [ ] **Step 2: 在 `wezterm-gui/src/commands.rs` 中加 `CommandDef`**

跟随 `ToggleSessionSidebar` 的写法，加在其后：

```rust
        CloseAllSessions => CommandDef {
            brief: "Close All Sessions".into(),
            doc: "End every session, including the ones running in background \
                  workspaces, and quit"
                .into(),
            // No default binding: this ends work that is deliberately still
            // running, so it should take a moment to reach for.
            keys: vec![],
            args: &[ArgType::ActivePane],
            menubar: &["Window"],
            icon: Some("md_close_box_multiple"),
        },
```

并在同文件下方的动作名列表中，`FocusSessionSidebarSearch,` 之后加入：

```rust
        CloseAllSessions,
```

- [ ] **Step 3: 在 `wezterm-gui/src/termwindow/mod.rs` 中分发**

在第 3312 行 `FocusSessionSidebarSearch => self.focus_session_sidebar_search(),` 之后加入：

```rust
            CloseAllSessions => self.close_all_sessions(),
```

- [ ] **Step 4: 实现 `close_all_sessions`**

放在 `close_requested` 附近。它就是过去关窗口时的行为，现在改由用户显式触发：

```rust
    /// End every session and quit.
    ///
    /// This is what closing the window used to do. Attached to a mux domain
    /// the sessions outlive the window, so ending them has to be asked for.
    fn close_all_sessions(&mut self) {
        let mux = Mux::get();
        self.kill_unattended_windows();
        mux.kill_window(self.mux_window_id);
        if let Some(window) = self.window.clone() {
            window.close();
            front_end().forget_known_window(&window);
        }
    }
```

- [ ] **Step 5: 让 `close_requested` 在可 detach 时直接放行**

在 `wezterm-gui/src/termwindow/mod.rs:524` 的 `close_requested` 开头，`let unattended = ...` 之前插入：

```rust
        // A pane in a mux domain outlives this window: the server owns it,
        // the phone is talking to it, and the user is coming back to it.
        // Closing the window detaches; ending the sessions is
        // CloseAllSessions.
        let detachable = mux
            .get_active_tab_for_window(self.mux_window_id)
            .and_then(|tab| tab.get_active_pane())
            .and_then(|pane| mux.get_domain(pane.domain_id()))
            .map_or(false, |domain| domain.detachable());

        if detachable {
            window.close();
            front_end().forget_known_window(window);
            return;
        }
```

**其余部分完全不动。** 本地 domain 下 `close_prompt` 与 `kill_unattended_windows()` 的行为保持原样 —— 在那里，pane 确实随 GUI 进程一起死，提示"还有终端在后台跑"依然是对的。

- [ ] **Step 6: 编译并跑测试**

Run: `cargo build -p wezterm-gui; cargo test -p wezterm-gui`
Expected: PASS。`close_prompt` 的签名未改，其 6 个既有测试应原样通过。

- [ ] **Step 7: 手动验证**

- [ ] 配好 `default_domain = 'main'` 后关窗口：**不弹框，直接退出**
- [ ] 此时 `wezterm cli list` 仍能列出那些 pane，AI 进程还在跑
- [ ] 重开 GUI，自动连回，pane 与滚动历史都在
- [ ] 从命令面板执行 `Close All Sessions`：pane 确实全部消失
- [ ] 注释掉 `default_domain` 用本地 domain 时，关窗口**仍然**弹出原来的确认框

- [ ] **Step 8: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -F- <<'MSG'
gui: closing a window stops meaning ending the session

Killing every tab including the background workspaces is right only while
the panes are hosted by this process. Attached to a mux domain they are
not: they belong to the server, they are what the phone talks to, and the
user is coming back to them. So a detachable domain just closes the
window, the way detaching from tmux does.

Ending them is now CloseAllSessions, which is the old behaviour moved
behind an explicit request and given no default binding. On a local
domain nothing changes -- there the panes really do die with the process,
and the warning is still telling the truth.
MSG
```

---

### Task 8: `spawn` 与 `kill`

让客户端能按 `sessions.json` 的预设条目起一个 session，以及结束一个 pane。这是 L1 的范围：能起的**只有树里已有的条目**，不是任意命令。

这两个动作需要真实的 Mux 且是异步的，无法用 fake pane 覆盖，因此没有单元测试，验证靠 Step 4 的端到端清单。

**Files:**
- Modify: `wezterm-bridge/Cargo.toml`（新增两个依赖）
- Modify: `wezterm-bridge/src/service.rs`

**Interfaces:**
- Consumes: `Inbound::Spawn { node_id }`、`Inbound::Kill { pane }` from Task 3；`wezterm_sessions::{SessionStore, NodeId, NodeKind}` from Task 1
- Produces: `service::handle_spawn(node_id: u64) -> Vec<Outbound>`、`service::handle_kill(pane: PaneId) -> Vec<Outbound>`

- [ ] **Step 1: 在 `wezterm-bridge/Cargo.toml` 中加依赖**

```toml
portable-pty.workspace = true
promise.workspace = true
```

- [ ] **Step 2: 在 `service.rs` 中实现两个处理函数**

`SessionSpawn` 的 `args` / `cwd` / `env` 直接构造 `CommandBuilder`，不绕道 `SpawnCommand` —— 后者到 `CommandBuilder` 的转换住在 GUI 侧：

```rust
use portable_pty::CommandBuilder;
use wezterm_sessions::{NodeId, NodeKind, SessionStore};

/// Start the session named by a node in sessions.json.
///
/// Only entries already in the tree can be started: that is the line between
/// this and a remote shell.
pub fn handle_spawn(node_id: u64) -> Vec<Outbound> {
    let store = SessionStore::load();
    let node = match store.tree().find(NodeId(node_id)) {
        Some(node) => node,
        None => {
            return vec![Outbound::Error {
                message: format!("no such session node: {node_id}"),
            }]
        }
    };

    let spawn = match &node.kind {
        NodeKind::Session { spawn } => spawn.clone(),
        NodeKind::Folder { .. } => {
            return vec![Outbound::Error {
                message: format!("session node {node_id} is a folder, not a session"),
            }]
        }
    };

    // The gui sidebar names workspaces the same way, so a session started
    // from the phone and one started from the sidebar are the same session.
    let workspace = NodeId(node_id).workspace_name();

    promise::spawn::spawn_into_main_thread(async move {
        let mux = Mux::get();
        let size = config::configuration().initial_size(0, None);

        let mut cmd = match &spawn.args {
            Some(args) if !args.is_empty() => {
                CommandBuilder::from_argv(args.iter().map(Into::into).collect())
            }
            _ => CommandBuilder::new_default_prog(),
        };
        for (name, value) in &spawn.env {
            cmd.env(name, value);
        }

        let cwd = spawn.cwd.as_ref().map(|p| p.to_string_lossy().to_string());

        let window_id = mux.new_empty_window(Some(workspace.clone()), None);
        let domain = mux.default_domain();
        if let Err(err) = domain.spawn(size, Some(cmd), cwd, *window_id).await {
            log::error!("bridge failed to spawn session node {node_id}: {err:#}");
        }
    })
    .detach();

    // Spawning is async: the client learns the new pane id from the
    // pane_added event, not from a reply to this message.
    vec![]
}

/// End one pane.
pub fn handle_kill(pane: mux::pane::PaneId) -> Vec<Outbound> {
    promise::spawn::spawn_into_main_thread(async move {
        Mux::get().remove_pane(pane);
    })
    .detach();
    vec![]
}
```

- [ ] **Step 3: 在 `serve_client` 的消息循环中接上**

把 Task 6 中那段 `for reply in dispatch(...)` 替换为：

```rust
        let replies = match msg {
            Inbound::Spawn { node_id } => handle_spawn(node_id),
            Inbound::Kill { pane } => handle_kill(pane),
            other => dispatch(other, state, |id| Mux::get().get_pane(id).map(MuxPane)),
        };
        for reply in replies {
            writeln!(writer, "{}", json(&reply))?;
        }
```

`dispatch` 中原本对 `Spawn`/`Kill` 返回"由 mux 层处理"的分支保持不动 —— 它现在是一条不该被走到的路径，留着可以在接线出错时立刻暴露。

- [ ] **Step 4: 编译并端到端验证**

Run: `cargo build -p wezterm-mux-server`
Expected: 通过

用 Task 6 Step 7 的 PowerShell 客户端。先在 sidebar 里建一个 session 条目，它的 node id 可以从 `$env:USERPROFILE\.local\share\wezterm\sessions.json` 中读到，然后：

- [ ] `{"t":"spawn","node_id":<id>}` 之后收到 `pane_added` 通知
- [ ] `wezterm cli list` 能看到新 pane，且它的 workspace 是 `session:<id>`
- [ ] GUI 的 sidebar 上，该条目显示出 terminal count —— **证明两边认的是同一个 session**
- [ ] 对新 pane 发 `{"t":"send","pane":<新id>,"text":"echo hello"}`，命令真的执行
- [ ] `{"t":"kill","pane":<新id>}` 之后收到 `pane_removed`，且 `wezterm cli list` 里没有了
- [ ] `{"t":"spawn","node_id":999999}` 返回 `no such session node: 999999`，服务不崩
- [ ] 对一个文件夹节点 spawn，返回 `is a folder, not a session`

- [ ] **Step 5: 格式化并提交**

```bash
cargo +nightly fmt --all
git add -A
git commit -F- <<'MSG'
bridge: start and end sessions from the client

Starting is limited to entries already in sessions.json, which is the
line between this and a remote shell: the client picks a node, not a
command.

The workspace name comes from NodeId::workspace_name, the same function
the sidebar uses, so a session started from the phone and one started by
double-clicking the sidebar are the same session rather than two that
happen to share a name.

Both operations need the mux and are async, so they run through
spawn_into_main_thread and report nothing directly: the client learns
what happened from pane_added and pane_removed.
MSG
```

---

## 完成标准

- [ ] `cargo nextest run --all --no-fail-fast` 通过
- [ ] `cargo +nightly fmt --all --check` 无差异
- [ ] Task 6、7、8 的手动验证清单全部勾选
- [ ] 关掉 GUI 后，外部客户端仍能通过 8788 端口读写 mux 中的 pane，并起停 session

## 交接给下一个计划

Node 侧（飞书长连接、话题映射、卡片渲染、Claude Code hook 服务）在 Rust 侧落地后单独成计划。届时 IPC 协议的实际形态已由本计划确定，可直接照 `wezterm-bridge/src/proto.rs` 编写客户端。

本计划**未实现**的 spec 内容，留给下一个计划：

- `PaneInfo` 的 `workspace` / `title` / `cwd` 目前返回空值，需从 mux 的 window/tab 结构中取出（spec §8.1 的三层映射依赖它）
- 出站队列的定长与丢弃策略（spec §9.6）—— 当前 `std::sync::mpsc` 是无界的。在 Node 侧接入前不会暴露问题，但必须在下一个计划中解决
- 断线重连时的状态比对与有损降级（spec §9.4、§9.7）—— 这是客户端侧的职责，Rust 侧已提供所需的全部原语（`list` 拿当前 pane、`read` 拿当前内容）
