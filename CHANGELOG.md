# 变更记录

从上游 wezterm 分叉之后自己开发的内容。分叉点是 `78cd82dbb`
（`config: Replace winapi with windows-sys (#8098)`），在那之前的历史都属于上游，
不在这里记。

一共做了四件事，互相衔接：

1. **会话侧边栏** —— 窗口左侧一棵保存好的会话树，每个条目绑一个 mux workspace，
   照 MobaXterm 的会话面板做的。
2. **飞书桥** —— 一条本地 IPC，把 mux 里的 pane 暴露给外部客户端，好让多项目、
   多会话的工作能在手机上跟进和指挥。
3. **飞书客户端** —— 桥另一头的 Node 进程，住在另一个仓库
   `D:\dev\remote-controll`。它的提交在下面用 **`[client]`** 标出；那条功能横跨
   两个仓库，只记一半会看不懂。
4. **分屏标签组** —— 每块分屏顶上一行标签条，一块屏里可以叠多个终端；
   配置项 `enable_pane_tab_bar` 打开后对新建的窗口标签页生效。

飞书相关的设计文档在 `docs-internal/`：

- `2026-09-03-feishu-bridge-design.md` —— 整体设计与 IPC 协议
- `2026-09-03-feishu-bridge-plan-rust.md` —— Rust 侧实现计划
- `2026-09-07-feishu-per-pane-control-design.md` —— 按 pane 订阅与服务生命周期

分屏标签组的设计文档是 `docs/superpowers/specs/2026-11-05-pane-tabs-design.md`。

---

## 2026-11-09

分屏标签组，阶段 3：标签条上的拖动，以及点击规则收口。

- **`86085eb59` gui: 标签条点击规则收成 strip_press 一个函数**
  是否忽略这一下按键，以及这一下要记成什么，原来在调用方和测试辅助里各算一遍，
  现在只有 `strip_press` 一处。激活一个带「有新输出」小点的标签会让小点消失、
  标签变窄，重排后别的 `×` 可能正好落在指针下；所以激活之后紧跟的关闭一律吞掉，
  吞掉的那一下仍记着那次激活，连按第三下也被吞。窗口失焦时清掉点击记录。
- **`007c3d52c` gui: 标签条标签可拖动**
  按住标签拖：在同一条标签条内松手是叠内换位，拖到另一块屏的标签条上松手是
  挪进那块屏的叠，在标签条以外松手则取消。拖动中只画一条插入竖线，松手才调一次
  `move_pane_to_stack`，不像窗口标签栏那样实时换位（连着 mux server 时每一步都是
  一个 PDU 加一次整树重拉）。只在同一个窗口标签页内有效；指针移动不到半个格
  宽算单击。
- **`b25e7ce7d` gui: 键盘 MovePaneToStack 与拖动松手共用一个挪动 helper**
  两条路径共用 `spawn_move_pane_to_stack`，`await` 之后都 `invalidate` 重画。
  连着 mux server 时本地要等 resync 才拿到新顺序，而换位和同宽叶子之间的挪动
  没有输出，不重画的话标签条会停在旧顺序上，下一次拖动也会按旧画面量位置。

## 2026-11-06

分屏标签组，阶段 2：协议补齐，标签条的绘制和鼠标，三个按键动作，焦点卡死修复。

- **`35c1792ec` mux: 嵌套拖动按子树最小高度夹分割线**
  修阶段 1 遗留的问题：`header_rows = 1` 时拖外层分割线，嵌套的内层成员可能被
  压成 0 行。
- **`ee1195f34` mux、`9e3c79a98` codec: 「在叠里新开」与「挪进叠」的协议**
  mux 层新增两个入口，线上新增 `SpawnPaneInStack`（68）和 `MovePaneToStack`
  （69）两个 PDU。
  升级注意：`CODEC_VERSION` 从 49 涨到 50，`wezterm-gui.exe`、
  `wezterm-mux-server.exe`、`wezterm.exe` 要一起换，旧的 mux-server 得先停掉。
- **`6562fecb7` gui: 修焦点乒乓——丢掉过时的 PaneFocused 重放，client 不再回报
  server 的焦点回声**
  上游原有的 bug：同一个主线程任务里连切两次焦点（如 `Multiple { Left, Left }`）
  会无限乒乓，GUI 卡死；平时手动切换不会卡。本地 domain 上不需要 server 就能
  复现。两条回路：`frontend.rs` 重放过时的本地
  `PaneFocused`；`ClientPane` 把 server 迟到的焦点回声当成新的选择又报回去。
  修复接受三个代价：
  1. 高延迟连接上连续切换时，本地焦点会短暂把中间经过的 pane 重放一遍，这段
     时间里敲的字会进中间那块；本机 unix socket 上约 1 ms。
  2. 别的连接改了焦点之后，server 记录的「这个 GUI 聚焦的 pane」要等用户下次在
     这个 GUI 里切换才更新；不带 `--pane-id` 又没有 `WEZTERM_PANE` 的 cli 命令
     会选到旧 pane。
  3. 焦点通知指向一个 GUI 还没同步到的 pane 时可能乱序应用，两边会静默不一致，
     直到下一次操作。
