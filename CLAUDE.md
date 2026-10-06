# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

ShellRS（crate 与二进制都叫 `shellrs`）是 Xshell / WinSCP 式的 SSH 主机管理工具，基于 `gpui-kit` 0.6.1（GPUI + gpui-base + gpui-component）。界面文案用中文，标识符用英文。功能说明见 `docs/manual.md`（`README.md` / `README.en.md` 是开源首页，截图在 `docs/screenshots/`）。

- **术语。** 保存的一项叫「主机」（量词「台」），代码和库里叫 host（`Host`、`HostId`、`HostStore`、`hosts` 表）。它以前叫「会话」/ `Session`，已全部改掉，新代码和文案别再用。填 IP / 域名的字段叫「地址」（`Host.address`）。SSH 协议层的 session（「SSH 会话通道」、russh 的 `Session`、SFTP 会话）照旧。
- **两种不保存的连接。** 「外部连接」：堡垒机像调 Xshell（`ssh://`、`-url`）或 WinSCP（`sftp://`）一样拉起 ShellRS，只开一个终端或 SFTP 标签（`HostStore::is_external`）。「临时连接」：标题栏「临时连接…」打开，功能齐全。两者共同的「不保存」在代码里叫 temporary（`HostInfo::temporary`）。
- **全是真的。** SQLite（`~/Library/Application Support/shellrs/shellrs.db`，`SHELLRS_DATA_DIR` 可覆盖目录）、本地终端（`portable-pty` + `alacritty_terminal`）、SSH（`russh`）、SFTP（双栏、传输队列、断点续传）、端口转发（`-L` / `-R` / `-D`）、凭据、连接方式（直连、多级跳板、HTTP / SOCKS5 代理）、右侧栏七个工具、内置编辑器和预览、外部 CLI、在线升级。密码和口令只进系统钥匙串（`keyring`），**数据库里永远不出现秘密**。

做 UI 之前先读 `gpui-kit` 与 `gpui-kit-design-guides` 两个技能（其中的 Coding Guides 和 Design Guides 在本仓库是规范，不是参考）。不要凭记忆臆造 gpui-kit API：以 `~/.cargo/registry/src/*/gpui-component-0.6.1/`、`gpui-base-0.6.1/` 的源码或 `https://gpui-kit.com/component/<name>.md` 为准。

## 已定的产品决定

用户定过的事，不要改回去。细节以 `docs/manual.md` 和代码为准。

- **SFTP**：Dock 标签，每次「打开 SFTP」新开一个、各自连接。操作对标 WinSCP Commander：两行工具栏、路径标签、WinSCP 的列、高亮多选、「名称」格才是项目、框选。本地侧与远程侧对称，本地删除进废纸篓；书签按主机、按侧持久化。传输进行中还能再发起，新批次在标签底部的传输队列里排队，不做前台进度对话框。大小列默认整 KB，右键列标题可切换。「显示隐藏文件」（`.` 开头的）本地、远程各一个开关、互不影响，每侧对所有标签生效，存进设置，默认不显示；工具栏用睁眼 / 闭眼两个图标表示状态，不用选中效果；显示时隐藏文件的文字浅一些。暂不做目录同步、过滤、查找、目录树和 Dock 布局持久化。
- **主机树**：首次启动是空库；分组可任意嵌套，主机可以不属于分组；删除分组连同子分组和主机，确认框写明数量。
- **主机对话框**：
  - 认证方式三选一：「密码」（留空就每次连接时询问，不悄悄试密钥）、「使用凭据」、「无密码」（依次试服务器免认证、SSH Agent、`~/.ssh` 默认私钥，服务器要密码就报错、不询问）。主机自己没有私钥文件，要用私钥就建密钥凭据。
  - 「测试连接」是真登录，不强制填密码。
  - 连接方式三选一：直接连接、SSH 跳板、代理连接。跳板只走列出的链（跳板自己的连接方式不参与），不做拖动排序；删掉的跳板留「已删除的主机」占位，连接时直接报错。代理是 HTTP `CONNECT` 或 SOCKS5，失败直接报错、不弹问答。跳板和代理不叠加。
- **凭据**：侧栏第三个列表（密码 / 密钥 / SSH Agent），凭据带用户名。
  - 密钥的来源「本机文件 / 粘贴 / 生成新密钥」在同一个对话框里（Ed25519 默认，另有 RSA 4096）。粘贴和生成的私钥存成数据目录 `keys/` 下的文件，不进钥匙串：Windows 凭据管理器装不下 RSA 私钥。
  - 删除仍在用的凭据要确认，那些主机改回自己登录（密码凭据的改成「密码」，其余改成「无密码」）。
  - 主机对话框里不做「新建凭据…」（不叠弹层）。
- **端口转发**：侧栏第二个列表，不是 Dock 标签；标题栏「ShellRS」后面三个图标切换主机 / 端口转发 / 凭据。
  - 一条规则经由一台保存的主机，自己建连接：不算主机的连接状态，主机「断开连接」不停它；删除主机连同规则一起删。
  - 「创建」只保存不启动，可设「启动 ShellRS 时自动开启」；新建时「经由主机」留空，由用户自己选。
  - 断线按 1、3、10 秒自动重连三次，重连不弹问答。
  - 对话框是一行三张类型卡片、实时示意图（服务器按角色叫「SSH 服务器」）、一句话说明和一直循环的流向动画。
