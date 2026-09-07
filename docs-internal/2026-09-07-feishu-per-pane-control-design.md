# 飞书 per-pane 控制 设计

> 承接 `2026-09-03-feishu-bridge-design.md`。那份 spec 描述的 bridge 是**全局**的：
> Node 客户端一连上就收到所有 pane 的事件。这份 spec 把它改成**按 pane 订阅**，
> 并把飞书服务（Node 进程）的起停交给 wezterm 自己管。

## 1. 要解决的问题

现在要让飞书跑起来，得手动开一个终端敲 `node src/main.js`，关机就没了；而且一旦
起来，**所有** pane 的输出都会推到飞书上——包括那些只是开着晾在那儿的 shell。

目标是三件事：

1. 每个 pane 自己决定要不要上飞书，互不牵连
2. 第一个 pane 打开时自动把服务拉起来，最后一个关掉时自动收掉
3. 有个地方能看见"服务到底起没起"，并且能一键关掉

## 2. 硬约束：GUI 和 mux-server 是两个进程

bridge 活在 mux-server 里（`wezterm-mux-server/src/main.rs:243`），而按钮在 GUI 上。
GUI 按下按钮后，没有现成的通道告诉 bridge "订阅这个 pane"。

排除掉的两条路，记下来免得以后重走：

- **user vars 走不通**。`Pane` trait 上只有 `copy_user_vars()`（只读，
  `mux/src/pane.rs:270`），codec 里也没有对应的 PDU。它是给终端程序通过 OSC 转义序列
  **从 pane 内部往外**传值用的，方向反了。
- **GUI 直连 bridge 的 8788** 能work，但要让 GUI 读 IPC token、维护第二条连接，
  而且远程 mux 场景直接失效。订阅是 pane 的属性，让它绕过 mux 协议是本末倒置。

所以走协议：新增一个 PDU，和 `SetPaneZoomed` 同级。

## 3. 架构总览

```
 GUI                          mux-server                        Node 客户端
 ───                          ──────────                        ───────────
 标签栏图标 ──点击──▶ 面板
                  │
                  │ SetPaneFeishu{pane_id, enabled}
                  └──────────────▶ sessionhandler
                                        │
                                        ▼
                                   Mux::set_pane_feishu()
                                        │
                                        ├─▶ MuxNotification::PaneFeishuChanged
                                        │        │
                                        │        ├─▶ bridge: 只对订阅的 pane 发 dirty
                                        │        │
                                        │        └─▶ 服务管理器: 起 / 停 Node
                                        │
                                        └─▶ 回传给所有 GUI 客户端，图标状态同步
```

**订阅状态的唯一真相在 Mux 里**。bridge 和服务管理器都是它的读者，GUI 上的图标只是
它的投影。这样多个 GUI 窗口看到的状态天然一致，不需要额外同步。

## 4. 协议扩展

### 4.1 新 PDU

`codec/src/lib.rs`，照 `SetPaneZoomed`（`:860`）的样子：

```rust
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct SetPaneFeishu {
    pub pane_id: PaneId,
    pub enabled: bool,
}
```

在 `pdu!` 宏里注册（`:450`），取下一个空闲编号。**`CODEC_VERSION` 从 45 升到 46**
（`:444`）。

### 4.2 推送 PDU：两个方向不是一回事

`SetPaneFeishu` 只解决"GUI 告诉服务端"。反方向——服务端状态变了要让**所有** GUI
窗口知道——是另一条路，而且不能省：`MuxNotification` 是**进程内**的，GUI 和
mux-server 各有独立的 `Mux` 实例，服务端 notify 一下客户端是听不见的。

照 `PaneFocused` 那条现成的路走（codec `:495` → `dispatch.rs:169` 编码 →
`clientpane.rs:217` 应用到本地状态）：

```rust
/// 服务端 → 客户端：某个 pane 的订阅状态变了。
/// 变更可能来自任何一个 GUI 窗口，所以每个窗口都要被告知。
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct PaneFeishuChanged {
    pub pane_id: PaneId,
    pub enabled: bool,
}

/// 服务端 → 客户端：飞书服务本身的状态。
#[derive(Deserialize, Serialize, PartialEq, Debug)]
pub struct FeishuServiceStatus {
    pub state: FeishuState,
    /// 失败时带上原因（Node 的最后一行 stderr），面板里要显示。
    pub detail: Option<String>,
}

#[derive(Deserialize, Serialize, PartialEq, Debug, Clone, Copy)]
pub enum FeishuState {
    Stopped,
    Starting,
    Connected,
    Failed,
}
```

