# 仓库贡献指南

## 项目是什么

ShellRS（crate 与二进制都叫 `shellrs`）是一个类似 Xshell / WinSCP 的 SSH 主机管理工具，基于 `gpui-kit` 0.6.1（GPUI + gpui-base + gpui-component）。界面文案用中文，标识符用英文。

**保存的一项叫「主机」，代码里叫 `Host`。** 左侧列表里保存的一项（地址、端口、用户、认证方式）在界面上叫「主机」，量词用「台」，代码和数据库里一律叫 host（`Host`、`HostId`、`HostStore`、`host/` 模块、`hosts` 表）。它以前叫「会话」/ `Session`，界面、代码和表名都已改掉，新写的文案和标识符不要再用「会话」/ session 指它。填 IP / 域名的那个字段叫「地址」（`Host.address`、`hosts.address`），不叫「主机」（「主机密钥」「首次连接此主机」这些照旧）。SSH 协议里的 session 不在此列：「SSH 会话通道」、russh 的 `client::Session` / `server::Session`、`channel_open_session`、SFTP 会话照旧。

主机和分组是**真实持久化**的，存在一个 SQLite 文件里（`~/Library/Application Support/shellrs/shellrs.db`，`SHELLRS_DATA_DIR` 可覆盖目录）。本地终端（`portable-pty` + `alacritty_terminal`）和 SSH 远程连接（`russh`）也都是真的。密码与私钥口令存在系统钥匙串里（`keyring`），**数据库里永远不出现秘密**。SFTP 双栏浏览与上传也已接入真实文件系统，支持断点续传；下载、目录同步和传输队列尚未实现。详见 `README.md`。

已确认的产品决定：WinSCP 式双栏文件浏览器是每个主机独立的「SFTP」Dock 标签页；暂不做传输队列和 Dock 布局持久化；首次启动是空库，不预置任何分组或主机；分组支持任意层级嵌套，主机也可以不属于任何分组（渲染在树的根层级）；删除分组会连同其子分组和里面的主机一起删，删除前确认并写明数量。

做 UI 之前先读 `gpui-kit` 与 `gpui-kit-design-guides` 两个技能（其中的 Coding Guides 和 Design Guides 在本仓库是规范，不是参考）。不要凭记忆臆造 gpui-kit API：以 `~/.cargo/registry/src/*/gpui-component-0.6.1/`、`gpui-base-0.6.1/` 的源码或 `https://gpui-kit.com/component/<name>.md` 为准。

## 常用命令

```bash
cargo run                                   # 启动应用
cargo build                                 # 首次构建会编译整个 GPUI 栈，很慢
cargo test                                  # 单元测试（模块内）+ tests/ui/（无头窗口 UI 集成测试）
cargo test --lib                            # 只跑单元测试
cargo test --test ui                        # 只跑 UI 集成测试
cargo test --test ui new_host -- --nocapture   # 按名称片段跑单个 UI 测试
cargo clippy --all-targets -- -D warnings   # 必须无警告
cargo fmt --check

sqlite3 ~/Library/Application\ Support/shellrs/shellrs.db '.schema'   # 查落盘结果
security find-generic-password -s shellrs -a "password:<user>@<host>:<port>"   # 查钥匙串条目
cargo test -- --ignored                     # 会读写真实钥匙串的测试，默认跳过
```

工具链：`rust-toolchain.toml` 固定 Rust 1.98.1，因为 `gpui-pre` 用到了 1.94 尚未稳定的 std API。`Cargo.lock` 把 `gpui-pre*` 系列固定在 0.3.2（gpui-kit 0.6.1 发布时配套的版本），不要随手 `cargo update` 它们，除非确认仍能编译。`cargo test` 需要 gpui-kit 的 `test-support` 特性，首次会再编译一遍（同样慢）。

本终端的 `screencapture` 被 macOS 权限拦截，视觉检查只能靠 UI 集成测试或让用户看窗口。

## 架构