- **临时连接与外部连接**：
  - 临时连接的密码只在内存。
  - 外部连接只认命令行参数，不注册系统的 `ssh://` 链接；打开时收起左侧栏。
  - 外部 CLI 照样列出这两种主机，分组写「（未保存）」。
- **开始页**：没有标签时显示「最近连接」。「快速连接」搜索保存的主机、回车连接；多选做好但关着（常量 `MULTIPLE`）。
- **右侧栏**：窗口最右一列竖排的工具切换。
  - 显示规则：只跟着 SSH 远程终端出现，SFTP、本地终端、设置、开始页都不显示。它是工作区级的一份，开合和所选工具在所有终端间共用。默认收起，宽 320 px，只能往宽里拉。⌘⌥B / Ctrl+Alt+B 切换。
  - 工具从上到下：命令片段、历史命令、Docker、系统服务、进程管理、网络连接、系统监控。
  - 已知不是 Linux 的主机不显示系统监控、网络连接、进程管理、系统服务；已知是 Windows 的再去掉 Docker 和历史命令；外部连接只有命令片段。
  - 系统监控每 2 秒读一次（运行时长随读数刷新，不自己走），磁盘每 30 秒，只在显示时读。≥ 70% 黄、≥ 90% 红。
  - 网络连接、系统服务、Docker、历史命令的列表不自己刷新：打开、换终端、点刷新、执行命令之后才读。进程管理每 15 秒读一次。
  - 结束进程，停止 / 重启服务，停止 / 重启 / 删除容器都先确认。不是 root 时用 `sudo -n`，要密码就报「需要 root 权限…」。
  - 系统服务的标签用 gpui-kit 的下划线 `TabBar`（均分放不下「已停止 202」），Docker 用均分的 `shared::count_tabs`。
  - 历史命令读主机的 `~/.bash_history`，不自己记录；点一条是放进输入行、不执行，另有执行按钮。
  - 命令片段所有主机共用（库里预留了「适用范围」列）；「点击时自动执行」新建时默认勾上；执行按钮鼠标移上去才出现，位置一直留着。
- **在线升级**：
  - 清单地址 `https://dl.shellrs.com/update/v1/{channel}.json`、平台 key 的名字和公钥槽位一经发布就不能改。
  - 默认在后台检查并下载，下好、校验完才在标题栏亮按钮（`success` 色，出错时 `warning` 色）。
  - 更新对话框不列更新内容：「查看更新内容」打开整页的 `https://shellrs.com/changelog`，另一个按钮是「重启并安装」，没有「稍后」。不点也在退出时装好。
  - 「设置 › 关于」只有「应用更新」一组三行：当前版本、更新渠道（稳定版 / Beta，运行时切换）、自动升级。
  - 检查只发 User-Agent，不发安装标识。
- **编辑器**：中间区的「编辑器」Dock 标签，不用系统程序打开。
  - 双击、回车、F4、右键「编辑」都打开它，本地文件也行；新建文件后直接打开。
  - 只认 UTF-8（不做 GBK），上限 5 MB，约 15 种语言语法高亮。
  - 远程文件原地改写：截断后写原文件，不走 `.filepart` 加改名。保存前核对，被改过就问「覆盖」。
- **终端通知**：只做程序请求的（OSC 9 / 777）和响铃，不做 OSC 133 长命令通知：那要每台服务器装 shell 集成，收益不抵成本。不在前台发系统通知，在前台而终端在别的标签发应用内通知；响铃在终端就在眼前时不提醒。
- **主题**：「应用外观」（浅色 / 深色 / 跟随系统）决定深浅；「外观 › 主题」左右两栏里浅色、深色各选一套，按当前外观生效。组里第一项「界面跟随主题」默认关闭：界面是中性的黑白灰（gpui-kit 默认主题），只有终端按主题配色；打开后界面也按主题配色。用户试过默认跟随，觉得中性界面更合适，别改回去。页面「重置」对卡片和开关各管各的。
  - 左栏只列浅色主题，右栏只列深色主题；卡片是名称加示例输出，选中的整张加底色，右上角是蓝底（主题的 `blue`）对勾。
  - 只有内置的，浅色、深色各 10 套，默认 ShellRS Light / Dark（底色与界面一致），Solarized 第二，Tokyo Day / Tokyo Night 第三。同一主题的浅、深两版在两栏同一行，没有另一版的（Rosé Pine Dawn、Everforest Light、Dracula、Nord）排最后。只收宽松许可（MIT、Apache-2.0）的配色。不做自定义主题、导入、按主机指定。
- **关键字高亮**：设置左栏单独一个分类，排在「终端」下面，三组：常规（总开关）、预览（当前终端主题下的示例输出）、规则（N）。规则存在 `settings.json`，所有终端共用。
  - 规则表在行里直接改，不弹对话框：拖动把手（一直显示，只有把手能拖）、启用、正则表达式、备注、颜色（gpui-kit `ColorPicker` 加 `#rrggbb` 输入框）、通知、删除（不确认）。规则都是正则，区分大小写；颜色是用户数据，任意 RGB，不跟主题；不做加粗。重叠时靠前的优先。
  - 总开关默认关闭。首次（文件里没有这一项）预置三条示例：ERROR 红、WARN 琥珀、IPv4 蓝，各自启用、都不通知，打开总开关就能用；用户删光后保持为空。空的新行照样保存。
  - 通知同响铃：看不到那个终端才提醒，同一终端 10 秒一次，标题用备注。全屏程序（备用屏）里不高亮也不通知。