于是一共三个新 PDU，`CODEC_VERSION` 只升一次。

`dispatch.rs` 里加两个 `Item::Notif(...)` 分支；GUI 侧收到后更新本地 `Mux` 的
`feishu_panes` 和服务状态缓存，并 `invalidate` 让标签栏图标重画。

### 4.3 版本不兼容是有意的

升版本意味着旧 mux-server 和新 GUI 互相拒绝连接（`verify_version_compat`）。这不是
副作用而是要的效果：旧 server 不认识这个 PDU，连上了也只会让订阅静默失效，还不如
明着报错。部署时两个二进制一起换、mux-server 重启一次即可。

### 4.3 服务端处理

`wezterm-mux-server-impl/src/sessionhandler.rs`，照 `Pdu::SetPaneZoomed`（`:546`）：

```rust
Pdu::SetPaneFeishu(SetPaneFeishu { pane_id, enabled }) => {
    spawn_into_main_thread(async move {
        catch(
            move || {
                let mux = Mux::get();
                // 存在性检查不能省：pane 可能在客户端发出请求之后、
                // 请求到达之前就没了。
                mux.get_pane(pane_id)
                    .ok_or_else(|| anyhow!("no such pane {}", pane_id))?;
                mux.set_pane_feishu(pane_id, enabled);
                Ok(Pdu::UnitResponse(UnitResponse {}))
            },
            send_response,
        )
    });
}
```

### 4.4 客户端

`wezterm-client/src/client.rs`，在 rpc 列表里加一行（`:1355` 附近）：

```rust
rpc!(set_pane_feishu, SetPaneFeishu, UnitResponse);
```

## 5. mux 侧：订阅状态

### 5.1 存储

`mux/src/lib.rs` 的 `Mux` 结构加一个字段，和现有的 `num_panes_by_workspace` 同级：

```rust
    /// Panes the user has asked to mirror to Feishu. Not persisted: a pane that
    /// goes away takes its subscription with it, which is what you want -- the
    /// next pane to reuse that id is a different pane.
    feishu_panes: RwLock<HashSet<PaneId>>,
```

存取：

```rust
    pub fn set_pane_feishu(&self, pane_id: PaneId, enabled: bool) {
        let changed = {
            let mut set = self.feishu_panes.write();
            if enabled { set.insert(pane_id) } else { set.remove(&pane_id) }
        };
        // 只在真的变了时才通知，否则重复点击会让服务管理器反复起停。
        if changed {
            self.notify(MuxNotification::PaneFeishuChanged { pane_id, enabled });
        }
    }

    pub fn pane_has_feishu(&self, pane_id: PaneId) -> bool {
        self.feishu_panes.read().contains(&pane_id)
    }

    pub fn feishu_pane_count(&self) -> usize {
        self.feishu_panes.read().len()
    }
```

### 5.2 pane 消失时清理

`remove_pane_internal` 里同步移除，并且**要发通知**——否则最后一个订阅的 pane 被关掉
时，服务管理器不知道该收工了：

```rust
    // 在 remove_pane_internal 中
    if self.feishu_panes.write().remove(&pane_id) {
        self.notify(MuxNotification::PaneFeishuChanged { pane_id, enabled: false });
    }
```

### 5.3 新通知

`MuxNotification` 枚举加一项：

```rust
    PaneFeishuChanged { pane_id: PaneId, enabled: bool },
```

**所有 match 它的地方都要处理**。GUI 侧至少三处（`termwindow/mod.rs` 的订阅过滤器、
`TermWindowNotif` 的 match、frontend 的 match）；漏掉会编译不过，这是好事。

⚠️ **`MuxNotification::PaneFeishuChanged` 和 `Pdu::PaneFeishuChanged` 同名，但完全
不是一回事**，实现时极易搞混：

