# 变更记录

从上游 wezterm 分叉之后自己开发的内容。分叉点是 `78cd82dbb`
（`config: Replace winapi with windows-sys (#8098)`），在那之前的历史都属于上游，
不在这里记。

一共做了三件事，互相衔接：

1. **会话侧边栏** —— 窗口左侧一棵保存好的会话树，每个条目绑一个 mux workspace，
   照 MobaXterm 的会话面板做的。
2. **飞书桥** —— 一条本地 IPC，把 mux 里的 pane 暴露给外部客户端，好让多项目、
   多会话的工作能在手机上跟进和指挥。
3. **飞书客户端** —— 桥另一头的 Node 进程，住在另一个仓库
   `D:\dev\remote-controll`。它的提交在下面用 **`[client]`** 标出；那条功能横跨
   两个仓库，只记一半会看不懂。

设计文档在 `docs-internal/`：

- `2026-09-03-feishu-bridge-design.md` —— 整体设计与 IPC 协议
- `2026-09-03-feishu-bridge-plan-rust.md` —— Rust 侧实现计划
- `2026-09-07-feishu-per-pane-control-design.md` —— 按 pane 订阅与服务生命周期

---

## 2026-09-08

- **`cbfeebf31` gui: 关闭 mux 窗口时，mux 里那个窗口也得关掉**
  关闭窗口后会立刻自己再开一个，得关两次，第二次还会留下一个没有窗口的进程。
  两个原因：detach 分支只关了 OS 窗口没动 mux 窗口，而 `reconcile_workspace`
  的职责就是给活跃 workspace 里每个 mux 窗口配一个 GUI 窗口，于是当场重建；
  以及 `spawn_initial_tab = false` 建的空 sidebar 窗口带着 `retain_when_empty`，
  它的 GUI 窗口被挪去显示 session 之后，这个标记压住了 mux 的 `Empty` 通知，
  进程就永远不退。

## 2026-09-07

- **`748cd850f` bridge: pane id 不是名字，pane_added 也不是新闻**
  面板显示的编号和飞书上的对不上 —— 面板打印的是 GUI 进程本地的 pane id，
  而桥活在 mux-server 里报的是服务端的 id。新增 `Pane::feishu_pane_id()`，
  `ClientPane` 覆写成返回 `remote_pane_id`。同时 `PaneInfo` 补上 session 名、
  cwd 和订阅状态，新增 `pane_title`（标题会变，只在建 pane 那刻取一次没用）
  和 `pane_mirror`（值得播报的是用户打开镜像，不是 pane 出现），连接时不再
  重放 `pane_added`。
- **`[client] 1d95e1f` feishu: 说清楚是哪个窗口，而且只在确实是一个时才说**
  飞书消息带上项目名和终端标题；`session up` 只在开启镜像时发一次；停掉逐行
  推流，卡片改由 Stop hook 的最终回答生成 —— TUI 屏幕上大半是分隔线、输入框
  和状态栏，转发过去没有意义。
- **`73a45908d` feishu: 面板里的 pane 按会话来命名**
  六个镜像中的 pane 全叫 cmd.exe，等于没有列表。
- **`3e31926e1` keys: 没有 pane 的窗口也该能响应快捷键**
  键路径在没有活跃 pane 时直接返回，空窗口里所有快捷键都失效 —— 包括那些
  专门用来脱离这个状态的。
- **`ca7ce9006` feishu: 做一个面板，因为一个字形说不了两件事**
  标签栏图标要同时表达「服务起没起」和「这个 pane 在不在镜像」，一个小菱形
  扛不住，也没法看见早先打开又忘掉的 pane。
- **`fd5a79c0a` feishu: 什么都不许比进程活得久**
  mux-server 被强杀时 Node 服务会留下来，握着一条没人看得见也停不掉的连接。
  改用 Windows job object（`KILL_ON_JOB_CLOSE`），连孙进程一起带走。
- **`68223a1c1` feishu: 一个一个 pane 地镜像，服务按需启动**
  桥原本是全有或全无，服务还得手动开。改成每个 pane 自己选，第一个打开时
  拉起服务，最后一个关掉时收工。订阅状态存在 mux 里 —— 桥、服务管理器和
  GUI 图标三个读者只有在那里才能取得一致。
- **`7ff73e157` docs: 记下 Windows 上会咬人的构建规则**

## 2026-09-04

- **`8d10fc105` bridge: 不只是发文本，也要能发按键**
  选择列表和确认框是 CLI 最需要回答的时候，纯文本答不了。
- **`91d97a0e9` docs: PermissionRequest 并不带 tool_use_id**
- **`[client] 8931873` hooks: 配对用的 id 得我们自己造**
  真实的 PermissionRequest 没有自己的 id，按缺失字段做 key 会让所有请求撞成
  同一个，一次点击就结算掉碰巧找到的那个。
- **`[client] d6abf14` feishu: 别把 TUI 说的话弄丢，也让它看得见**
- **`[client] 7715998` feishu: `!key` 发一个按键**
- **`[client] 93b49fd` hooks: 把决定留个记录**
  toast 一秒就没了，事后回看批准过什么才是要记录的理由。
