WezTerm-dev 便携版
==================

版本：@VERSION@
打包日期：@DATE@
这是一份干净的包，不带任何会话、令牌、凭据或本机路径。


安装
----

1. 解压到当前用户能写的目录，例如 D:\WezTerm-dev。U 盘也可以。
   不要放到 C:\Program Files 下：那里写不进 data 文件夹，数据会改存到
   %APPDATA%\wezterm，便携就失效了。
2. 双击 wezterm-gui.exe 启动。需要的话自己建桌面快捷方式。

不需要安装程序，也不写注册表。


第一次启动
----------

- 本目录下会自动生成 data 文件夹，会话列表等数据都存在这里。
- 如果这台机器以前用过 WezTerm，原来的会话列表等会被复制一份过来
  （原文件保留），只做一次。
- 启动后是一个只有会话侧边栏的空窗口，在侧边栏里新建会话即可。


可选：飞书镜像
--------------

1. 装好 Node.js 和 remote-controll 服务。
2. 用记事本打开 wezterm.lua，找到末尾飞书那一段：把示例里的两个路径换成
   node.exe 和 main.js 的实际完整路径，再删掉那一行开头的两个减号。
3. 飞书应用凭据放到 data\feishu-credentials.txt。
4. Claude Code hook 用的令牌会在飞书服务第一次启动时生成在
   data\bridge-hook-token，把它填进这台机器的 Claude Code hook 配置。

不用飞书的话这一节可以跳过，其余功能不受影响。


其他
----

- 会占用本机 8788 端口。被占用的话，在 wezterm.lua 里把 8788 改成别的端口号。
- 以后要搬到别的机器：先确认 WezTerm 已经完全退出（任务管理器里没有
  wezterm-mux-server.exe），再把整个目录连同 data 文件夹一起拷走即可。
- data 文件夹里的凭据、令牌，以及 ssh_hosts.json 里侧边栏 SSH 会话的密码
  （明文保存）都是敏感信息，不要放进共享盘或代码仓库。