- **预览**：图片和 Markdown（不做 HTML），显示在快速查看式的大对话框里，不是标签。
  - 双击图片预览；Markdown 和 SVG 是文本，双击编辑，右键另有「预览」。
  - 图片默认适合窗口居中，可放大、缩小、看原图、滚动；比例只写百分比。

## 常用命令

```bash
cargo run                                   # 启动应用
cargo build                                 # 首次构建会编译整个 GPUI 栈，很慢
cargo test                                  # 单元测试（模块内）+ tests/ui/（无头窗口 UI 集成测试）
cargo test --lib                            # 只跑单元测试
cargo test --test ui                        # 只跑 UI 集成测试
cargo test --test ui new_host -- --nocapture   # 按名称片段跑单个 UI 测试
cargo test --test cli_bin                   # 以子进程运行 shellrs-cli 的端到端测试（Windows CI 也跑）
cargo test --lib update::                   # 在线升级；装了 minisign 和 jq 时还验证发布脚本签出的清单客户端认
cargo clippy --all-targets -- -D warnings   # 必须无警告
cargo fmt --check

sqlite3 ~/Library/Application\ Support/shellrs/shellrs.db '.schema'   # 查落盘结果
security find-generic-password -s shellrs -a "password:<user>@<host>:<port>"   # 查钥匙串条目
cargo test -- --ignored                     # 会读写真实钥匙串的测试，默认跳过
```

工具链：`rust-toolchain.toml` 固定 Rust 1.98.1，因为 `gpui-pre` 用到了 1.94 尚未稳定的 std API。`Cargo.lock` 把 `gpui-pre*` 系列固定在 0.3.2（gpui-kit 0.6.1 发布时配套的版本），不要随手 `cargo update` 它们。`cc` 停在 1.2.x，因为 SQL 高亮的 `tree-sitter-sequel` 要求 `cc ~1.2`。`cargo test` 需要 gpui-kit 的 `test-support` 特性，首次会再编译一遍。

本终端的 `screencapture` 被 macOS 权限拦截，视觉检查只能靠 UI 集成测试或让用户看窗口。

## 架构

单个二进制 crate，带 `src/lib.rs`，这样 `tests/ui/` 能驱动生产环境的 `Workspace`。模块按能力划分：功能模块不依赖 `workspace`（窗口壳），也不碰彼此的内部实现。

- `app/`：
  - `actions.rs` 是全部用户命令；带 id 的动作由 `id_actions!` 声明。
  - 另有快捷键、数据目录路径（`paths.rs`）、额外的 Lucide 图标（`CatalogIcon`）、操作系统 logo 和品牌图标（单色 SVG）、退出守卫（`quit.rs`）、macOS 关窗口即隐藏（`window_hiding.rs`）。
- `host/`：主机、分组、凭据、端口转发规则和命令片段的模型（它们同库、随主机级联删除，所以都放在这里，host 不依赖其他功能模块），以及：
  - `HostStore`：内存是唯一事实来源，写穿到 SQLite；
  - `database.rs`：`SCHEMA`、迁移；
  - `login.rs`：登录快照 `HostLogin`；
  - 主机树面板、主机和分组对话框、快速连接、`ssh_link.rs`（解析堡垒机链接）。
- `connection.rs`：问答协议（`ConnectionPrompt`…）、`Latency`、测试连接的 `LoginTest` / `ConnectionTester`。
- `ssh/`：
  - `SshConnector`：所有登录都走它（终端、SFTP、exec、测试连接、端口转发、外部 CLI）。
  - 远程终端的传输：pty、系统探测、延迟、旁路 exec。
  - 代理握手；外部 CLI 用的 `run_command`。
- `terminal/`：本地 PTY、驱动 `alacritty_terminal` 的引擎、`TerminalView`（网格、选区、查找、清屏）、关键字高亮（`highlight.rs`）、终端字体全局量、右侧栏工具共用的 `ExecTarget`。
- `sftp/`：可注入的传输和本地目录接口、`RemotePath`、`RemoteFs` 协议适配、worker（批次、三次重连、空闲断网检测），以及：
  - `upload.rs` / `download.rs`：断点续传，续传记录由 `journal.rs` 管；
  - `operations.rs`：删除、重命名、新建、改权限；
  - `edit.rs`：编辑器和预览的整文件读写。
- `explorer/`：SFTP 标签的界面：两侧面板、列表、路径标签、各种对话框、传输队列，以及：
  - `file_edit.rs`：给编辑器和预览读写文件；
  - `preview.rs` / `image_preview.rs`：预览对话框和可缩放的图片。
