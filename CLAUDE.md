# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

shellr 是一个类似 Xshell / WinSCP 的 SSH 会话管理工具，基于 `gpui-kit` 0.6.1（GPUI + gpui-base + gpui-component）。界面文案用中文，标识符用英文。

会话和分组是**真实持久化**的，存在一个 SQLite 文件里（`~/Library/Application Support/shellr/shellr.db`，`SHELLR_DATA_DIR` 可覆盖目录）。本地终端也是真的（`portable-pty` + `alacritty_terminal`）。**仍然是 mock 的**：SSH 远程连接（「连接」只是把会话标成已连接并开一个预制回复的标签页）和 SFTP 文件浏览器（内存里的种子目录树）。

已确认的产品决定：WinSCP 式双栏文件浏览器是每个会话独立的「SFTP」Dock 标签页；暂不做传输队列和 Dock 布局持久化；首次启动是空库，不预置任何分组或会话；分组支持任意层级嵌套，会话也可以不属于任何分组（渲染在树的根层级）；删除分组会连同其子分组和里面的会话一起删，删除前确认并写明数量。

做 UI 之前先读 `gpui-kit` 与 `gpui-kit-design-guides` 两个技能（其中的 Coding Guides 和 Design Guides 在本仓库是规范，不是参考）。不要凭记忆臆造 gpui-kit API：以 `~/.cargo/registry/src/*/gpui-component-0.6.1/`、`gpui-base-0.6.1/` 的源码或 `https://gpui-kit.com/component/<name>.md` 为准。

## 常用命令

```bash
cargo run                                   # 启动应用
cargo build                                 # 首次构建会编译整个 GPUI 栈，很慢
cargo test                                  # 单元测试（模块内）+ tests/ui.rs（无头窗口 UI 集成测试）
cargo test --lib                            # 只跑单元测试
cargo test --test ui                        # 只跑 UI 集成测试
cargo test --test ui new_session -- --nocapture   # 按名称片段跑单个 UI 测试
cargo clippy --all-targets -- -D warnings   # 必须无警告
cargo fmt --check

sqlite3 ~/Library/Application\ Support/shellr/shellr.db '.schema'   # 查落盘结果
```

工具链：`rust-toolchain.toml` 固定 Rust 1.98.1，因为 `gpui-pre` 用到了 1.94 尚未稳定的 std API。`Cargo.lock` 把 `gpui-pre*` 系列固定在 0.3.2（gpui-kit 0.6.1 发布时配套的版本），不要随手 `cargo update` 它们，除非确认仍能编译。`cargo test` 需要 gpui-kit 的 `test-support` 特性，首次会再编译一遍（同样慢）。

本终端的 `screencapture` 被 macOS 权限拦截，视觉检查只能靠 UI 集成测试或让用户看窗口。

## 架构

单个二进制 crate，带 `src/lib.rs`，这样 `tests/ui.rs` 能驱动生产环境的 `Workspace`。模块按能力划分，将来可拆成独立 crate；功能模块不得依赖 `workspace`（窗口壳），也不得触碰彼此的内部实现。

