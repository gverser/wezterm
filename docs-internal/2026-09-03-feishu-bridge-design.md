# WezTerm ↔ 飞书 远程控制桥 · 设计

日期：2026-09-03
分支：`feishu-bridge`（基于 `session-sidebar`）

## 1. 背景与目标

同时进行多个项目、每个项目多个 AI CLI session 时，AI 干活需要时间，人不可能一直守在屏幕前。需要一条通道：**在手机上接收这些 session 的状态，并远程与它们对话**。

Claude Code 自带 Remote Control（手机 App / claude.ai 连本地 session），能力完整且零部署，但有两点不满足：

1. **只能控制 Claude Code 一种被控对象**。本设计要求通用 —— Codex、Gemini CLI、`npm run dev` 等任何跑在终端里的东西都能纳管。
2. **客户端锁定在 Claude App**。本设计要求收在已经常驻的 IM 里（飞书）。

### 范围

第一期交付 **L1**：

- 查看任意 session 的输出
- 向任意 session 发送消息
- 远程批准/拒绝权限请求
- 列出 session 及其运行状态
- **从 `sessions.json` 的预设条目新建 session**（不是任意命令）
- 结束 session

架构上为 L2（白名单操作）和 L3（完整远程 shell）留出扩展点，但第一期不实现。

**明确不在范围内**：执行 `sessions.json` 之外的任意命令；归档与历史检索（见 §13）。

## 2. 术语

| 术语 | 含义 |
|---|---|
| pane | WezTerm 的一个终端窗格，一个 AI CLI 进程跑在里面 |
| workspace | WezTerm 的工作区，sidebar 里一个 session 条目对应一个 |
| session | 用户视角的一个任务单元。在 `sessions.json` 里是一条预设，运行起来后对应一个 workspace 及其中的 pane |
| bridge 模块 | WezTerm 内新增的 Rust crate，订阅 mux 事件并暴露本地 IPC |
| bridge 进程 | 独立的 Node.js 进程，负责飞书对接 |
| 卡片 | 飞书消息卡片。一轮对话一张，底部带输入框，`value` 里携带 `pane_id` 作为路由依据 |

## 3. 总体架构

```
┌─ wezterm-mux-server.exe（常驻，无窗口）──────────────┐
│  Mux ── 所有 pane / workspace 真正活在这里            │
│   └─ wezterm-bridge：Mux::subscribe() + TCP 服务      │
└───────────┬──────────────────────┬───────────────────┘
            │ mux 协议              │ NDJSON / 127.0.0.1
            │                      │
   ┌────────┴────────┐    ┌────────┴──────────┐
   │ wezterm-gui     │    │ bridge (Node.js)  │──长连接──→ 飞书
   │ session sidebar │    │ 飞书官方 SDK       │←─────────  单聊
   │ 关掉不影响 session│   │ HTTP server (hook)│
   └─────────────────┘    └───────────────────┘
                                   ↑
                          Claude Code hooks（HTTP POST）
```

### 3.1 关键决策：mux-server 常驻

pane 必须活在 `wezterm-mux-server` 里，而不是 GUI 进程里。GUI 只是视图。

```lua
unix_domains = { { name = 'main', connect_automatically = true } }
default_domain = 'main'
```

- GUI 启动 → 自动连 mux-server；server 未运行则自动拉起（`serve_command` 默认 `wezterm-mux-server --daemonize`）
- 关闭 GUI → session 继续运行
- 重开 GUI → 自动连回，pane 与滚动历史原样还在

**Windows 支持已确认**：`daemonize.rs` 虽为 `#![cfg(unix)]`，但 `wezterm-mux-server/src/main.rs` 的 `#[cfg(windows)]` 分支用 `DETACHED_PROCESS` 标志 spawn 自身副本，功能完整。

### 3.2 组件职责

| 组件 | 职责 | 语言 |
|---|---|---|
| `wezterm-bridge` | 订阅 `MuxNotification`；按指令读写 pane、spawn、kill；暴露本地 TCP | Rust |
| bridge 进程 | 飞书长连接；pane 与卡片的映射；卡片渲染；接收 Claude Code hook 的 HTTP 请求 | Node.js |
| Claude Code hooks | 把语义级事件推给 bridge，并接收权限决策 | settings.json 配置 |