- `editor/`：编辑器标签（`EditorPanel`），语言、缩进和状态栏文字是 `model.rs` 的纯函数。
- `forward/`：端口转发的运行（`ForwardManager`、每条规则一个工作线程、SOCKS 解析）和界面。`credential/`：凭据的界面。
- `monitor/`、`netstat/`、`processes/`、`services/`、`docker/`、`history/`、`snippets/`：右侧栏的七个工具。各自是「命令（`linux.rs` 等）+ 纯解析和模型（`model.rs`）+ 面板」，确认和通知在工作区。
- `secrets/`：`SecretStore`（钥匙串）、`SecretRef`（条目归属）、`TemporarySecretStore`（不保存的连接的密码只在内存）。
- `settings/`：`AppSettings`、写 `settings.json` 的 `SettingsStore`、把设置落到窗口上的 `apply`、设置标签。
- `cli/`：外部 CLI（`shellrs list/exec/upload/download`，给 AI Agent 用）、套接字协议、应用里的服务端、PATH 和 Agent skill 的安装。
- `update/`：在线升级，包括清单、验签、下载、各平台安装器和 `Updater`。
- `workspace/`：窗口壳。持有 store、`DockArea`、各类标签的注册表和**全部动作处理器**，按领域拆成多个文件（`forwards.rs`、`credentials.rs`、`links.rs`、`editors.rs`、`tools.rs`、`updates.rs`…）。另有标题栏、状态栏、左侧栏 `Sidebar`、右侧栏 `ToolSidebar`、开始页。
- `shared/`：多个功能共用的展示片段（`ClosableTabTitle`、`HostMark`、`SegmentedControl`、`RowTooltips`、`confirm_danger`、`form_error_notification` 等）。

## 关键约定与坑

### 状态与持久化

- **内存说了算，写穿。** `HostStore` 的 mutator「改内存 → 写库 → `notify`」，`*_unnotified` 是纯内存的一半（给单元测试和加载）。写失败时内存照样改，store 发 `PersistFailed`，工作区弹错误通知，绝不静默吞掉。消费方 `cx.observe(&store, ..)`。设置同理：写穿到 `settings.json`（整文件替换），工作区观察后用 `settings::apply` 落到窗口上。
- **秘密只进钥匙串。** `schema_never_contains_secret_columns` 守着这条线。`SecretStore` 的方法是阻塞的，只在后台执行器或工作线程上调；写入只经 `HostStore::save_secret`。
  - `SecretRef` 决定条目归属：密码按 `user@host:port`，私钥口令按文件路径，密码凭据按随机的 `keychain_id`，代理密码按代理地址加用户名，不保存的连接只在内存。
  - 没人再用的条目由 store 清理（`password_in_use`），私钥口令从不自动删。
  - 秘密不放进派生了 `Debug` 的 `HostDraft` / `CredentialDraft`。
  - 对话框里的密码字段是异步预填的，`*_loaded` 标志没置上之前，空字段不算「用户清空了」。
- **私钥文件。** ShellRS 保存的私钥在 `keys/` 下（目录 0700、文件 0600，经临时文件改名写入）。只有没人再用时才删这个目录下的文件；用户自己的私钥文件从不改动或删除。
- **登录快照在 UI 线程解析。** 工作线程读不到 store，拿到的是 `HostLogin`：凭据和跳板都已查好。它只含影响登录的字段并派生 `PartialEq`：改主机或凭据时比较前后快照，不同才发 `ConnectionSettingsChanged` 重连，所以只改名称不重连。
- **id 由内存分配**，四个独立计数器，重启后从库里最大 id + 1 开始，删掉的最大号会被复用。对外只给 `PublicId`（16 位随机串，改名、改地址都不变，不复用），不给 `HostId`。
- **改表**：现在 `SCHEMA_VERSION` 是 14，所有表都是 `STRICT`。
  - 改表要升版本、在 `STEPS` 里加一步，并加一个「用冻结的旧 schema 升级、再和新库比对」的测试。
  - 加列直接 `ALTER TABLE ADD COLUMN`，在 `SCHEMA` 里也写在最后一列之后。
  - 库只往前升：旧版本遇到新库直接报错、不动文件。迁移前先 `VACUUM INTO` 备份。
- **一个数据目录只跑一个 ShellRS。** 起图形界面之前先连外部 CLI 的套接字：连得上就发 `Request::Activate`（带启动链接）然后退出。为兼容已装的 CLI，不升 `PROTOCOL_VERSION`。
- **临时主机在 `HostStore` 的 `temporary` 里**：`host()` / `login()` 找得到，`hosts()` 看不到，也不写库。会写外键的检查用 `saved_host`。最后一个标签关掉就 `remove_temporary`。

### GPUI / gpui-kit

- **一个命令一个处理器。** 按钮、菜单、快捷键都只派发 `app/actions.rs` 里的动作，由 `Workspace` 处理，不写临时闭包直接改状态。例外是对话框内部自己的状态（预览的缩放）：工作区够不着对话框里的视图，由视图自己处理。
- **焦点。**
  - `window.dispatch_action` 只能到达焦点路径上的处理器，所以工作区里必须有东西持有焦点。
  - 永远不要聚焦工作区根节点的 handle，否则对话框的焦点陷阱拿不到焦点。
  - 中间区每个面板都要在 `set_active(true)` 里收下焦点，否则「×」、⌘W 和快捷键会静默失效。
  - 对话框关掉后焦点为空：标题栏、开始页、对话框和右侧栏的按钮一律经工作区 focus handle 的 `dispatch_action` 派发，在对话框里还要用 `window.defer` 延后。
  - 要预先聚焦对话框里的字段，只能在 `open_dialog` 返回后的同一次更新里同步调用。