- `app/` — `actions.rs` 定义全部用户命令（`gpui_kit::actions!` 单元动作 + 携带 `SessionId` / `GroupId` 的 `session_action!` / `group_action!` 动作）；`paths.rs` 解析数据目录（`SHELLR_DATA_DIR` → `dirs::data_dir()/shellr`）与数据库路径；`mod.rs` 绑定快捷键、初始化 gpui-kit 并 `set_locale("zh-CN")`；`assets.rs` 用 `icon_assets!` 把额外的 Lucide 图标并入默认图标包——额外图标用 `CatalogIcon::*`，默认包用 `IconName::*`。
- `session/` — `model.rs`（Session / SessionGroup / SessionDraft / GroupDraft；`Session.group` 与 `SessionGroup.parent` 都是 `Option<GroupId>`）、`database.rs`（`SessionDatabase`：rusqlite 连接、`PRAGMA user_version` 迁移、行与模型的映射）、`store.rs`（`SessionStore` 实体，**内存是唯一事实来源**，写穿到数据库）、`outline.rs`（纯函数：store → `TreeItem`，递归铺开嵌套分组；节点 id 形如 `g:<id>` / `s:<id>`；`group_options` 给表单提供按全路径标注的分组列表）、`session_panel.rs`（左侧 Dock 面板：搜索 + 树 + 右键菜单）、`session_dialog.rs`、`group_dialog.rs`（`GroupForm`、`open_group_dialog`、`confirm_delete_group`）。
- `terminal/` — 本地终端是真的：`transport.rs`（字节流传输的 trait）、`local_pty.rs`（`portable-pty` 实现，跑在专用线程上）、`engine.rs`（驱动 `alacritty_terminal`，解析线程 + 16ms UI 轮询批量刷新）、`terminal_view.rs`（网格渲染、选区、输入编码）、`local_terminal_panel.rs`（⌘T 开的本地标签页）、`model.rs`。远程会话仍是 mock：`mock_shell.rs`（纯函数，预制回复）、`mock_transport.rs` 与 `terminal_panel.rs`（中间区标签页）。
- `explorer/` — `model.rs`（纯内存 `DirTree` / `Location` 导航）、`mock_fs.rs`（种子目录树）、`file_listing.rs`（`TableDelegate`）、`file_pane.rs`（单栏：地址行 + `DataTable` + 底部汇总）、`explorer_panel.rs`（`h_resizable` 里的两个栏）。
- `workspace/` — `workspace_view.rs` 持有 `SessionStore`、`DockArea`、按会话登记的面板注册表，以及**全部动作处理器**；`title_bar.rs`、`status_bar.rs`、`recent_sessions.rs`（中间区没有标签页时显示的「最近连接」开始页，**不是** Dock 面板）、`dock_skin.rs`（`WorkspaceDockSkin`：包一层 `DockSkin`，中间区为空时用 `deferred` 把开始页画在空的中间区之上；工作区在 `DockEvent::LayoutChanged` 时同步「中间区是否为空」并在刚变空时把焦点移到开始页）。
- `shared/` — 多个功能共用的展示片段（`ClosableTabTitle`）。

关键流程与不变量：

- **一个命令一个处理器。** 按钮、菜单、右键菜单、快捷键都只派发 `app/actions.rs` 里的动作，由 `Workspace` 处理。新增命令加在那里，不要写临时闭包直接改状态。
- **持久化写穿，内存说了算。** `SessionStore` 带 `Context` 的 mutator 负责「改内存 → 写库 → `notify`」，`*_unnotified` 那一半是纯内存的（给单元测试和加载用）。写库同步发生在 UI 线程：要落盘的都是用户在对话框里确认的单行写入，不是流。写失败时内存里的改动照样生效，store 发 `SessionStoreEvent::PersistFailed`，工作区订阅后弹一个错误通知——**绝不静默吞掉**。`ConnectionState` 是运行时状态，不入库；`last_connected_at` 入库，开始页的「最近连接」因此能跨启动。删除分组靠数据库两个外键的 `ON DELETE CASCADE`，内存里的 `remove_group_unnotified` 必须给出一样的结果，并把被删会话的 id 返回给工作区去关标签页。
- **id 由内存分配。** `next_session_id` / `next_group_id` 两个独立计数器，`SessionStore::load` 把它们置为库里最大 id + 1。`SessionStore::seed()` 生产环境不再使用，是 `tests/ui.rs` 的夹具。
- **状态经由 store 流动。** 面板和对话框拿到 `Entity<SessionStore>` 的克隆，通过它的方法修改（方法内部会 `notify`），消费方用 `cx.observe(&store, ..)`。状态栏得知当前会话的路径是 `BasePanel::set_active` → `store.set_active`；`on_removed` 负责复位状态并发出 `TerminalPanelEvent::Closed` / `ExplorerPanelEvent::Closed`，工作区据此清理注册表。
- **Dock 面板**需实现 `EventEmitter<PanelEvent> + Focusable + Render + BasePanel + Panel`，一律用 `panel_handle(...)` 包装；`tab_name` 保持 `None`，由 `title` 返回富元素——标签页的图标和「×」关闭按钮就是这样来的（`ClosableTabTitle` 的「×」只派发面板的关闭动作，见下一条）。`panel_name()` 是持久化键，一经选定不可更改。
- **标签页全部可关。** `TabGroup::close_panel` 会拒绝关闭区域内最后一个面板，所以关闭不走 tab group：「×」派发 `CloseTerminal` / `CloseExplorer`，⌘W 派发 `CloseActiveTab`（关工作区记录的最近显示的中间标签，面板在 `set_active(true)` 时发出 `Activated` 事件告知），处理器一律用 `DockArea::remove_panel` 移除。中间区空了就显示最近连接列表：`SessionStore::recent_sessions()` 按「最近一次变为已连接」的顺序给出，`set_state` 变为 Connected 时把会话移到最前。
- **焦点规则。** `window.dispatch_action` 只能到达焦点元素路径上的处理器，所以工作区内部必须有东西持有焦点（启动时聚焦会话面板）。永远不要聚焦工作区根节点的 handle：对话框层是它的子元素，祖先持有焦点会让对话框的焦点陷阱拿不到焦点，导致「取消 / 创建」无响应。标题栏和开始页的按钮改用工作区 handle 上的 `FocusHandle::dispatch_action` 派发。对话框打开后焦点停在对话框宿主上，直到用户点击某个字段；延迟或下一帧的聚焦请求不会生效。中间区一次只渲染激活的那个标签页，所以每个面板都必须在 `set_active(true)` 里把焦点收下：失活的标签页要是还攥着焦点，它的 handle 就不在这一帧的派发树里，「×」、⌘W 和所有快捷键会一起静默失效（见 `both_tabs_of_one_session_stay_closable`）。
- **树的语义。** 树行的左键按下会同时选中并切换展开（无法抑制）；双击打开靠 `ListItem` 上的 `ClickEvent::click_count() == 2`。`TreeState::set_items` 会清空选择，`SessionPanel::rebuild_tree` 按 id 恢复选择，展开状态由它自己的 `expanded` 集合维护。对话框新建出来的节点靠 `SessionPanel` 比对 `known_groups` / `known_sessions` 快照来发现，然后展开祖先链并选中——对话框不需要回调面板。
- **树的右键菜单挂在容器上，不挂在行上。** gpui-component 的 `ContextMenu` 在 `request_layout` 里给弹出菜单抢焦点，而树行是 `uniform_list` 在 prepaint 阶段才渲染的：焦点于是在一帧的中途易主，gpui 的无障碍断言会 panic（`set_focus called more than once in a single frame`，只在开了辅助功能的调试构建里出现）。所以 `SessionPanel` 把 `.context_menu(..)` 挂在 `#session-tree` 容器上，右键命中的节点由行上的右键处理器写进一个 `Rc<Cell<Option<SessionNode>>>`（容器在捕获阶段先清空它，空白处右键因此得到根菜单）。gpui-kit 自己的 `Table` 也是这么挂的。
- **对话框关闭后焦点会丢。** 这是 gpui-component 的既有行为，不是本仓库引入的：关掉对话框后 `window.dispatch_action` 没有落点，要先点一下面板里的东西。UI 测试里同理（见 `groups_and_sessions_are_read_back_from_the_database`）。
- **渲染回调不得读取实体。** 树行渲染闭包和 `render_td` 运行在所属实体的 render 过程中，只能传入普通快照（例如已连接会话的 `Rc<HashSet<SessionId>>`）。
- `px(..)` 只允许出现在 API 要求 `Pixels` 的地方（窗口 / Dock / 分栏尺寸、表格列宽、缩放字号），其余一律用 rem 助手和 `cx.theme()` token。