### 3.3 端口与凭据

系统中存在**两个本地服务**和**两个独立凭据**，不要混淆：

| | 监听方 | 端口（默认） | 协议 | 调用方 | 凭据 |
|---|---|---|---|---|---|
| **IPC 服务** | `wezterm-bridge`（Rust，mux-server 内） | 8788 | NDJSON over TCP | bridge 进程 | **IPC token** |
| **Hook 服务** | bridge 进程（Node.js） | 8787 | HTTP POST | Claude Code hooks | **Hook token** |

- **IPC token**：mux-server 启动时生成，写入 `bridge_token_path` 文件（权限限当前用户）。bridge 进程读文件后在首行 auth 消息中出示。**从不进入环境变量。**
- **Hook token**：bridge 进程启动时生成，写入 `$DATA_DIR/feishu-bridge-hook-token`，并需导出为环境变量 `BRIDGE_TOKEN` 供 Claude Code 的 hook 配置通过 `allowedEnvVars` 插值到 `Authorization` header。

两者**不可共用**：IPC token 若进入环境变量，任何被 AI 执行的子进程都能读到它，进而可以直接向 mux-server 下指令、绕过所有约束。分开是安全边界，不是冗余。

## 4. 关窗口语义的变更

### 4.1 现状与冲突

`wezterm-gui/src/termwindow/mod.rs:524` 的 `close_requested()` 当前语义是**关窗口 = 杀掉所有 tab，包括后台 workspace**（`kill_unattended_windows()`）。代码注释写明：

> not prompting is a request to skip the question, not to leave a headless process behind

在 pane 跑于 GUI 进程的模式下这是正确的 —— 不杀就变成孤儿进程。

但在 mux-server 模式下，pane 住在另一个进程里，"关窗口"与"结束 session"是两件不同的事，且两者都需要：关电脑前要全杀；出门前关 GUI 让 AI 继续跑要 detach。

### 4.2 变更：关窗口默认 detach，杀 session 是显式动作

采用终端复用器的标准心智模型：**关窗口 = detach；结束 session 是另一个动作**（tmux 的 `detach` 与 `kill-session`）。

| 场景 | 行为 |
|---|---|
| 连接 mux domain 时关窗口 | **直接 detach，不弹确认框**。session 继续在 mux-server 中运行 |
| 连接本地 domain 时关窗口 | **现有行为完全不变**：`close_prompt` 提示 + `kill_unattended_windows()` |
| 想真的结束全部 session | 新增 KeyAssignment `CloseAllSessions`，**无默认绑定**，从命令面板调用 |

不弹框不会让后台 session 被遗忘：sidebar 已经在每个条目右侧显示 terminal count，重开 GUI 一眼可见。

**曾考虑并否决的方案**：把 `confirm_close_sessions` 扩成「保持后台运行 / 全部关闭 / 取消」三选。否决原因是其底层 `overlay::confirm::run_confirmation` 硬编码两个按钮（按钮坐标、渲染、鼠标命中测试、键盘导航各一套），加第三个按钮需要把它泛化为 N 按钮；`overlay::selector` 也无法复用，它绑定了 lua `InputSelector` 的 KeyAssignment 与事件回调。这笔 TUI 改造的投入，换来的只是一个在标准心智模型下本就不必要的提示框。

`WindowCloseConfirmation::NeverPrompt` 在 mux domain 下同样是 detach —— 留下的不是孤儿进程，而是有主的后台 session。

## 5. 两条通道

不是一条通道，是两条，各司其职。

| | 出（AI → 用户） | 入（用户 → AI） |
|---|---|---|
| **HTTP hook**<br>Claude Code 专用，语义级 | `Notification`（`permission_prompt` / `idle_prompt` / `agent_needs_input` / `agent_completed`）<br>`Stop` → `last_assistant_message`<br>`SessionStart` / `SessionEnd` | `PermissionRequest` 的 `decision` |
| **mux 通道**<br>通用，任何 CLI | `PaneOutput`<br>`Alert`（Bell / 标题变更）<br>`PaneAdded` / `PaneRemoved` | `send_paste` / `key_down` |

### 5.1 分工原则