- **焦点监听器里改渲染状态要 `defer_in`**：`on_focus_in` 等在绘制的焦点阶段执行，这时 `notify` 不排新帧。
- **后台线程不直接唤醒 GPUI 前台任务。** 工作线程的事件由 UI 定时轮询：终端和 SFTP 每 16 ms，转发和右侧栏工具每 50 ms，升级每 100 ms。等回答的 `oneshot` 在轮询的事件处理里（UI 线程）完成。
- **渲染回调不得读取实体**（树行、`render_td`），只传快照。
- **窗口还在搭的时候不能弹通知**：`Root` 还没挂上，会 panic。用 `notify_once_open` 延后。
- **对话框**：
  - 每个 `open_dialog` 都要 `.overlay_closable(false)`。
  - 表单校验错误一律弹 `form_error_notification`，并在 `on_close` 里 `dismiss_form_error`。
  - 通知层要包成 `deferred`、优先级 99，高过对话框，否则被压在对话框背景底下。
  - 下拉框攥着焦点时，提交会把它弹开，这是 gpui-kit 的行为。
- **右键菜单挂在容器上，不挂在行上**（主机树、文件列表、开始页、进程列表）。`ContextMenu` 在 `request_layout` 里抢焦点，挂在 `uniform_list` 的行上会触发「set_focus called more than once」断言。行把命中的项写进一个 `Rc<Cell<..>>`，容器在捕获阶段先清空它。行和容器不要各挂一个菜单，两个会一起弹出。
- **滚动条挂在不滚动的外层上**，或直接用 `overflow_y_scrollbar()`：挂在滚动元素自己身上，滚动条会跟着内容一起移走。
- **按上一帧的尺寸重排**（路径标签、监控的竖条、图片预览的画框）：render 时拿不到布局尺寸。放一个 `canvas`，在 prepaint 记下尺寸，变了就 `on_next_frame` 通知重画。绝对定位的 `canvas` 必须 `top_0().left_0()`。
- **`img` 会给自己的盒子加上图片的宽高比**，压过百分比高度。要控制尺寸就给宽和高都写绝对值。
- **gpui-kit 的 `Settings` 把选中的分类放在元素状态里**，标签切走、不再绘制时就丢了，切回来回到第一类。设置页靠页头的 `title_suffix`（只给正在显示的页绘制）记下当前分类，再经 `default_selected_index` 恢复；页内滚动位置不保留。
- **`Select` 必须放进有尺寸的容器**，否则会盖住整行。`ListItem` 里会增长的文字要 `w_0().flex_grow(1.).overflow_x_hidden()`，否则会把 `suffix` 挤出去。
- **`ColorPicker` 要自己给常用色**（`featured_colors`）：默认的第一行是主题色，它们也在下面的调色板里。gpui-kit 按十六进制给色块起 id，同一颜色出现两格就是重复的无障碍节点，调试版在辅助技术（或借助它的窗口管理工具）连着时直接 panic；测试平台不建无障碍树，测不出来。常用色不能取调色板（shadcn 的 stone、red … pink）里的颜色。
- **要彩色就不能用 `Icon`**：它把 SVG 当成单色遮罩画。系统徽章是「主题色圆底 + 单色 logo」。
- **`px(..)` 只用在 API 要 `Pixels` 的地方**，其余用 rem 助手和 `cx.theme()` 的 token。
- **标签页全部可关。** 关闭走 `DockArea::remove_panel`（`TabGroup::close_panel` 拒绝关最后一个）。中间区面板的 `closable()` 返回 `false`，否则「…」菜单会多一个绕过确认的「关闭」。标题用 `ClosableTabTitle`，它带标签菜单，双击切换侧栏。`panel_name()` 是持久化键，不能改。
- **左右侧栏各是一个面板。** 左侧 Dock 里只有 `Sidebar`（三个列表），右侧 Dock 里只有 `ToolSidebar`。不用 `set_dock` 换面板，否则焦点会丢。
  - 当前标签只经 `Workspace::set_active_tab` 改，否则右侧栏不跟着变。
  - 右侧 Dock 不可折叠，否则标签栏会多出 gpui-kit 的折叠按钮；关闭时临时设成可折叠。
  - 最窄 320 px，由 `hold_tool_sidebar_width` 维持。
- **动画用 gpui-kit 的 `animate_keyframes`**（转发示意图），不自己写播放。

### SSH

- **登录。** 所有登录都走 `SshConnector`，共享 known_hosts 写锁。known_hosts 写在 `<data_dir>/known_hosts`，不碰 `~/.ssh/known_hosts`。
  - 每次先发 `none`，再按 `LoginMethod` 走。
  - 密码和口令先查钥匙串，查不到或被拒才弹框；被拒时不删钥匙串条目。
  - 失败的说明一律经 `describe_login_error`，它沿错误链找真正的原因。