单个二进制 crate，带 `src/lib.rs`，这样 `tests/ui/` 能驱动生产环境的 `Workspace`。模块按能力划分，将来可拆成独立 crate；功能模块不得依赖 `workspace`（窗口壳），也不得触碰彼此的内部实现。

- `app/` — `actions.rs` 定义全部用户命令（`gpui_kit::actions!` 单元动作 + 携带 `HostId` / `GroupId` 的 `host_action!` / `group_action!` 动作）；`paths.rs` 解析数据目录（`SHELLRS_DATA_DIR` → `dirs::data_dir()/shellrs`）与数据库路径；`mod.rs` 绑定快捷键、初始化 gpui-kit 并 `set_locale("zh-CN")`；`assets.rs` 用 `icon_assets!` 把额外的 Lucide 图标并入默认图标包——额外图标用 `CatalogIcon::*`，默认包用 `IconName::*`；`OS_ICONS` 另外嵌入 `assets/icons/os/*.svg`（各操作系统的真实 logo，取自 Simple Icons，CC0；Windows 那个是自己画的四格，Simple Icons 已下架），用 `Icon::default().path(os.icon_path())` 渲染。
- `host/` — `model.rs`（Host / HostGroup / HostDraft / GroupDraft / `HostOs`；`Host.group` 与 `HostGroup.parent` 都是 `Option<GroupId>`，`Host.os` 是 `Option<HostOs>`）、`database.rs`（`HostDatabase`：rusqlite 连接、`PRAGMA user_version` 迁移、行与模型的映射）、`store.rs`（`HostStore` 实体，**内存是唯一事实来源**，写穿到数据库）、`outline.rs`（纯函数：store → `TreeItem`，递归铺开嵌套分组；节点 id 形如 `g:<id>` / `s:<id>`；`group_options` 给表单提供按全路径标注的分组列表）、`host_panel.rs`（左侧 Dock 面板：搜索 + 树 + 右键菜单）、`host_dialog.rs`、`group_dialog.rs`（`GroupForm`、`open_group_dialog`、`confirm_delete_group`）。
- `terminal/` — 本地终端是真的：`transport.rs`（字节流传输的 trait）、`local_pty.rs`（`portable-pty` 实现，跑在专用线程上）、`engine.rs`（驱动 `alacritty_terminal`，解析线程 + 16ms UI 轮询批量刷新）、`terminal_view.rs`（网格渲染、选区、输入编码）、`local_terminal_panel.rs`（⌘T 开的本地标签页）、`model.rs`。远程主机的传输在 `ssh/`，中间区的标签页是 `terminal_panel.rs`。共享 prompt 协议位于 `connection.rs`，`transport.rs` 通过 `TerminalPrompt` / `TerminalPromptKind` / `TerminalSecret` 重导出保持兼容。
- `ssh/` — `probe.rs`（探测远端系统的命令与纯解析函数，外加驱动两步探测的 `HostOsProbe` 状态机）、`transport.rs`：真实的 russh 客户端。known_hosts 校验写 `<data_dir>/known_hosts`（**不碰** `~/.ssh/known_hosts`），认证链是 agent → publickey → password → keyboard-interactive。密码和私钥口令先查 `secrets`，查不到或被服务器拒绝才走 prompt 弹框；被拒绝时**不删**钥匙串条目，只在弹框说明里讲清原因。
- `secrets/` — 系统钥匙串（`keyring` 4：macOS Keychain / Windows 凭据管理器 / Secret Service）。`SecretRef` 决定归属：密码按 `user@host:port`（改名、复制主机都不丢，同一台机器的多个主机共用一条），私钥口令按文件路径（同一把钥匙只问一次），密码凭据的密码按凭据随机生成、永不复用的 `keychain_id`（`credential:<id>`）。`SecretStore` 的三个方法全是阻塞的，**只能**在后台执行器或传输层工作线程上调用。`InMemorySecretStore` 给测试，`NoSecretStore` 给没有钥匙串的机器（界面据此禁用密码字段）。
- `explorer/` — 目录与选择快照、`DataTable` 双栏、上传确认 / 冲突对话框、进度和取消 / 恢复展示。`ExplorerAction` 统一路由到工作区；文件扫描与网络操作在后台，目录响应按请求编号丢弃过期结果。
- `sftp/` — 可注入传输与本地目录接口、`RemotePath`、russh-sftp 3.0.0 协议适配、有界并发分块上传、UUID 临时文件、安全替换、原子 JSON 续传记录和三次自动重连。`ssh/connection.rs` 的 `SshConnector` 与终端共享认证、钥匙串服务及 known_hosts 写锁。
- `workspace/` — `workspace_view.rs` 持有 `HostStore`、`DockArea`、按主机登记的面板注册表，以及**全部动作处理器**；`title_bar.rs`、`status_bar.rs`、`recent_hosts.rs`（中间区没有标签页时显示的「最近连接」开始页，**不是** Dock 面板）、`dock_skin.rs`（`WorkspaceDockSkin`：包一层 `DockSkin`，中间区为空时用 `deferred` 把开始页画在空的中间区之上；工作区在 `DockEvent::LayoutChanged` 时同步「中间区是否为空」并在刚变空时把焦点移到开始页）。
- `update/` — 在线升级：`feed.rs` 从 `dl.shellrs.com` 取已签名的清单和安装包（reqwest，走系统代理），`verify.rs` 先验 minisign 签名（trusted comment 绑定通道和版本）再信清单、按大小和 SHA-256 信安装包，`install*.rs` 按安装方式装（macOS 整包交换、Windows 运行 Inno 安装程序、Linux 覆盖 AppImage），`updater.rs` 的 `Updater` 实体管检查、下载和重启，工作线程的事件由 UI 定时轮询。不依赖 `workspace` 和 `settings`；`new_with_services` 里的 `Updater` 没有服务、从不联网，生产路径才 `set_services` + `start`，UI 测试经 `workspace.updater()` 注入假的。详见 `CLAUDE.md` 和 `README.md` 的「下载与更新」「发布」。
- `shared/` — 多个功能共用的展示片段（`ClosableTabTitle`、`HostMark`）。

