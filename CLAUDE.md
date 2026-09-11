# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

shellr 是一个类似 Xshell / WinSCP 的 SSH 会话管理工具，基于 `gpui-kit` 0.6.1（GPUI + gpui-base + gpui-component）。第一版是**纯 mock UI**：不接网络，只有内存样例数据。界面文案用中文，标识符用英文。已确认的产品决定：WinSCP 式双栏文件浏览器是每个会话独立的「SFTP」Dock 标签页；暂不做传输队列和 Dock 布局持久化。

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
```

工具链：`rust-toolchain.toml` 固定 Rust 1.98.1，因为 `gpui-pre` 用到了 1.94 尚未稳定的 std API。`Cargo.lock` 把 `gpui-pre*` 系列固定在 0.3.2（gpui-kit 0.6.1 发布时配套的版本），不要随手 `cargo update` 它们，除非确认仍能编译。`cargo test` 需要 gpui-kit 的 `test-support` 特性，首次会再编译一遍（同样慢）。

本终端的 `screencapture` 被 macOS 权限拦截，视觉检查只能靠 UI 集成测试或让用户看窗口。

## 架构

单个二进制 crate，带 `src/lib.rs`，这样 `tests/ui.rs` 能驱动生产环境的 `Workspace`。模块按能力划分，将来可拆成独立 crate；功能模块不得依赖 `workspace`（窗口壳），也不得触碰彼此的内部实现。

- `app/` — `actions.rs` 定义全部用户命令（`gpui_kit::actions!` 单元动作 + 携带 `SessionId` 的 `session_action!` 动作）；`mod.rs` 绑定快捷键、初始化 gpui-kit 并 `set_locale("zh-CN")`；`assets.rs` 用 `icon_assets!` 把额外的 Lucide 图标并入默认图标包——额外图标用 `CatalogIcon::*`，默认包用 `IconName::*`。
- `session/` — `model.rs`（Session / Group / Draft）、`store.rs`（`SessionStore` 实体，**唯一事实来源**）、`outline.rs`（纯函数：store → `TreeItem`，节点 id 形如 `g:<id>` / `s:<id>`）、`session_panel.rs`（左侧 Dock 面板：搜索 + 树 + 右键菜单）、`session_dialog.rs`（`SessionForm`、`open_session_dialog`、`confirm_delete_session`）。
- `terminal/` — `mock_shell.rs`（纯函数，预制回复）与 `terminal_panel.rs`（中间区标签页）。
- `explorer/` — `model.rs`（纯内存 `DirTree` / `Location` 导航）、`mock_fs.rs`（种子目录树）、`file_listing.rs`（`TableDelegate`）、`file_pane.rs`（单栏：地址行 + `DataTable` + 底部汇总）、`explorer_panel.rs`（`h_resizable` 里的两个栏）。
- `workspace/` — `workspace_view.rs` 持有 `SessionStore`、`DockArea`、按会话登记的面板注册表，以及**全部动作处理器**；`title_bar.rs`、`status_bar.rs`、`recent_sessions.rs`（中间区没有标签页时显示的「最近连接」开始页，**不是** Dock 面板）、`dock_skin.rs`（`WorkspaceDockSkin`：包一层 `DockSkin`，中间区为空时用 `deferred` 把开始页画在空的中间区之上；工作区在 `DockEvent::LayoutChanged` 时同步「中间区是否为空」并在刚变空时把焦点移到开始页）。
- `shared/` — 多个功能共用的展示片段（`ClosableTabTitle`）。

关键流程与不变量：

- **一个命令一个处理器。** 按钮、菜单、右键菜单、快捷键都只派发 `app/actions.rs` 里的动作，由 `Workspace` 处理。新增命令加在那里，不要写临时闭包直接改状态。
- **状态经由 store 流动。** 面板和对话框拿到 `Entity<SessionStore>` 的克隆，通过它的方法修改（方法内部会 `notify`），消费方用 `cx.observe(&store, ..)`。状态栏得知当前会话的路径是 `BasePanel::set_active` → `store.set_active`；`on_removed` 负责复位状态并发出 `TerminalPanelEvent::Closed` / `ExplorerPanelEvent::Closed`，工作区据此清理注册表。
- **Dock 面板**需实现 `EventEmitter<PanelEvent> + Focusable + Render + BasePanel + Panel`，一律用 `panel_handle(...)` 包装；`tab_name` 保持 `None`，由 `title` 返回富元素——标签页的图标和「×」关闭按钮就是这样来的（`ClosableTabTitle` 的「×」只派发面板的关闭动作，见下一条）。`panel_name()` 是持久化键，一经选定不可更改。
- **标签页全部可关。** `TabGroup::close_panel` 会拒绝关闭区域内最后一个面板，所以关闭不走 tab group：「×」派发 `CloseTerminal` / `CloseExplorer`，⌘W 派发 `CloseActiveTab`（关工作区记录的最近显示的中间标签，面板在 `set_active(true)` 时发出 `Activated` 事件告知），处理器一律用 `DockArea::remove_panel` 移除。中间区空了就显示最近连接列表：`SessionStore::recent_sessions()` 按「最近一次变为已连接」的顺序给出，`set_state` 变为 Connected 时把会话移到最前。
- **焦点规则。** `window.dispatch_action` 只能到达焦点元素路径上的处理器，所以工作区内部必须有东西持有焦点（启动时聚焦会话面板）。永远不要聚焦工作区根节点的 handle：对话框层是它的子元素，祖先持有焦点会让对话框的焦点陷阱拿不到焦点，导致「取消 / 创建」无响应。标题栏和开始页的按钮改用工作区 handle 上的 `FocusHandle::dispatch_action` 派发。对话框打开后焦点停在对话框宿主上，直到用户点击某个字段；延迟或下一帧的聚焦请求不会生效。
- **树的语义。** 树行的左键按下会同时选中并切换展开（无法抑制）；双击打开靠 `ListItem` 上的 `ClickEvent::click_count() == 2`。`TreeState::set_items` 会清空选择，`SessionPanel::rebuild_tree` 按 id 恢复选择，展开状态由它自己的 `expanded` 集合维护。
- **渲染回调不得读取实体。** 树行渲染闭包和 `render_td` 运行在所属实体的 render 过程中，只能传入普通快照（例如已连接会话的 `Rc<HashSet<SessionId>>`）。
- `px(..)` 只允许出现在 API 要求 `Pixels` 的地方（窗口 / Dock / 分栏尺寸、表格列宽、缩放字号），其余一律用 rem 助手和 `cx.theme()` token。

## 测试约定

UI 测试（`tests/ui.rs`）在无头窗口里渲染真实的 `Workspace`，通过 `gpui_kit::test::TestWindowExt`（`click`、`double_click`、`input`、`find`、`within`）驱动。测试模块里要显式导入类型，`use gpui_kit::*` 会遮蔽 `#[test]`。被测试查询的元素带稳定的领域 id，自定义 `div` 还要加 `.test_support()`：`("session-row", id)`、`("terminal", id)`、`("explorer", id)`、`("local-pane"|"remote-pane", id)`、`("close-terminal", id)`、`("close-explorer", id)`、`recent-sessions`、`("recent-session", id)`、`recent-new-session`、`session-search`、`new-session`、`theme-toggle`、`status-connection`、`commit`、`form-error`、`session-name` / `-host` / `-port` / `-user`、`remote-path`。动作派发和 `set_active` 都是延迟执行的：先退出 `update_window`，调用 `cx.run_until_parked()`，再 `render_frame` 后断言。种子会话 id 按插入顺序（`web-01`=1 … `dev-box`=6）；`web-01` 与 `staging-api` 初始为已连接并已打开终端标签。