- **算法按 OpenSSH 的默认来**：加了 `ecdh-sha2-nistp*`，素数组交换最小 2048 位，因为用户的堡垒机是老 Java 上的 Apache SSHD。没有加 SHA-1、CBC 这些 OpenSSH 默认也不开的算法。
- **区分断网和要人处理**：`is_network_error`，它要显式认 `russh::Error::IO`。只有网络类错误才自动重连。agent 的错误只格式化进文字，不挂进错误链。
- **跳板。** 每台跳板开好通道就丢掉自己的 `Handle`，整条链靠下一跳攥着的通道流活着：最终连接一断，链依次关闭。问答写明是哪台跳板；跳板和代理的失败包一层 `RouteFailure`。
- **外部连接只开一个通道**（`shell_only`）。用户的堡垒机在同一连接上多开一个通道就会结束会话，所以不做系统探测，旁路 exec 直接拒绝。以后给 SFTP 加别的通道，也要对 `is_external` 关掉。
- **右侧栏的工具在终端自己的连接上执行命令**（`TerminalTransportCommand::Exec`），不另起登录。
  - 一次一条，10 秒超时，输出最多 1 MB。
  - 命令写成一行 `sh -c '…'`，脚本里不出现单引号和 `!`，因为要经得过 bash、zsh、fish、csh。
  - 拼进命令的名字先过 `valid_*`。
  - 各工具都有用本机 `sh` 真跑脚本的单元测试。
- **每次连上都重探主机系统**（`uname` / `os-release`，Windows 再试 `cmd /c ver`），结果经 `HostStore::set_host_os` 入库。往返延迟每 5 秒量一次（`keepalive@openssh.com`）。

### SFTP

- **每个标签独立连接、独立传输引擎。** 引擎一次只跑一批，其余在 `TransferQueue` 里排队，引擎的 `Idle` 推动队列前进；进度只归队首；停止的批次挡在队首，等用户继续或移出。
- **SFTP 独立于终端。** 关闭终端不停传输；断开或删除主机会停止传输，并保留续传数据。
- **下载严格按偏移顺序写 `.filepart`**，因为续传只信它的长度。
- **文件操作从不跟随符号链接**：删除只删链接本身，改权限跳过链接，传输原样创建链接。
- **空闲时也要发现断网**：通道流包一层 `WatchedStream`。等待时只持有 `Weak`，不能攥着 `Arc<SftpClient>`，否则「断开」关不掉连接。
- **选择归面板，不归表格。** `DataTable` 设成 `row_selectable(false)`，`FilePane` 持有按名称保存的 `Selection`。「名称」单元格才是项目，拖文件只从图标和文件名开始，其余地方按下拖动是框选。
  - GPUI 会把同一类型的拖动通知给所有监听者，所以框选的监听要核对是不是自己发起的。
  - 快速一甩时拖动的开始和松开落在同一帧，起点仍会收到单击，用 `ends_a_drag` 排除。
  - 文件列表的快捷键按焦点路由（`ExplorerShortcut`，找 `contains_focus` 的那个标签），不按最近激活的标签。
- **工具栏永远一行高**，放不下的按钮整个藏起来。新标签两侧对半分，改工具栏宽度时要重算放不放得下。分栏状态归标签（`ResizableState`）。读目录不挪动列表；出的问题写在窗口左下角（`status-connection`）。
- **断开时**，对远程侧的操作要弹「SFTP 连接已断开 / 重新连接」。
- **编辑器和预览：先读后开**，读写走 SFTP 标签自己的连接。
  - `ReadFile`、`ReadBytes`、`WriteFile` 和 `List` / `Operate` 一样另起任务，和传输并行。
  - 文件读成功才开标签或对话框；读不了的在 `refuse_open` 说明，远程的给「下载…」。
  - 查重在工作区（`editor_for_location`）：已开就切过去。
- **编辑器写回**：`open_replace` 截断写，保存前核对 `FileStamp`（大小加修改时间）。写到一半失败是 `SaveFailure::Interrupted`：作为 context 包在原错误外面，所以仍认得出断网；这时作废戳，下一次保存不再核对。
- **编辑器的文本规则**：只认 UTF-8；所有换行都是 CRLF 才转成 LF 给编辑器、保存时换回，BOM 原样保留。⌘S 绑在 `FileEditor` 上下文。
- **编辑器的关闭和退出**：关 SFTP 标签会带走它的远程编辑器；批量关闭、关 SFTP 标签、退出时有未保存的文件，只问一次。
  - 退出守卫是全局量（`app::set_quit_guard`）：对话框关掉后焦点为空，`Quit` 到不了工作区的元素。Windows / Linux 点窗口的关闭按钮也走它。
  - macOS 程序坞右键退出、注销关机，以及「重启并安装」都拦不住。
- **高亮语言**就是 `Cargo.toml` 给 gpui-kit 开的 `tree-sitter-*` 特性；`language_for` 只能给出这些名字。
- **预览的图片**：
  - `ImagePreview` 自己在后台解码，所以解不开时能说出来；用 `ImageSource::Render` 画，关掉时 `drop_image`。
  - 缩放的四个动作由它自己处理（`ImagePreview` 上下文：⌘= / ⌘- 放大缩小，⌘0 原图，⌘9 适合窗口）。
  - 打开后在同一次更新里把焦点交给它，快捷键才有落点。