## 测试约定

UI 测试（`tests/ui.rs`）在无头窗口里渲染真实的 `Workspace`，通过 `gpui_kit::test::TestWindowExt`（`click`、`double_click`、`input`、`press`、`find`、`within`）驱动。`Workspace::new` 要求外部传入 `Entity<SessionStore>`：生产环境由 `main.rs` 从数据库加载，测试用 `open_workspace_with_store` 注入 `SessionStore::seed()`（或一个临时文件上的真库）。**不要用右键菜单驱动测试**：菜单项的 ElementId 只是序号，而且 `PopupMenu` 实体会让测试以「leaked handles」失败；直接 `window.dispatch_action` 派发菜单会派发的那个动作。测试模块里要显式导入类型，`use gpui_kit::*` 会遮蔽 `#[test]`。被测试查询的元素带稳定的领域 id，自定义 `div` 还要加 `.test_support()`：`("session-row", id)`、`("group-row", id)`、`("terminal", id)`、`("explorer", id)`、`("local-pane"|"remote-pane", id)`、`("close-terminal", id)`、`("close-explorer", id)`、`recent-sessions`、`("recent-session", id)`、`recent-new-session`、`session-search`、`new-session`、`new-group`、`theme-toggle`、`status-connection`、`commit`、`ok`（确认对话框）、`form-error`、`session-name` / `-host` / `-port` / `-user`、`group-name`、`remote-path`。动作派发和 `set_active` 都是延迟执行的：先退出 `update_window`，调用 `cx.run_until_parked()`，再 `render_frame` 后断言。种子会话 id 按插入顺序（`web-01`=1 … `dev-box`=6），种子分组同理（`生产`=1、`测试`=2、`开发`=3）；`web-01` 与 `staging-api` 初始为已连接并已打开终端标签。