关键流程与不变量：

- **SFTP 独立于终端。** 主机状态综合所有终端与 SFTP 状态计算；关闭终端不停止上传，断开 / 删除主机会停止上传并保留续传数据。每主机一个运行批次，来源、端点和目标在确认时固定。认证问答按终端 / SFTP 来源与 SFTP 连接代次隔离。
- **SFTP UI 测试完全注入。** 使用 `Workspace::new_with_services` 注入目录与传输，不访问用户服务器和钥匙串。工作线程事件由 UI 定时轮询，禁止后台线程直接唤醒 GPUI 前台任务；实体回调中的 SFTP 动作通过 `ExplorerDispatch` 延后派发，避免重入实体。

- **一个命令一个处理器。** 按钮、菜单、右键菜单、快捷键都只派发 `app/actions.rs` 里的动作，由 `Workspace` 处理。新增命令加在那里，不要写临时闭包直接改状态。
- **持久化写穿，内存说了算。** `HostStore` 带 `Context` 的 mutator 负责「改内存 → 写库 → `notify`」，`*_unnotified` 那一半是纯内存的（给单元测试和加载用）。写库同步发生在 UI 线程：要落盘的都是用户在对话框里确认的单行写入，不是流。写失败时内存里的改动照样生效，store 发 `HostStoreEvent::PersistFailed`，工作区订阅后弹一个错误通知——**绝不静默吞掉**。`ConnectionState` 是运行时状态，不入库；`last_connected_at` 入库，开始页的「最近连接」因此能跨启动。删除分组靠数据库两个外键的 `ON DELETE CASCADE`，内存里的 `remove_group_unnotified` 必须给出一样的结果，并把被删主机的 id 返回给工作区去关标签页。
- **主机系统每次连上都重探一遍。** 认证过、shell 起来、`Started` 发出去之后，传输层另开一条 exec 通道跑 `uname -s; cat /etc/os-release`，在主 IO 循环里和 shell 的输出一起轮询，所以终端不会为了探测多等一个往返。POSIX 那条什么都没输出（Windows 的 shell 就是这样）时再问一次 `cmd /c ver`，还是不认就放弃，主机保留原来的标记。结果沿 `TerminalTransportEvent::HostOsDetected` → 引擎 → `TerminalView` → `TerminalPanel` → 工作区 → `HostStore::set_host_os` 这条链走，和 prompt 是同一套管线。`os` 列由 `set_host_os` 单独写，`update_host` 不碰它（同 `last_connected_at`）。标记是 `shared::HostMark`，主机树和开始页共用：探到了就用该系统的品牌色打底、白色（浅色品牌用黑色）画 logo，没探到就退回 `muted` 底色加主机名称第一个字。徽章沿用 `Avatar` 的圆形画法，尺寸走 `Sizable`——树里用 `small`（20px），开始页用默认的 `medium`（32px）。`AlmaLinux` 和 `macOS` 没有单一品牌色，改用 `foreground` / `background`，这也是 Apple 官方的画法。品牌色写在 `host_os!` 表里而不是渲染处，那是设计规范允许的「颜色本身即数据」那一档。主机树里分组行和其他行用 `plain_mark` 占同样宽度的槽位，否则标签对不齐。
- **秘密只进钥匙串，不进数据库。** `database.rs` 的 `schema_never_contains_secret_columns` 守着这条线。写入的唯一入口是 `HostStore::save_secret`：它在后台线程上调钥匙串，失败同样走 `PersistFailed`。主机换了端点或被删掉时，store 用 `password_in_use`（按查好凭据之后的登录快照 `HostLogin` 判断）看还有没有别的主机在用旧端点，没有才删条目；私钥口令按文件共享，从不自动删。主机对话框里的密码字段是异步预填的，`*_loaded` 标志没置上之前空字段不算「用户清空了」，所以预填还没回来就点保存不会误删。秘密绝不放进 `HostDraft`，它派生了 `Debug`。
- **id 由内存分配。** `next_host_id` / `next_group_id` 两个独立计数器，`HostStore::load` 把它们置为库里最大 id + 1。`HostStore::seed()` 生产环境不再使用，是 UI 测试的夹具。
- **状态经由 store 流动。** 面板和对话框拿到 `Entity<HostStore>` 的克隆，通过它的方法修改（方法内部会 `notify`），消费方用 `cx.observe(&store, ..)`。状态栏得知当前主机的路径是 `BasePanel::set_active` → `store.set_active`；`on_removed` 负责复位状态并发出 `TerminalPanelEvent::Closed` / `ExplorerPanelEvent::Closed`，工作区据此清理注册表。
- **Dock 面板**需实现 `EventEmitter<PanelEvent> + Focusable + Render + BasePanel + Panel`，一律用 `panel_handle(...)` 包装；`tab_name` 保持 `None`，由 `title` 返回富元素——标签页的图标和「×」关闭按钮就是这样来的（`ClosableTabTitle` 的「×」只派发面板的关闭动作，见下一条）。`panel_name()` 是持久化键，一经选定不可更改。
- **标签页全部可关。** `TabGroup::close_panel` 会拒绝关闭区域内最后一个面板，所以关闭不走 tab group：「×」派发 `CloseTerminal` / `CloseExplorer`，⌘W 派发 `CloseActiveTab`（关工作区记录的最近显示的中间标签，面板在 `set_active(true)` 时发出 `Activated` 事件告知），处理器一律用 `DockArea::remove_panel` 移除。中间区空了就显示最近连接列表：`HostStore::recent_hosts()` 按「最近一次变为已连接」的顺序给出，`set_state` 变为 Connected 时把主机移到最前。
- **焦点规则。** `window.dispatch_action` 只能到达焦点元素路径上的处理器，所以工作区内部必须有东西持有焦点（启动时聚焦主机面板）。永远不要聚焦工作区根节点的 handle：对话框层是它的子元素，祖先持有焦点会让对话框的焦点陷阱拿不到焦点，导致「取消 / 创建」无响应。标题栏和开始页的按钮改用工作区 handle 上的 `FocusHandle::dispatch_action` 派发。对话框打开后焦点停在对话框宿主上，直到用户点击某个字段；延迟或下一帧的聚焦请求不会生效。
- **树的语义。** 树行的左键按下会同时选中并切换展开（无法抑制）；双击打开靠 `ListItem` 上的 `ClickEvent::click_count() == 2`。`TreeState::set_items` 会清空选择，`HostPanel::rebuild_tree` 按 id 恢复选择，展开状态由它自己的 `expanded` 集合维护。对话框新建出来的节点靠 `HostPanel` 比对 `known_groups` / `seen_hosts` 快照来发现，然后展开祖先链并选中——对话框不需要回调面板。
- **对话框关闭后焦点会丢。** 这是 gpui-component 的既有行为，不是本仓库引入的：关掉对话框后 `window.dispatch_action` 没有落点，要先点一下面板里的东西。UI 测试里同理（见 `groups_and_hosts_are_read_back_from_the_database`）。
- **要彩色就不能用 `Icon`。** GPUI 的 `svg()` 走 `Window::paint_svg`，签名只收一个颜色，整张图当遮罩上色，所以彩色 SVG 塞进 `Icon` 会变成一坨纯色。真要彩色只能走 `img()`（`ImageSource`），那条路把 SVG 光栅化成 RGBA，颜色能留住，代价是变位图。系统徽章因此是「主题色的圆底 + 单色遮罩 logo」，不是一张彩图。
- **渲染回调不得读取实体。** 树行渲染闭包和 `render_td` 运行在所属实体的 render 过程中，只能传入普通快照（例如已连接主机的 `Rc<HashSet<HostId>>`）。
- `px(..)` 只允许出现在 API 要求 `Pixels` 的地方（窗口 / Dock / 分栏尺寸、表格列宽、缩放字号），其余一律用 rem 助手和 `cx.theme()` token。