1. **权限批准走 hook，不模拟按键。** 原本需要从 TUI 文本识别弹窗、计算光标位置、发送方向键；改为一次 HTTP 请求/响应。
2. **用户消息走 mux `send_paste`。** hook 是单向观察者，无法反向注入输入。
3. **AI 回复优先取 `Stop` hook 的 `last_assistant_message`**（干净的完整 Markdown），`PaneOutput` 作为通用兜底 —— 非 Claude Code 的 CLI 只有这一条。

### 5.2 权限批准回路

```
Claude Code 要执行 rm -rf                   bridge                飞书
    │                                         │                    │
    ├─ PermissionRequest hook ──HTTP POST────→│                    │
    │  { tool_name, tool_input, tool_use_id } ├─ 卡片 + 按钮 ─────→│
    │  Header: X-Wezterm-Pane: 7              │                    │
    │        ⏳ 阻塞等待，最长 600 秒          │      用户点「拒绝」│
    │                                         │←───── 回调 ────────┤
    │←──── { hookSpecificOutput: {  ──────────┤                    │
    │        decision: "deny", ... } }        │                    │
    └─ 拒绝执行                                │                    │
```

HTTP hook 默认超时 600 秒（`UserPromptSubmit`、`PreModelSwitch`、`PostModelSwitch` 为 30 秒，`MessageDisplay` 为 10 秒），`timeout` 字段可自定义。

**超时是安全的**：hook 超时后回落到 Claude Code 默认权限流程（终端内正常弹窗），不会误批准，也不会卡死。

#### 请求的配对 id 由 bridge 自己生成

实测的 `PermissionRequest` 载荷字段（真实 Claude Code 发出，非文档）：

```
session_id, transcript_path, cwd, scratchpad_dir, prompt_id,
permission_mode, effort, hook_event_name, tool_name, tool_input,
permission_suggestions
```

**其中没有 `tool_use_id`** —— 官方文档列出了它，实际不存在。若以它为 key 登记挂起的请求，每条请求的 key 都是 `undefined`：只有一条在等时恰好能配上（第一次端到端验证就是这么蒙混过关的），**两条并发就会互相覆盖，一次点击会结掉不确定的那一条**。而多 session 并行正是本项目的前提，这个碰撞必然发生。

因此配对 id **由 bridge 自己生成**（uuid），不依赖载荷里的任何字段。它只用于把卡片上的点击和挂起的 HTTP 请求配对，本来就是 bridge 内部的事。

`permission_suggestions` 是文档未提及的字段，尚未利用。

### 5.3 pane 关联

hook 的 JSON 载荷中没有 WezTerm 的 pane id。通过 HTTP header 传环境变量解决。此处的 `$BRIDGE_TOKEN` 是 **Hook token**（见 §3.3），不是 IPC token：

```json
{
  "type": "http",
  "url": "http://127.0.0.1:8787/hook",
  "timeout": 600,
  "headers": {
    "X-Wezterm-Pane": "$WEZTERM_PANE",
    "Authorization": "Bearer $BRIDGE_TOKEN"
  },
  "allowedEnvVars": ["WEZTERM_PANE", "BRIDGE_TOKEN"]
}
```

`WEZTERM_PANE` 在 mux 层设置（`mux/src/domain.rs:482`），因此 mux-server 起的 pane 同样具备。

### 5.4 通用性边界

hook 通道是 Claude Code 独有的。其他 CLI 只有 mux 通道：能看输出、能发输入、能起停，但**没有语义级的"需要决策"信号，也没有远程权限批准**。

这是分层的自然结果，不是缺陷：mux 通道保底通用，hook 通道为支持它的工具做增强。将来其他 CLI 提供类似机制，按同样方式接入。

## 6. Rust 侧

### 6.1 新增 crate `wezterm-bridge`

```rust
Mux::get().subscribe(|notif| {
    match notif {
        MuxNotification::PaneOutput(id)   => mark_dirty(id),
        MuxNotification::PaneAdded(id)    => push_lifecycle(id, Added),
        MuxNotification::PaneRemoved(id)  => push_lifecycle(id, Removed),
        MuxNotification::Alert { pane_id, alert } => push_alert(pane_id, alert),
        _ => {}
    }
    true   // 返回 false 会取消订阅
});
```