```
mux-server 进程                          GUI 进程
──────────────                          ────────
Mux::set_pane_feishu()
  └─ notify(MuxNotification::PaneFeishuChanged)   ← 进程内
       ├─ bridge 订阅它，更新过滤
       ├─ FeishuService 订阅它，决定起停
       └─ dispatch.rs 订阅它，转成 ──▶ Pdu::PaneFeishuChanged   ← 跨进程
                                          └─ 客户端收到，更新本地 Mux 的 feishu_panes
                                               └─ 本地再 notify 一次（进程内）
                                                    └─ 标签栏图标重画
```

也就是说**同一个语义事件在两个进程里各 notify 一次**，中间靠 PDU 摆渡。客户端侧那次
本地 notify 不能省——图标是靠它才知道要重画的。

## 6. bridge 侧：按订阅过滤

`wezterm-bridge/src/state.rs` 的 `PaneState` 加 `subscribed: bool`，默认 `false`。

`service.rs` 的两处改动：

1. 订阅 `MuxNotification::PaneFeishuChanged`，更新对应 pane 的 `subscribed`
2. **dirty 刷新时跳过未订阅的 pane**——这是省资源的关键：未订阅的 pane 连
   `get_changed_since` 都不用调

`Outbound::PaneAdded` / `PaneRemoved` 仍然照发（Node 侧需要知道有哪些 pane 存在，
才能在飞书上列出可选项），但 `dirty` 只对订阅的 pane 发。

## 7. 服务生命周期

### 7.1 由谁管

mux-server。它和 pane 的生命周期天然一致，而且只有一个实例——GUI 可能开好几个窗口，
让它们抢着管一个子进程会很难看。

新模块 `wezterm-mux-server/src/feishu.rs`（或放进 `wezterm-bridge`）：

```rust
/// 飞书服务进程的看管者。
///
/// 起停都由订阅数驱动：0 → 1 时拉起，1 → 0 时收掉。
pub struct FeishuService {
    child: Mutex<Option<std::process::Child>>,
}
```

订阅 `MuxNotification::PaneFeishuChanged`，每次变更后看 `mux.feishu_pane_count()`：

- `0 → 非0`：spawn
- `非0 → 0`：kill

### 7.2 不能搭 `MuxNotification::Empty` 的便车

直觉上"没有 pane 了就关服务"应该听 `Empty`，但**那条通知现在可能永远不来**：
`prune_dead_windows` 里被 `retain_when_empty` 的空窗口挡住了
（`mux/src/lib.rs:961`，见 `spawn_initial_tab` 那次改动）。订阅数是自洽的判断依据，
不依赖窗口状态。

### 7.3 四个状态怎么判定

| 状态 | 判据 |
|---|---|
| `Stopped` | 没有子进程 |
| `Starting` | 进程起了，但 bridge 上还没有客户端连进来 |
| `Connected` | bridge 上有客户端连着 |
| `Failed` | `try_wait()` 发现进程已经退出 |

**"Node 连上飞书了没有"这件事 wezterm 无从知道**——那是 Node 和飞书服务器之间的事，
中间还隔着长连接握手。能观测到的只有"Node 连上了 bridge"，所以 `Connected` 的判据
就是它。Node 连上 bridge 却连不上飞书的情况（凭据过期、网络不通），由 Node 自己的
日志负责，面板不假装知道。

这也意味着 bridge 需要往外暴露一个"当前有几个客户端"的读法，`service.rs` 里加个
计数即可。

### 7.4 spawn 的细节

```rust
let mut cmd = std::process::Command::new(&config.feishu_command[0]);
cmd.args(&config.feishu_command[1..]);
// IPC token 绝不进环境：子进程里跑的任何东西都能读到自己的环境。
// Node 客户端自己从 token 文件读，路径是它的默认约定。
cmd.stdout(std::process::Stdio::null());
cmd.stderr(log_file);   // 服务自己的日志，出问题要能查
```

**Windows 上要加 `CREATE_NO_WINDOW`**，否则每次起服务都会闪一个控制台窗口。

### 7.5 收尸

- 正常路径：订阅归零 → `child.kill()` → `child.wait()`
- mux-server 退出：`Drop` 里再 kill 一次，避免留下孤儿进程
- **服务自己崩了**：`try_wait()` 发现进程没了，把状态标成"已停止"并让 GUI 看到；
  不自动重启——反复崩溃时自动重启只会刷屏

## 8. GUI：图标与面板

### 8.1 标签栏图标