### 端口转发

- **规则在 store，运行状态在 `ForwardManager`。** 管理器订阅 store：规则删了就停，设置变了就重启。转发不计入主机的连接状态。
- **认证问答**归属 `PromptOwner::Forward(id, 代次)`，对话框开头写明是哪条转发；取消问答就等于停止。
- **发现断线靠处理器被丢弃**，不靠 `Handler::disconnected`：russh 有时会跳过它。远程转发先连上本机目标，再 `accept` 通道。
- **本地和动态转发先监听再登录**，监听器跨重连保留。停止时要 `handle.disconnect`。停止中的运行占着自己的位置（`Link::stopping`），否则重启会撞上「端口已被占用」。
- **首次登录可以问，重连不问**：重连非交互，陌生主机直接答否；只对网络类错误重试。

### 外部 CLI、升级、发布、平台

- **外部 CLI 经正在运行的应用执行**：命令本身不读钥匙串、不开数据库。
  - 服务端一直监听，按「启用外部 CLI」开关拒绝。
  - Unix 套接字 0600，并用 `peer_cred` 核对 uid。
  - Windows 用命名管道：带 DACL 和 `FIRST_PIPE_INSTANCE`；客户端以 `SECURITY_IDENTIFICATION` 打开，并核对属主；用同步的 Win32 管道；关句柄时不 `DisconnectNamedPipe`。
  - exec 和传输都不许挂住：非交互，陌生主机直接答否，问题按固定策略回答。
  - `exec --json` 的输入输出都是 JSON，输出只用 ASCII（`ascii_json` 转义其余字符），出错也是 JSON：给 Windows PowerShell 用，绕开引号和代码页。不做 MCP：主流 Agent 都能执行命令，CLI 加 skill 已经够用。
  - UI 测试不监听真实套接字，`CliIntegration` 默认没有路径。
- **Windows**：
  - 发布版是 GUI 子系统，命令改由 `shellrs-cli.exe` 承担；PATH 里放的是副本，加进用户 PATH。
  - `main` 先用 `link_arguments` 认堡垒机的链接参数，认出来就不走 CLI。
  - Windows 只在 CI 上编译和测试：`windows.yml` 按 `paths` 触发，新加 `cfg(windows)` 代码的文件要加进那个列表。
- **在线升级**：
  - `new_with_services` 建的 `Updater` 没有服务（测试从不联网），生产路径再 `set_services` 加 `start`。
  - 重启走 `set_restart_path` 加 `restart`。Windows 不在退出过程中起进程，交给 `run` 返回之后的 `start_handed_over`。
- **发布走 tag** `v<版本>`（`release.yml`，需要人工批准，清单最后才上传）。`CHANGELOG.md` 那一节由改版本号的提交写，功能提交不改。一次性准备见 `docs/release.md`。
- **macOS 上关窗口是隐藏**（`hide_when_closed`），⌘Q 才退出。测试平台的 `hide()` 没实现，所以这条只挂在生产路径上。

### 终端

- **查找条必须是终端 `div` 的兄弟**，不能是子元素，否则输入会被发给 shell；它还要 `.occlude()`。
- **清屏只在本地模拟器里做**，全屏程序运行时不做。终端里的 ⌘K 是清屏，比全局的 `FocusSearch` 深一层，所以优先。
- **终端字体走全局量 `TerminalFont`**（终端模块不依赖 settings），格子在下一帧 prepaint 时按新尺寸重排。
- **终端颜色走全局量 `TerminalColors`**（`theme.rs`）：`settings::apply` 按当前外观设成那一栏选中的主题，系统外观变化时也重设。终端格子、边距、选区、链接、颜色查询（OSC 4 / 10 / 11 / 12）和设置里的两个预览都从它取，不用界面主题的 `background` / `foreground`；查找高亮仍用界面的 `warning`。主题的 `key` 存进 `settings.json`，发布后不能改；读不出或深浅不符的 key 退回该外观的默认。
- **界面配色由所选主题推出**（`settings/app_theme.rs`）：从 gpui-kit 默认的浅色或深色主题出发重新着色。灰色按它在默认背景与文字之间的位置，换到该主题背景与文字之间；默认里用作可读文字的灰色（对比度 ≥ 4.5）至少保留该主题正文对比度的 80%（最高 4.5）。`base.*` 按名字换成对应的 ANSI 色，其余彩色按色相换；`.light` 和彩色的 `*foreground` 删掉，交给 gpui-kit 自己推。ShellRS Light / Dark 和「界面跟随主题」关闭时都直接用 gpui-kit 的默认主题（`TerminalThemeSettings::app_theme`）。`settings::apply` 按名字（`theme_name`）判断要不要重建，把结果放进 `Theme` 的浅色或深色槽再 `Theme::change`。
- **链接**：正则找出的地址和 OSC 8 标出的都算，只认 http / https（`links.rs`）；OSC 8 的格子优先于正则。一直画成蓝色（终端主题的 ANSI 蓝，悬停时亮蓝；界面主题的 `link` 和正文同色）加下划线，⌘ / Ctrl 单击才打开，普通单击照旧选文字。
- **鼠标上报**（`mouse.rs` 是纯编码）：按住 Shift 不上报，右键始终是自己的菜单。写入走 `engine.write`，不算用户输入。
- **关键字高亮只叠加在显示上**（`highlight.rs`）。`snapshot()` 每帧对可见行把折行接成逻辑行，用 `regex` crate 匹配，结果写进 `TerminalCell.highlight`，prepaint 改文字色和粗细；不存坐标，所以缩放重排、回滚都不用失效处理。
  - 不用 alacritty 的 `RegexSearch`（查找、链接用的那套）：它的 DFA 编不了 `\b`，`^` 只在第一行生效，`$` 会碰到行尾补的空格。
  - 通知在解析线程上：按 `\n` 切开输出，在 LF **之前**读光标所在的逻辑行（这时 `\r` 和颜色都已生效），锁外匹配。备用屏和同步更新进行中不读。
  - 规则由 `settings::apply` 编译成全局量 `TerminalHighlights`，引擎 `observe_global` 后换进与解析线程共用的槽；重启运行时也要把槽传过去。
  - `settings.json` 里读不了的规则单条丢掉（`readable_rules`），否则整个设置文件会回退成默认值。
  - 规则表的输入框和取色器状态归 `HighlightRulesEditor`（行 id 不随位置变，拖动后字段跟着走），每次改动整表写回设置；设置被别处改了才重建行。