### 6.2 指令集

bridge 进程 → Rust 侧，四个动作，全部有现成 API：

| 指令 | 实现 |
|---|---|
| 发消息给 AI | `Pane::send_paste(text)` + `Pane::key_down(Enter)` |
| 读增量 | `Pane::get_current_seqno()` + `get_changed_since(range, seqno)` + `get_lines(range)` |
| 新建 session | mux spawn，参数取自 `SessionSpawn { domain, cwd, args, env }` |
| 结束 session | `Mux::remove_pane` / `Mux::kill_window` |

### 6.3 挂载点与配置

`wezterm-mux-server` 启动流程中读取配置决定是否启动 bridge。新增配置项：

| 配置项 | 类型 | 默认 | 含义 |
|---|---|---|---|
| `bridge_enabled` | bool | `false` | 是否启动 bridge 的 IPC 服务 |
| `bridge_port` | u16 | `8788` | IPC 服务监听端口（**非** hook 服务的 8787，见 §3.3） |
| `bridge_token_path` | Option\<PathBuf\> | `$DATA_DIR/bridge-token` | IPC token 文件位置 |

### 6.4 前置重构：提取 `wezterm-sessions`

`wezterm-gui/src/sessions/` 的**数据模型与持久化**需提取为独立 crate，供 GUI 和 bridge 共同依赖 —— mux-server 看不到 GUI crate 的模块。

**提取边界**（不是整个模块）：

| 文件 | 去向 | 理由 |
|---|---|---|
| `tree.rs` | → `wezterm-sessions` | 纯数据模型，仅依赖 `config::keyassignment` 与 serde/std |
| `store.rs` | → `wezterm-sessions` | 纯文件 IO，仅依赖 `config::DATA_DIR` 与 serde_json |
| `mod.rs` 的 `clamp_scroll` / `visible_range` / `clamp_width` / `close_prompt` | **留在 GUI** | 侧边栏布局与提示文案，属于渲染关注点，bridge 用不到 |

`wezterm-gui/src/sessions/mod.rs` 保留上述 GUI 函数及其测试，并从新 crate re-export 数据类型，使 sidebar 现有 import 路径不变。

不采用"bridge 只读 `sessions.json` 文件"的松耦合方案：格式仍是同一份，只是失去编译期保证；`SessionSpawn` 增加字段时 bridge 会静默读不到，要到运行时才暴露。

## 7. IPC 协议

**NDJSON over `127.0.0.1` TCP**，每行一个 JSON 对象。

不使用 `wezterm-uds`：WezTerm 在 Windows 上用 AF_UNIX socket 文件，而 Node.js 在 Windows 上的 `net.connect({path})` 只接受 named pipe，不接受 AF_UNIX 文件。

选择本地 TCP 的收益：**零新依赖**（Rust 侧 `std::net::TcpListener`，Node 侧 `net` + `readline`），跨平台无差异，便于调试。

### 7.1 消息格式

Rust → Node（`wezterm-bridge/src/proto.rs` 的 `Outbound`）：

```json
{"t":"auth_ok"}
{"t":"pane_added","pane":9,"workspace":"session:6","title":"claude","cwd":"D:\\dev\\x","session":"casb","subscribed":false}
{"t":"pane_removed","pane":9}
{"t":"pane_title","pane":9,"title":"◑ Claude Code 修复编号"}
{"t":"pane_mirror","pane":9,"enabled":true}
{"t":"dirty","pane":7,"seqno":11820}
{"t":"lines","pane":7,"first_row":3140,"lines":["...","..."],"seqno":11820}
{"t":"alert","pane":7,"kind":"Bell"}
{"t":"panes","panes":[{"pane":7,"workspace":"session:6","title":"claude","cwd":"D:\\dev\\x","session":"casb","subscribed":true}]}
{"t":"error","message":"no such pane: 99"}
```

`pane_added` 和 `panes` 里的每一项是同一个 `PaneInfo`（`Outbound::PaneAdded` 是
newtype，serde 的内部 tag 把它摊平，所以线上格式仍是一层）。字段的意思：