`wezterm-gui/src/tabbar.rs` 加一个 `TabBarItem::FeishuButton`，位置在
`RightStatus`（`:676`）左边、`NewTabButton`（`:614`）右边。

四种视觉状态，靠图标颜色区分（不用文字，标签栏寸土寸金）：

| 状态 | 含义 |
|---|---|
| 暗色 | 服务没起，当前 pane 也没订阅 |
| 暗色 + 角标 | 服务起着，但当前 pane 没订阅（别的 pane 订了） |
| 亮色 | 当前 pane 订阅中，服务正常 |
| 黄色/闪烁 | 服务正在启动（Node 起来到连上飞书有几秒） |

### 8.2 面板

点击图标 → `set_modal`（`termwindow/mod.rs:2034`）弹出。照
`termwindow/paneselect.rs` 或 `palette.rs` 的样子实现 `trait Modal`。

内容：

```
  飞书服务

  状态    ● 已连接                     [关闭服务]

  当前 pane  #7  Claude Code           [关闭飞书]

  其他订阅中的 pane
    #3  casb
    #12 wezterm
```

- **状态行**：未启动 / 启动中 / 已连接 / 已停止（崩溃）
- **[关闭服务]**：直接 kill Node，所有订阅一并清掉——这是"我现在不想被打扰"的按钮
- **当前 pane 的开关**：发 `SetPaneFeishu`
- **其他 pane 列表**：只读，让用户知道自己开了哪些，避免忘了关

面板不直接向服务端查询任何东西，它读的全是本地状态：订阅列表来自 GUI 侧 `Mux` 的
`feishu_panes`（由 `PaneFeishuChanged` 推送同步），服务状态来自本地缓存（由
`FeishuServiceStatus` 推送同步）。这样面板的绘制是纯本地的、没有异步等待，打开就是
最新的；代价是**状态有一个 RTT 的滞后**——点了开关到图标变色之间隔着一次往返。本地
回环上这不到一毫秒，不值得为它做乐观更新（乐观更新反而会在服务端拒绝时闪烁）。

### 8.3 键盘

Modal 的 `key_down` 里：`Esc` 关闭面板，`Space`/`Enter` 切换当前 pane 的订阅。
不给默认快捷键——这不是高频操作。

## 9. 配置

`config/src/config.rs`：

```rust
    /// 飞书服务的启动命令。为空则功能整体关闭（标签栏图标也不显示）。
    #[dynamic(default)]
    pub feishu_command: Vec<String>,
```

用户的 `wezterm.lua`：

```lua
config.feishu_command = { 'node', 'D:/dev/remote-controll/src/main.js' }
```

**默认为空**意味着上游用户和没配过的场景完全不受影响，标签栏也不会多出一个看不懂的
图标。

## 10. 失败模式

| 情况 | 行为 |
|---|---|
| `feishu_command` 没配 | 图标不显示，PDU 仍然可发（无害），服务永不启动 |
| Node 起不来（路径错） | 状态显示"已停止"，面板里给出 stderr 的最后一行 |
| Node 起来了但连不上飞书 | 服务侧的问题，状态停在"启动中"；Node 自己的日志里有原因 |
| 订阅时 pane 已消失 | 服务端返回 `no such pane`，GUI 侧忽略（图标本来也会跟着 pane 一起消失）|
| 旧 mux-server | 版本检查直接拒绝连接，报错明确 |
| 多个 GUI 窗口同时点 | 状态在 mux 里，通知会广播给所有客户端，图标自动同步 |

## 11. 不做的事

- **不持久化订阅状态**。pane 没了订阅就没了；重开 wezterm 需要重新点一次。
  持久化要处理"pane id 复用"和"session 恢复"，收益不抵复杂度。
- **不自动重启崩溃的服务**。
- **不做 per-pane 的飞书账号/群组**。一个 bot 一个会话，够用。

## 12. 实施顺序

1. 协议层（PDU + 版本号 + 两端）——可以先用日志验证消息通了
2. Mux 状态 + 通知——单元测试可覆盖 `set_pane_feishu` 的去重逻辑
3. bridge 过滤
4. 服务生命周期
5. GUI 图标
6. GUI 面板

1-4 完成后功能就可用了（只是没有 UI，得用 `wezterm cli` 之类触发）；5-6 是让它好用。