- **`d69544e61` gui: 三个按键动作**
  `SpawnCommandInPaneStack`、`ActivatePaneStackRelative`、`MovePaneToStack`，
  没有默认快捷键，`enable_pane_tab_bar` 打开时出现在命令面板里。后续
  `5dc15ca0a` 把三个动作共用的守卫（标签页没有标签条时不做事）合成一份，标签条
  也改用 `Tab::visible_pane_of_leaf`。
- **`7fb2b6685` gui: 标签条排版纯函数 `layout_strip`**
  后续 `d96fce2d0` 修了零宽字素让 `×` 与关闭热区错位，`eb1137d34` 把零宽长
  字素按 1 格计宽，并让叠内新开的命令显示用户设的 label。
- **`982d03963` gui: 画 pane 叠标签条**
  注册标签条的 UIItem，隐藏 pane 有新输出时重画一次并在标签上画小点。后续
  `525237507` 改正 `prune_hidden_output` 的注释：zoom 时仍画被 zoom 那一叠的
  标签条。
- **`bbff0f314` gui: 标签条鼠标操作**
  点标签切换，点 `×` 或中键关闭，双击空白处在叠里新开。后续 `6c3ebacae` 让双击
  只认两次按中同一项，双击 `×` 只关一个 pane；`4879ef806` 把连击改成按「上一次
  按中的条目、动作和时间」判断，不再依赖格子里的连击计数（确认框抓鼠标会让
  列号从四舍五入变成截断，计数因此被重置），并让 pane 级 overlay 的输出也触发
  重画（上游原有的缺口：overlay 的 id 不在 tab 里，输出从不重画）。
- **`fcb8d1ce8` mux: 叶可见成员与 pane 尺寸各收成一份，钳住拖分隔条的行数**
  **`7ca67d93d` mux-server: 改正 `spawn_pane_in_stack` 回包注释里的理由**
  整理，基本不改行为；另在 `adjust_node_at_cursor` 加了 `.max(0)` 下限，防的是
  已经非法的树。

## 2026-11-05

分屏标签组，阶段 1：mux 模型。此时还没有标签条，只有模型、协议和配置项。

- **`6736f8e70` mux: 新增 PaneStack 与槽位/成员尺寸换算**
  **`aa275a5d8` mux: Tab 的叶子换成 PaneStack，并为标签条让出叶子顶上的行**
  **`a9a974285` mux: `split_and_insert` 里 resize 失败时把树放回去**
  **`ef0884b7c` mux: 叠的操作——加入、挪动、切换可见成员**
  **`0d352c505` mux: zoom 以叶子为单位，可见成员换了 zoom 跟着走**
  标签页的叶子从单个 pane 换成一叠 pane，同一叠里只有一个可见。每个标签页有自己的
  `header_rows`，建标签页时定死；非 0 时每个叶子顶上让出那么多行给标签条。zoom
  以叶子为单位，叠内切换不取消 zoom。`header_rows = 0` 时行为与以前一致。
- **`4bc564316` codec: 线上布局改为整叠 PaneStackEntry，SpawnResponse 带
  header_rows**
  **`30980b887` client: `ClientDomain::spawn` 先钳位 server 给的 header_rows
  再使用**
  升级注意：`CODEC_VERSION` 从 48 涨到 49，`wezterm-gui.exe`、
  `wezterm-mux-server.exe`、`wezterm.exe` 要一起换，旧的 mux-server 得先停掉。
- **`0937d2717` mux: 加 `enable_pane_tab_bar` 配置项**
  默认 `false`，只决定新建标签页的 `header_rows`；连 mux server 时以 server 端
  配置为准。`tab.rs` 以外的调用点改用整叠 API。
  文档见 `docs/config/lua/config/enable_pane_tab_bar.md`。
- **`2a75a3e29` mux: 终审修复**
  叶子最小行数收口为 `min_leaf_rows`，空叠不再留在 Split 下，补不变量测试。

## 2026-10-29

- **`36b9db46e` gui: 标签可以在标签栏里拖动换位**
  按住标签左右拖，标签跟着指针实时换位，松手停在那里；按下时照旧激活它，
  挪动和 `MoveTab` 走同一个 `move_tab`。指针越过邻居的中线才换位，量的是
  标签此刻画出来的位置，宽度不同的两个标签不会来回闪。
- **`c66421480` mux: 标签换了顺序要告诉 mux server**
  新 PDU `SetWindowTabOrder`（67）：客户端把窗口里来自这个 server 的标签
  按显示顺序整个发过去，server 重排；没提到的标签留在原位，激活的标签不变。
  拖动和 `MoveTab` 挪过的顺序关掉窗口再连回来也保留。只做客户端到 server
  一个方向，同时连着的其他 GUI 要等下次连接。
  升级注意：`CODEC_VERSION` 从 47 涨到 48，`wezterm-gui.exe`、
  `wezterm-mux-server.exe`、`wezterm.exe` 要一起换，旧 mux-server 得先停掉。