- **同步更新（DEC 2026）要解析线程自己收尾**：vte 只管攒住 `\e[?2026h` 之后的输出，超时（150 ms）后调 `stop_sync` 是调用方的事，alacritty 自己的事件循环就是这么做的。解析线程等输出时带着截止时间（`receive`，用 `async_io` 的定时器），到点就画出来；输出一直不停时在 `advance` 前补查；退出、出错追加提示前也先 `stop_sync`。否则程序死在一帧中间、连接断在一帧中间时，屏幕会一直冻住。
- **OSC 52 只写不读**（`Osc52::OnlyCopy`），只认剪贴板 `c`，主选择区忽略。
- **通知**：OSC 9 / 777 由 `terminal/notices.rs` 的扫描器在解析线程上认（alacritty 会丢掉），OSC 9 里「数字;」开头的是 ConEmu 的命令，不算通知。投递在 `workspace/notices.rs`：窗口不在前台走 GPUI 的 `show_system_notification`（tag 是 `terminal:local:<id>` / `terminal:remote:<id>`，点击回调按 tag 切标签），在前台走应用内通知。`shellrs::init` 里的 `set_app_identity` 用 bundle id，测试平台没有它就不发系统通知；macOS 上 `cargo run` 没有 bundle，看不到系统通知。

## 测试约定

- **一个测试程序。** UI 测试（`tests/ui/main.rs`，`cargo test --test ui`）按功能分模块，共用的夹具和假实现在 `support/`。只有一个模块用的助手就放在那个模块里。不要在 `tests/` 下另开测试文件：每个文件都要再链接一遍 GPUI。
- **全部注入。** `Workspace::new_with_services` 注入终端、SFTP、本地目录、测试连接和端口转发的假实现。测试不碰用户的服务器、钥匙串（用 `InMemorySecretStore`）、设置文件（用 `SettingsStore::in_memory()`）、真实套接字和网络。`HostStore::seed()` 是夹具：种子主机按插入顺序编号（`web-01` = 1 … `dev-box` = 6），分组也一样（生产 = 1、测试 = 2、开发 = 3）；`web-01` 和 `staging-api` 初始已连接，并打开了终端标签。
- **元素定位。** 被测试查询的元素带稳定的领域 id，自定义的 `div` 还要加 `.test_support()`。id 从源码或已有测试里找，这里不列。
- **不要用右键菜单驱动测试**：菜单项的 id 只是序号，`PopupMenu` 还会让测试报「leaked handles」。直接派发菜单会派发的那个动作。测试模块里显式导入类型，`use gpui_kit::*` 会遮蔽 `#[test]`。
- **异步与时序。**
  - 动作派发和 `set_active` 都是延迟执行的：先 `run_until_parked` 再 `render_frame`，然后断言。
  - 工作线程的事件要推进时钟（`advance_clock`）或用 `cx.wait_for` 等。
  - 夹具一律 `set_reduce_motion(true)`。
  - GPUI 只给激活的窗口发焦点事件，需要时先 `activate_window`。
  - 按上一帧尺寸重排的元素，要 `simulate_next_frame` 才会重画。
  - 对话框关掉后焦点是空的，接着派发动作前先点一下面板。
- **看不到的东西换个法子查。**
  - 被遮蔽的输入框读不到值，密码一律查 `InMemorySecretStore`。
  - 按钮的禁用状态用「是否还能取得焦点」判断。
  - ⌘ / Shift 单击用 `modified_click`。
  - 测试的文字系统每个字符都是 0.6 em（真实中文是 1 em），靠文字宽度撑出的布局问题在测试里看不出来。
- **单元测试起子进程一律经 `crate::testing`**（`sh`、`sh_accepts`、`no_forks()`），免得子进程继承别的测试刚建的套接字。
- **测试里的二进制常量**（PNG 等）用脚本生成，别手抄：抄错一个字节就解不开。