| 字段 | 含义 |
|---|---|
| `workspace` | mux 工作区名，sidebar 起的 session 是 `session:<node_id>` |
| `session` | sidebar 里这个 session 的名字（"casb"），手开的 pane 为 `null` |
| `cwd` | pane 所在目录，本地路径写法 |
| `subscribed` | 用户是否要求镜像这个 pane |

**pane_added 不代表"有个 session 起来了"**。bridge 把**每个** pane 都报给客户端，
因为客户端要能把它们列成可选目标；值得播报的是 `pane_mirror`，那才是用户说"这个
我要看"。两者的区别是 §8 里那条"开一个 shell 敲条命令"不该惊动手机的由来。

第一个被订阅的 pane 是**服务启动的原因**，那条 `pane_mirror` 发出时还没有客户端连
上。客户端从连接后的 `list` 里按 `subscribed` 补上，并自己记住播报过谁 —— 断线重连
时 `list` 会把同样的状态再报一遍，那不是新消息。

`pane_title` 是标题变化。Claude Code 把当前在做的事写进终端标题，`/rename` 也改它；
只在 pane 创建那一刻取一次标题，得到的永远是 `cmd.exe`。

Node → Rust（`Inbound`）：

```json
{"t":"auth","token":"..."}
{"t":"send","pane":7,"text":"继续，用方案 B","submit":true}
{"t":"read","pane":7,"max_lines":200}
{"t":"spawn","node_id":12}
{"t":"kill","pane":7}
{"t":"list"}
```

`submit` 缺省为 `true`，`max_lines` 缺省为 200。`spawn` 与 `kill` 不产生直接回复：两者都是异步的，客户端从 `pane_added` / `pane_removed` 得知结果。

`dirty` 目前客户端不再理会：pane 里跑的是 TUI，屏幕上大半是分隔线、输入框和状态栏，
把它们转发到飞书没有意义。任务的结果由 Claude Code 的 Stop hook 给出，`read` 只剩
`!screen` 一个用途 —— 想看屏幕当下长什么样。

### 7.2 鉴权

此处使用的是 **IPC token**（见 §3.3），与 Hook token 是两个独立凭据。

监听地址固定 `127.0.0.1`。本机其他进程同样可连 localhost，因此必须鉴权：

- mux-server 启动时生成随机 token，写入 `bridge_token_path`，文件权限限制为当前用户
- bridge 进程读取该文件，连接后第一行必须是 `{"t":"auth","token":"..."}`
- 鉴权失败或首行不是 auth，立即断开

**IPC token 绝不导出为环境变量。** 它等价于向 mux-server 下达任意指令的权限；一旦进入环境，任何被 AI 启动的子进程都能读到并绕过全部约束。

### 7.3 单写者约束

**Rust 侧只有一个线程写 socket。** mux 通知、flush 定时器与命令循环都把消息交给它，不各写各的。

这不是风格问题：`TcpStream::try_clone` 给出的是独立句柄，多个线程并发 `writeln!` 会让两条消息在字节层交错，NDJSON 帧就此错位 —— 而这只在负载高时出现，平时测不出来。

**Node 侧写回 Rust 时受同样约束**：那边同样有多个来源要发指令（用户消息、卡片回调、定时任务），同样需要串行化。

## 8. 飞书侧

### 8.1 会话模型：单聊 + 卡片路由

本节原先设计为「话题群 + 一个 pane 一个话题」，实测后改为单聊。

**实测依据**（机器人单聊收到的 `im.message.receive_v1` 事件）：

```
chat_type : p2p
chat_id   : oc_...
thread_id : (none)   ← 单聊消息不带话题
```

话题是群聊特性（错误码 230071 的措辞也是「该消息所属的群聊不支持以话题形式回复」）。单聊里没有话题可用。

**替代方案：把路由信息放进卡片。**

```
sessions.json          mux                飞书单聊
─────────────          ───                ────────
session「重构」   →   pane 7        →   卡片（value 带 pane_id: 7）
session「测试」   →   pane 9        →   卡片（value 带 pane_id: 9）
```