## 测试约定

UI 测试（`tests/ui/`）在无头窗口里渲染真实的 `Workspace`，通过 `gpui_kit::test::TestWindowExt`（`click`、`double_click`、`input`、`press`、`find`、`within`）驱动。`Workspace::new` 要求外部传入 `Entity<HostStore>`：生产环境由 `main.rs` 从数据库加载，测试用 `open_workspace_with_store` 注入 `HostStore::seed()`（或一个临时文件上的真库）。**不要用右键菜单驱动测试**：菜单项的 ElementId 只是序号，而且 `PopupMenu` 实体会让测试以「leaked handles」失败；直接 `window.dispatch_action` 派发菜单会派发的那个动作。测试模块里要显式导入类型，`use gpui_kit::*` 会遮蔽 `#[test]`。被测试查询的元素带稳定的领域 id，自定义 `div` 还要加 `.test_support()`：`("host-row", id)`、`("host-os", id)` 与开始页的 `("recent-host-os", id)`（`aria_label` 是系统名，未探测时是「未探测到系统」）、`("group-row", id)`、`("terminal", id)`、`("explorer", id)`、`("local-pane"|"remote-pane", id)`、`("close-terminal", id)`、`("close-explorer", id)`、`recent-hosts`、`("recent-host", id)`、`recent-new-host`、`host-search`、`new-host`、`new-group`、`theme-toggle`、`status-connection`、`commit`、`ok`（确认对话框）、`form-error`、`host-name` / `host-address` / `host-port` / `-user` / `-auth` / `-key-path` / `-password` / `-passphrase`、`group-name`、`remote-path`。被遮蔽的输入框在测试里读不到值（`ElementSnapshot::value()` 返回 `None`），所以密码相关的断言一律查注入的 `InMemorySecretStore`，不查界面。动作派发和 `set_active` 都是延迟执行的：先退出 `update_window`，调用 `cx.run_until_parked()`，再 `render_frame` 后断言。种子主机 id 按插入顺序（`web-01`=1 … `dev-box`=6），种子分组同理（`生产`=1、`测试`=2、`开发`=3）；`web-01` 与 `staging-api` 初始为已连接并已打开终端标签。