- **`[client] fa60618` hooks: 按钮直接放在 body 里**
  schema 2.0 去掉了 action 容器，包起来会被 200861 拒绝。
- **`[client] 23c3b2a` main: hook 服务与桥、飞书一起跑**
- **`[client] 0814b5e` hooks: 一个事件显示什么，一个回答返回什么**
- **`[client] d9b0034` hooks: 问一次工具调用能不能放行的那张卡片**
- **`[client] 4fcf728` hooks: 等人回答的权限请求**
- **`[client] 741812d` hooks: 接收 Claude Code 事件的本地服务**
- **`[client] 46c7d7a` docs: hook 集成的计划**
- **`[client] 87ab9ce` main: 把正在发生的事说出来**
- **`[client] 63ffda3` main: 察觉 websocket 已经聋了**
  连接被中途悄悄掐断后两端仍显示 Established，发出去的消息全无声消失。

## 2026-09-03

- **`e7e3abd44` docs: 飞书这侧从话题群改为单聊**
- **`cd264e730` docs: 让 spec 的协议章节与实际做出来的东西一致**
- **`8a98196f6` bridge: 从客户端启动和结束会话**
  只能启动 `sessions.json` 里已有的条目 —— 客户端挑的是节点，不是命令，
  这是它和远程 shell 的分界线。
- **`a09c43730` gui: 关闭窗口不再等于结束会话**
  连到 mux 域时 pane 属于服务端，别的客户端可能正在跟它说话，用户也还会回来。
  所以可分离的域只关窗口，像 tmux 那样；要结束是 `CloseAllSessions`，
  且不给默认按键。
- **`98a58c3c5` mux-server: 配置里开了就把桥起起来**
- **`bf4d66180` bridge: 把客户端消息翻译成 mux 操作**
- **`43b70856e` bridge: 本地监听与它的令牌**
  socket 在回环上，本机任何进程都能连，所以连上本身证明不了什么。第一行必须
  证明客户端读到了只有本用户能读的文件，比较用常数时间，免得靠计时一个字节
  一个字节猜出来。
- **`091fcfdc0` bridge: 线上协议**
  NDJSON，`Inbound` / `Outbound` 两个枚举。
- **`d5b48bc3c` bridge: 不随输出增长的 pane 状态**
  `PaneOutput` 只带一个 pane id，内容本来就在 scrollback 里，所以一个 dirty
  位加一个 seqno 水位就够了 —— 链路断一分钟还是断几小时，内存都是平的。
- **`8523909bd` sessions: 把会话树和它的持久化抽成 wezterm-sessions**
  mux-server 要按请求启动会话就得读到会话列表，而它看不见住在 wezterm-gui
  里的模块。
- **`8f9823e53` docs: Rust 侧的实现计划**
- **`f574854cc` docs: 飞书远程控制桥的设计**
  目标是让多项目、多会话的工作能从手机上跟进和指挥。没有用 Claude Code
  自带的 Remote Control，两个原因：它只能驱动 Claude Code，而且把客户端
  钉死在 Claude 应用里。
- **`1a8536935` gui: 侧边栏可折叠、会数终端、并且诚实地关闭**
  单击改为只选中，打开改成双击（否则浏览一棵树会在每个看过的条目后面留下
  一个 shell）；运行中的条目显示终端数；关窗口时把「用户看不见、但会被一起
  杀掉」的 mux 窗口列出来问一句。
- **`[client] 1a5e523` main: 一个 pane 的积压是历史，不是新闻**
- **`[client] c54cc84` main: 一次回复结束当前轮**
- **`[client] 884473c` lines: 别重复相邻两次读取共有的部分**
- **`[client] 84d3623` cards: 每个 pane 每轮一张卡片，用流式追加**
- **`[client] e290ffa` router: 判断一条消息发给哪个会话，判断不了就不猜**
  「继续」发错了会话，比没发出去更糟 —— 尤其当那个会话正停在一件危险的事情前面。
- **`[client] 144ee86` card: 一轮对话渲染成什么形状**
- **`[client] 2e743ca` bridge: 连本地 mux 服务的 NDJSON 客户端**
- **`[client] acbac7f` config: 凭据从一个永远不进 git 的文件里读**
- **`[client] 96e8a10` docs: 飞书客户端第一期计划**

## 2026-09-01

- **`1de242934` gui: 加入会话侧边栏**
  窗口左侧一棵保存好的会话树，形状照 MobaXterm 的会话面板。每个条目绑定
  自己的 mux workspace，所以每个会话保有自己的一组标签页：点一个就把标签栏
  切到那个会话，上一个条目下面跑着的东西留在后台继续跑。
  宽度折进 `border.left`，因为 resize、render、标签栏起始 x 和鼠标坐标映射
  都已经把这个值当作内容区左边界，一起加宽就能挤压终端而不是盖住它。
  workspace 用稳定的节点 id 命名而不是节点名，这样重命名不会遗弃已经跑起来的
  终端，同名的两个条目也不会撞车。