- **`7f1adc393` gui: 侧边栏的节点可以拖到树里别的位置**
  会话和文件夹都能拖，文件夹连同内容一起走；横线标出插入的空隙（缩进即层级），
  方框标出要放进的文件夹，最后一行下面是顶层末尾。松手才挪，立刻写入
  `sessions.json`。文件夹不能拖进自己里面，搜索时不能拖。单击的效果
  （展开折叠、切换会话）改到松手且没有拖动时才发生，双击不变。
  文档见 `docs/config/lua/config/session_sidebar_enabled.md`。

## 2026-09-17

- **`bbbe8dda0` gui/mux: 关闭最后一扇窗口时可以顺手停掉 mux server**
  `wezterm cli kill-server` 发一条 `KillServer` PDU，mux-server 答完
  `UnitResponse` 就退出；GUI 实例收到会直接拒绝，得带 `--prefer-mux` 才连得到
  真正的 mux-server。新配置项 `mux_server_close_confirmation`
  （默认 `"AlwaysPrompt"`）：关闭一个 mux 会话的最后一扇 GUI 窗口时问一句
  "要不要把 server 也停了"，`y` 才停，`n` 还是老样子只 detach、server 继续跑；
  设成 `"NeverPrompt"` 恢复旧的静默 detach，不问。
  文档见 `docs/config/lua/config/mux_server_close_confirmation.md`。
  升级注意：`CODEC_VERSION` 从 46 涨到 47，新版 GUI 连不上还在跑的旧版
  mux-server，旧的那个进程得先用 `Stop-Process` 手动杀掉一次；这次没有重新生成
  shell 补全文件。

## 2026-09-15

- **`03db00a06` bridge: 告诉飞书服务共享文件在哪**
  启动飞书服务时注入 `BRIDGE_TOKEN_PATH`、`BRIDGE_PORT`、`HOOK_TOKEN_PATH`，
  以及 `data\feishu-credentials.txt` 存在时的 `FEISHU_CREDENTIALS`；环境里
  已有的同名变量优先。hook token 只在不会让已配置的 hooks 失效时才换到
  `data\`。`remote-controll` 不用改。
- **`b23cf9efe` config: 便携安装的数据放在安装目录**
  exe 旁边有 `wezterm.lua` 时（上游的 thumb drive 模式），`DATA_DIR` 改为
  `<安装目录>\data\`：`sessions.json`、`bridge-token`、`recent-*.json`、
  `repl-history` 都跟着安装目录走。首次使用时从 `%APPDATA%\wezterm` 和
  `~\.wezterm-bridge-hook-token` 复制一份（`data\.migrated` 标记只做一次，
  旧文件不删）；落位用硬链接而不是覆盖，GUI 和 mux-server 同时启动也读不到
  写了一半的 `sessions.json`。`data\` 建不出来或写不进去时照旧用 `%APPDATA%`。
  从便携安装的 pane 里启动的程序（比如 `cargo run` 的开发版 GUI）继承了
  `WEZTERM_CONFIG_FILE`，只要它指向的目录里有 wezterm 的 exe，就用同一个
  `data\`——否则开发版 GUI 和部署版 mux-server 会各读写一份 `sessions.json`。
  有文件复制失败时不写 `.migrated`，下次启动重试。

## 2026-09-14

- **`3e24b2eb3` wsl: WSL 服务卡死时，窗口和新标签页都不能跟着卡**
  发行版列表来自 `wsl -l -v`，启动、每次重载配置、每次在本地 domain 里新建
  pane 都要跑一遍；WSL 服务卡住时 wsl.exe 永远不返回，wezterm-gui 连第一扇
  窗口都开不出来，新标签页也跟着卡。现在列表缓存起来：只有第一次要等，之后
  超过 30 秒的旧列表先直接用，后台单线程去刷新；刷新失败就保留上一份好的
  列表 —— 丢掉的话 WSL 标签会悄悄变成本地 shell。`wsl -l -v` 限时 3 秒，
  超时连整棵进程树一起杀（job object）：wsl.exe 有三层进程，只杀自己起的
  那个，另外两个会一直挂在卡住的服务上。

## 2026-09-10

- **`5be353dd1` mux: `mux_env_remove` 支持前缀匹配，默认再去掉颜色和 agent 变量**
  mux-server 是常驻进程，启动时的环境变量会一直传给它之后建的 pane。从设了
  `NO_COLOR` 的工具里启动，同一个窗口里有的 pane 黑白、有的彩色；从
  Claude Code 里启动，之后每个 pane 都带着一个早已结束的会话的
  `CLAUDECODE` / `CLAUDE_CODE_*` 标记。默认列表加上 `NO_COLOR`、
  `FORCE_COLOR`、`CLICOLOR`、`CLICOLOR_FORCE`、`CLAUDECODE`、`CLAUDE_CODE_*`、
  `CLAUDE_PID`；以 `*` 结尾的条目按前缀匹配（Windows 上不分大小写）。
  `CLAUDE_CONFIG_DIR` 这类用户自己设的变量不动。

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