每张卡片底部带一个[输入框组件](https://open.feishu.cn/document/feishu-cards/card-json-v2-components/interactive-components/input)，交互组件的 `value` 里携带 `pane_id`。**用户在哪张卡片上输入，回调就带回哪个 `pane_id`** —— 路由是显式的，比依赖话题上下文更准确。

选单聊而非话题群的理由：

| | 单聊 + 卡片路由 | 话题群 |
|---|---|---|
| 配置成本 | 零 | 建群、切话题模式、拉机器人 |
| 路由准确性 | 显式（卡片携带 pane_id） | 靠话题上下文 |
| 多 session 视觉隔离 | 卡片按时间线交错 | 各话题独立 |

**迁移成本很低**：若实际使用中刷屏严重，改成话题群只需把 `chat_id` 从单聊换成群 id，数据结构与卡片结构均不变。

### 8.2 卡片策略：一轮对话一张流式卡片

```
用户在某张卡片的输入框里发「继续，用方案 B」
    │
    ├─ 回调携带 pane_id → bridge → send 写入该 pane
    ├─ 创建 streaming_mode 卡片（普通消息，占 1 次配额）
    ├─ dirty 事件到达 → read 增量 → 流式追加进这张卡片
    └─ Stop hook 到达 → 用 last_assistant_message 定稿，并附上输入框
```

卡片结构：

```
┌────────────────────────────────┐
│ 项目A · 重构            pane 7 │  ← 标题带 session 身份
├────────────────────────────────┤
│ AI 的输出（流式追加）           │
├────────────────────────────────┤
│ [ 输入框                    ] │  ← value: { pane_id: 7 }
│ [ 发送 ]                        │
└────────────────────────────────┘
```

**为什么一轮一张卡**：飞书卡片上限 30KB，长 session 必然超出；而 `Stop` hook 恰好给出轮次边界。定稿后卡片不再更新，下一轮开新卡。

飞书 `streaming_config` 自带打字机效果参数（`print_frequency_ms` 默认 70ms、`print_step`、`print_strategy`），直接使用。

### 8.3 直接发文本的处理

用户不一定会用卡片上的输入框 —— 他可能直接在单聊底部打字。那条消息没有 `pane_id`。

规则：

1. 维护一个**当前目标 pane**（最后一次通过卡片交互或被选中的那个），直接发的文本路由到它
2. 文本以 `#<pane_id> ` 开头时，显式指定目标，并把当前目标切过去
3. 没有当前目标时，回一张列出全部 session 的卡片让用户选，不猜

不猜是原则：把消息发错 session 比不发更糟，尤其当它是一句「继续」而那个 session 正停在一个危险操作前。

### 8.4 配额分层

| 用途 | 通道 | 配额 |
|---|---|---|
| AI 输出内容 | 流式卡片更新 | 单卡片 10 次/秒，**流式模式下不触发 QPS 限制** |
| 权限请求卡片 | 普通消息 | 占用向同一用户的 5 QPS |
| 任务完成 / session 起停 | 普通消息 | 占用向同一用户的 5 QPS |

**向同一用户发消息限 5 QPS**。纪律：内容一律走流式卡片更新，普通消息只用于低频事件。这样即使十个 session 并发，普通消息也只是零星几条。

### 8.5 本地节流

即使流式更新不受 QPS 限制，`PaneOutput` 在终端刷屏时每秒可触发上百次，仍需在 bridge 本地合并（Rust 侧已实现 200ms flush，只发 dirty 信号）：

- 收到 dirty 后才 `read` 拉内容，不主动轮询
- 一次 flush 内的多次变更合并为一次流式追加
- 后台 session（用户最近未交互）降频至 1s

## 9. 状态与可靠性

### 9.1 核心原则

> **bridge 不持有任何与输出量成正比的状态。**
> 只记水位线，内容随用随取；所有队列定长，溢出即有损降级。

理由：`MuxNotification::PaneOutput(PaneId)` 只携带 pane id，不携带内容。内容始终在 mux 的 scrollback 中，而 scrollback 由 WezTerm 的 `scrollback_lines` 配置管理，本身有界。bridge 再存一份等于同一数据存两遍，且更差 —— 高频写入、可能过时、需要自行维护轮转与清理。

### 9.2 每个 pane 的常驻状态

```js
{ paneId, threadId, lastSeqno, lastRow, dirty }   // ≈ 60 字节
lastStopMessage                                    // ≤ 8 KB（截断）
```

**内存上限 = pane 数 × 8KB**。100 个 session 约 800KB，**与断线时长、输出量、运行天数无关**。

### 9.3 断线期间的四类事件

| 事件 | 处理 | 理由 |
|---|---|---|
| `PaneOutput` | **不攒**，只置 `dirty = true` | 内容在 scrollback，重连后 `get_changed_since(lastSeqno)` 一次取全 |
| `PaneAdded` / `PaneRemoved` | **不攒**，重连后**比对状态** | 映射表 ∖ 当前 pane 列表 = 期间关闭的；当前列表 ∖ 映射表 = 期间新建的。用状态差代替事件重放 |
| `PermissionRequest` | **不攒** | 它是阻塞中的 HTTP 请求。飞书不通则推不出卡片，hook 超时回落终端弹窗。过期的权限请求留存有害无益 |
| `Stop` | **每 pane 只留最后一条**，截断 8KB | mux 中无法重建（`last_assistant_message` 为 hook 独有）；用户重连后只关心最后一轮 |

### 9.4 重连流程

```
1. 读映射表，与当前 pane 列表比对 → 处理期间新建/关闭的 pane
2. 对每个 dirty 的 pane：get_changed_since() → get_lines()
3. 发送「已重连」+ 最后一条 Stop 消息 + 当前状态
4. 若丢失行数超阈值 → 「期间产生大量输出，已跳过」+ 最后 N 行
```

### 9.5 持久化：单聊方案下不需要

原设计（话题群）要求持久化 `pane → thread_id` 映射，因为丢失它会让同一 session 出现两个话题、历史永久断裂。**改成单聊 + 卡片后，这个后果消失了。**

单聊下需要记住的只有两样，且两样丢失都无害：

| 状态 | 丢失后果 |
|---|---|
| `pane → 当前卡片的 message_id` | 当前那一轮不再更新，下一轮开新卡 —— 本来就是一轮一张 |
| 当前目标 pane（§8.3） | 用户下次直接发文本时需重新指定一次 |

两者都是**当前轮次的瞬态**，不是跨重启的身份。而卡片自身携带 `pane_id`，即使 bridge 重启，旧卡片上的输入框仍能正确路由 —— **路由信息存在飞书那侧，不在我们这里**。

所以 bridge 进程是无状态的：重启后重新订阅、重新 `list`，就回到了可工作状态，不读任何落盘文件。

`lastSeqno` 同样不持久化：重启后不补发历史，直接对齐当前状态。

**若将来迁回话题群**，持久化就重新变得必要 —— 那时 `thread_id` 是跨重启的身份，丢不得。

### 9.6 出站队列

飞书持续返回 429 或超时会导致待发请求积压。每个 pane 一个**定长发送队列**，满则**丢弃最旧的** —— 对终端输出而言最新的才有价值。

### 9.7 边界情况

- **断线过久，输出滚出 scrollback**：`StableRowIndex` 失效（`Pane::get_lines` 文档注明 "can be invalidated if the scrollback is busy"）。丢失行数超阈值时不补发，改为有损降级提示 + 当前尾部 N 行。
- **alternate screen 切换**（vim、less 等全屏程序）：同样导致索引失效，复用同一套降级逻辑。

## 10. 失败模式

| 故障 | 影响 | 处理 |
|---|---|---|
| 飞书长连接断开 | 手机上收不到更新 | SDK 自动重连；按 §9.4 重连流程恢复 |
| **bridge 进程挂掉** | **session 完全不受影响** | 权限请求 hook 超时回落终端弹窗；重启后按 §9.4 恢复 |
| mux-server 挂掉 | session 全部丢失，真故障 | bridge 检测到 TCP 断开后向飞书告警。这是少数必须打扰用户的情况 |
| 飞书 API 429 | 发送延迟 | 定长队列 + 丢弃最旧 |

**关键性质**：bridge 是旁路，不在关键路径上。它挂掉后 AI 照常工作，只是手机上看不到；回到电脑前一切正常。不能让一个 IM 机器人的故障拖垮开发环境。

## 11. 安全

- **IPC**：绑定 `127.0.0.1`，token 鉴权（§7.2）。本机其他进程可连 localhost，token 不可省。
- **可执行范围**：远程只能启动 `sessions.json` 中预先定义的条目，不能执行任意命令。这是 L1 与 L3 的分界线。
- **飞书账号即入口**：能以你的身份登录飞书的人就能操作这些 session。账号安全等同于机器的部分控制权。第一期依赖飞书自建应用的可见范围控制（仅自己）。
- **数据出境**：session 的输出内容会经过飞书服务器。涉密项目不应纳入。

## 12. 测试策略

| 层 | 方式 |
|---|---|
| `wezterm-sessions` 提取 | 现有测试原样通过（`cargo nextest run -p wezterm-sessions`） |
| `wezterm-bridge` 事件订阅 | 单元测试：构造 `MuxNotification` 验证 dirty 标记与生命周期事件 |
| `wezterm-bridge` 指令执行 | 单元测试：mock `Pane`，验证 `send_paste` / `get_changed_since` 调用 |
| IPC 协议 | 契约测试：两侧共用一组 JSON 样例，Rust 侧验证序列化，Node 侧验证解析 |
| bridge 状态机 | Node 单元测试：断线/重连/映射表持久化/队列溢出丢弃 |
| 飞书对接 | mock 飞书 SDK，验证配额分层（内容不走普通消息）与节流窗口 |
| 端到端 | 手动清单：开 session → 手机对话 → 权限批准 → 关 GUI → 手机继续 → 重开 GUI → 状态一致 |

遵循仓库既有约定：`cargo nextest run --all --no-fail-fast`，`cargo +nightly fmt --all`。**Windows 上必须在 PowerShell 而非 Git Bash 中构建**（见仓库 CLAUDE.md：`.cargo/config.toml` 强制 `+crt-static`，OpenSSL 需 Strawberry Perl；换 shell 会触发约 25 分钟全量重建）。

## 13. 未来扩展

- **L2**：白名单操作。在 `sessions.json` 之外增加一组预定义动作（跑测试、`git status`、重启服务），仍非任意命令。
- **L3**：完整远程 shell。需要额外的身份验证与审计设计，安全模型与 L1/L2 不同。
- **归档**：回看历史 session 的完整对话。**这是功能，不是可靠性机制** —— 需独立设计（存储位置、保留期、检索方式），不与发送路径耦合。混在一起通常两件事都做不好。
- **其他 CLI 的语义通道**：若 Codex、Gemini CLI 等提供类似 hook 机制，按 §5 的分工接入。

## 14. 已确认的技术事实

本设计依赖的关键事实均已在源码或官方文档中验证：

| 事实 | 出处 |
|---|---|
| `MuxNotification` 含 `PaneOutput` / `PaneAdded` / `PaneRemoved` / `Alert` | `mux/src/lib.rs:57-98` |
| `Mux::subscribe(F: Fn(MuxNotification) -> bool)` | `mux/src/lib.rs:700` |
| `Pane` 提供 `get_current_seqno` / `get_changed_since` / `get_lines` / `send_paste` / `key_down` | `mux/src/pane.rs:179-260` |
| `WEZTERM_PANE` 在 mux 层设置 | `mux/src/domain.rs:482` |
| Windows `--daemonize` 用 `DETACHED_PROCESS` | `wezterm-mux-server/src/main.rs` |
| `sessions/` 零 GUI 依赖 | `wezterm-gui/src/sessions/{mod,tree,store}.rs` 的 import |
| `SessionTree` 持久化于 `$DATA_DIR/sessions.json` | `wezterm-gui/src/sessions/store.rs:12` |
| 飞书长连接可接收事件**与卡片回调**，均免公网 | [使用长连接接收回调](https://open.feishu.cn/document/event-subscription-guide/callback-subscription/step-1-choose-a-subscription-mode/configure-callback-request-address) |
| 飞书官方 SDK 无 Rust 版（Java/Python/Go/Node.js） | [Server SDK](https://open.feishu.cn/document/server-docs/server-side-sdk) |
| 群内发消息 5 QPS 共享；流式卡片单卡 10 次/秒且不触发 QPS 限制 | [流式更新卡片](https://open.feishu.cn/document/cardkit-v1/streaming-updates-openapi-overview) |
| HTTP hook 默认超时 600 秒；`PermissionRequest` 通过 `decision` 字段决策；超时回落默认权限流程 | [Hooks reference](https://code.claude.com/docs/en/hooks) |
