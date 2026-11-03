# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目是什么

ShellRS（crate 与二进制都叫 `shellrs`）是 Xshell / WinSCP 式的 SSH 主机管理工具，基于 `gpui-kit` 0.6.1（GPUI + gpui-base + gpui-component）。界面有简体中文和英文（中文是源语言），标识符用英文。功能说明见使用手册 `docs/manual.zh.md` / `docs/manual.md`；开源首页 `README.md`（英文）/ `README.zh-CN.md`；截图在 `docs/screenshots/`。

- **术语**：保存的一项叫「主机」（量词「台」），代码里叫 host（`Host`、`HostStore`、`hosts` 表）；以前叫「会话」/ `Session`，别再用。IP / 域名字段叫「地址」。SSH 协议层的 session 照旧叫 session。
- **两种不保存的连接**：「外部连接」是堡垒机用 `ssh://` / `-url` / `sftp://` 拉起 ShellRS，只开一个标签；「临时连接」从标题栏打开，功能齐全。代码里统称 temporary。
- **全是真的**：SQLite（默认在系统数据目录，`SHELLRS_DATA_DIR` 可覆盖，Windows 便携版在 exe 旁的 `data`）、本地终端、SSH（`russh`）、Telnet（自己写）、串口（`serial2`）、SFTP、端口转发、凭据、右侧栏工具、编辑器、外部 CLI、在线升级、匿名统计。密码和口令只进系统钥匙串，**数据库里永远不出现秘密**。

做 UI 之前先读 `gpui-kit` 与 `gpui-kit-design-guides` 两个技能，里面的 Coding Guides 和 Design Guides 在本仓库是规范。不要凭记忆臆造 gpui-kit API，以 `~/.cargo/registry/src/*/gpui-component-0.6.1/`、`gpui-base-0.6.1/` 的源码为准。

## 已定的产品决定

用户定过的事，不要改回去。细节以使用手册和代码为准。

- **界面语言**：跟随系统（默认）/ 简体中文 / English，即时生效；用 gpui-kit 自带的 `rust-i18n`。
- **SFTP**：Dock 标签，每次打开新开一个、各自连接；操作对标 WinSCP Commander，本地、远程两侧对称，本地删除进废纸篓。传输在标签底部的队列里排队，不弹前台进度框；小文件按 WinSCP 并行传，共用标签那条 SFTP 会话（不另登录，堡垒机上也能用）。文件图标默认彩色，设置里可换单色。「查找文件」只在远程侧，规则同 WinSCP，是单独的非模态窗口。暂不做目录同步、过滤、目录树。
- **标签各自记着打开时的主机**（同 Xshell、SecureCRT）：编辑、删除主机或分组都不碰已打开的标签（不重连、不改标题、不关）；没有主机级的连接状态，状态栏显示当前标签自己的连接。
- **主机树**：搜索匹配主机和分组名；分组可任意嵌套；删除分组连同子分组和主机。首次启动是空库。
- **导入主机**（issue #1）：从 `~/.ssh/config` 和 `~/.ssh/known_hosts` 导入，入口在主机侧栏的「…」菜单和空白处右键菜单（用得少，不占工具栏）。先预览再导入，只追加、不改已有主机；每行可改名称、分组、用户名和认证，状态写「新增」/「已存在」。ProxyJump、ProxyCommand 不导入；known_hosts 用户名填 root；非默认私钥建密钥凭据，私钥复制进 `keys/`（再导入同一个文件认得出那份副本），其余默认「无密码」。
- **主机对话框**：协议 SSH / Telnet / 串口，编辑时也能换（被当跳板或有转发经由的 SSH 主机除外）。认证三选一：密码（留空就每次问）/ 使用凭据 / 无密码（试 Agent 和默认私钥，不问密码）；主机自己没有私钥文件，要用私钥就建凭据。「测试连接」是真登录。连接方式：直接 / SSH 跳板（只走列出的链）/ HTTP 或 SOCKS5 代理，跳板和代理不叠加。
- **凭据**：侧栏第三个列表（密码 / 密钥 / SSH Agent），带用户名。密钥来源本机文件 / 粘贴 / 生成，私钥一律存成 `keys/` 下的文件（Windows 凭据管理器装不下 RSA 私钥）：粘贴和生成的直接写进去，选本机文件的复制一份进去，不出勾选框；原文件只读这一次、从不改动，用户改了原文件、连不上时自己重新选（用户定的）。同步只同步 `keys/` 里的私钥；升级前只记路径的旧凭据不动。删除在用的凭据要确认，那些主机改回自己登录。
- **串口主机**：默认 115200 8N1；连接是普通远程终端标签，Unix 上独占；没有 SFTP、跳板、转发，右侧栏只有命令片段，有「发送 Break」。
- **Telnet 主机**：协议自己写，不引第三方库。可选自动登录（手动或密码凭据）；连接方式同 SSH；服务器不回显时在本地编辑一行；不做 `telnet://` 外部连接；右侧栏只有命令片段。
- **端口转发**：侧栏第二个列表，每条规则经由一台保存的主机、自己建连接，和主机的标签无关。「创建」只保存不启动，可设随 ShellRS 自动开启；断线自动重连三次，不弹问答。
- **临时连接与外部连接**：临时连接的密码只在内存；外部连接只认命令行参数，不注册系统链接，打开时收起左侧栏。
- **外部 CLI**（给 AI Agent 用）：`hosts` / `credentials` 的增删改查，加 `exec`、`upload`、`download`、`sync`；对象叫 host；不做交互式 shell、不做 MCP。秘密只写不读。删除不确认、不关已打开的标签。`exec --terminal` 把命令打进已打开的终端（给只许登录一次的堡垒机），先发 ^C，不做忙碌检测（用户定的）。
- **本地终端**：Shell 设置是输入框加常用 shell 下拉；留空自动检测（Windows 优先 PowerShell 7）。
- **开始页**：没有标签时显示「最近连接」和「快速连接」。
- **右侧栏**：只跟着终端出现，工作区级一份；开没开、开哪个工具按主机记（存在主机表里，切标签也跟着变，issue #46），没开过的主机收起，本地终端共用一份（存设置里）；分屏时跟最近在前的终端，本地远程都算（用户定的）。本地终端旁只有「项目」；远程终端旁十一个工具：命令片段、历史命令、文件、tmux、Docker、Kubernetes、系统服务、进程管理、网络连接、系统监控、GPU/NPU；在「设置 › 终端 › 侧栏功能」里切换显示（GPU/NPU、Kubernetes 默认不显示），只在设置里改。按主机系统过滤工具。工具在终端自己的连接上执行命令；危险操作先确认，非 root 用 `sudo -n`；列表默认不自动刷新，定时读的只在显示时读。「文件」开在终端自己的连接上、随终端走；目录跟随靠 OSC 7，经用户同意才往服务器写配置。
- **项目**（本地终端）：右侧栏「项目」像主机一样保存本地终端，可以不分组；分组只有名字、不嵌套，和磁盘目录无关，分组和终端都能拖动排序，新建按钮在标题栏；删除终端连标签一起关，开着的才确认（用户试过「项目=目录+启动命令」和「目录列表」两版，都不要）。分组的「+」直接开一个终端、不填东西；关掉标签终端还在（变灰），点一下重开。每个终端自动记住 shell 最后所在的文件夹（Windows 读不到就回主目录），退出时开着的（顺序、在前的）随时写库，下次启动在原文件夹重开；「保留终端内容」（默认开）关标签和退出时记下最后 1000 行输出，重开时先显示。列表和标签上的名字跟 shell 和程序设的标题走（关标签、重启后记着最后一个），手动改过名的固定用那个名字（用户定的）。⌘T 的本地终端和分屏不恢复。
- **同步**（核心在 `crates/shellrs-sync`，方案见设计文档）：「设置 › 云同步」选一个文件夹（网盘同步的文件夹）、连接 WebDAV（坚果云、Nextcloud 等，地址是服务器上的文件夹）或 S3（AWS、R2、MinIO、OSS、COS 等兼容的，桶要用户先建好），没有同步就创建（同步密码至少 12 个字符）、已有就加入；第一次加入时的去重预览放下一轮（用户定的）。WebDAV 密码或 S3 的 Secret Key 只进钥匙串（`sync:login`），被拒时停下请用户重新登录，不弹框问。密码、口令和 `keys/` 里的私钥一律跟着同步，不设开关；对安全有要求的人不开同步就是（用户定的）。串口主机不同步，只留在本机（串口号是这台电脑的）。启动、窗口回到前台（每分钟最多一次）、改动后停 5 秒、「立即同步」、退出前（最多等 3 秒）各同步一次，不定时轮询；出错只写在设置页，要用户处理的（别处改了密码、被重置、被移出、一次删很多）才弹通知，自动同步等用户处理完。「更换同步密码」总是连数据密钥一起换，只有这一个入口（用户定的）。关闭同步时本机数据不动。
- **在线升级**：后台检查并下载，校验完才在标题栏亮按钮；对话框只有「查看更新内容」（打开官网更新日志）和「重启并安装」；不点也在退出时装好。检查只发 User-Agent。
- **撰写窗格**：中间区底部一个（Xshell 式），发给在前的终端（本地远程都算，同右侧栏），Enter 发送、Shift+Enter 换行；快捷键和终端右键菜单开关，开没开记在设置里。
- **命令广播**：标签图标前出现复选框，勾满两个自动排成分屏，撰写窗格改为发给全部（广播时它就是命令栏）；命令原样发出、不先清行；同步按键默认关。
- **Windows 便携版**：一个 zip 既是首次下载也是升级包；exe 旁有 `data` 才算便携版。密码仍进凭据管理器、和安装版共用（用户定的）。
- **软件渲染**：CPU 渲染时终端少画几帧、光标不闪、关动画。
- **匿名使用统计**（Aptabase）：默认开、可关；只发版本、系统、设置快照和功能次数，不发内容和任何标识。不用定时器（用户明确不要），只有发布构建发。
- **编辑器**：中间区 Dock 标签，不用系统程序；只认 UTF-8、上限 5 MB；远程文件原地改写，保存前核对是否被改过。
- **预览**：图片和 Markdown，快速查看式的大对话框，不是标签。
- **窗口**：默认记住大小和位置。
- **终端交互**：选中复制默认关；右键默认显示菜单。远程终端断开后按 Enter 在原标签重连，按 Space 关闭标签页。
- **标签连接状态**：标签图标彩色就是在线，连接中、断开时变灰（不用小圆点）。只在标签上变灰。
- **主机图标一律彩色**：没有品牌色的按地址从固定调色板取色，跨版本不变；绝不能是灰（灰留给离线）。
- **ZMODEM（rz / sz）**：用 `zmodem2`（钉死版本），自动识别、不设开关；进度写在终端里；可以拖文件到终端上传。不做 XMODEM / YMODEM。
- **终端通知**：只做程序请求的（OSC 9 / 777）和响铃，不做 OSC 133。
- **字号**：界面字号在「外观」里，默认 16、Windows 14（对齐系统 9pt 正文）；⌘= / ⌘- / ⌘0 在终端里调终端字号，在终端外调界面字号。
- **主题**：浅色、深色各选一套；「界面跟随主题」默认关，界面是中性灰、只有终端配色（用户试过，别改回去）。默认深色用炭灰，不用近黑。只有内置主题，只收宽松许可的配色。
- **键盘快捷键**：设置里单独一类，只收主要功能；冲突就拒绝。终端里只按 Ctrl 加字母的通用快捷键让给 shell；「显示或隐藏 ShellRS」是系统级快捷键，默认不设键。
- **关键字高亮**：设置里单独一类；规则是正则、颜色任意；总开关默认关，预置三条示例。

## 常用命令

```bash
cargo run                                   # 启动应用（发布版开着时只会激活它：先 ⌘Q 或设 SHELLRS_DATA_DIR）
SHELLRS_SIGN_IDENTITY="Apple Development: …" cargo run   # macOS：先用固定证书签名（scripts/macos-run.sh），钥匙串点过「始终允许」后重新编译不再问
cargo test                                  # 单元测试 + tests/ui/（无头窗口 UI 集成测试）
cargo test --lib                            # 只跑单元测试
cargo test --test ui new_host -- --nocapture   # 按名称片段跑单个 UI 测试
cargo test --test cli_bin                   # shellrs-cli 的端到端测试
cargo test -p shellrs-sync                  # 只跑同步核心（不编 GPUI，快）
cargo test -p shellrs-ssh                   # 只跑 SSH 核心（不编 GPUI）；SFTP 引擎的协议测试用本机的 sftp-server
cargo test -p shellrs-hosts                 # 只跑主机核心（数据库、HostData、同步一轮；不编 GPUI，快）
cargo test -p shellrs-mobile                # 手机端的 FFI 层（不带 -p 的 cargo 命令不含它，clippy 也要单独跑）
scripts/build-ios.sh; scripts/build-android.sh   # 手机端的库和 Swift / Kotlin 绑定，产物在 target/mobile/；之后 crates/shellrs-mobile/tests/{swift,kotlin}/run.sh 编译并跑一遍绑定
cargo test -p shellrs-sync webdav -- --include-ignored  # WebDAV 后端对本机 Apache（macOS 自带；Debian 装 apache2）；设 SHELLRS_TEST_WEBDAV_URL/_USER/_PASSWORD 改测别的服务器
cargo test -p shellrs-sync s3 -- --include-ignored      # S3 后端对 SHELLRS_TEST_S3_ENDPOINT/_ACCESS_KEY/_SECRET_KEY 指的服务（CI 起 versitygw）；不设就跳过
cargo test --lib terminal::run -- --include-ignored     # exec --terminal 对 Docker 里的真实 shell
cargo test --lib terminal::zmodem -- --include-ignored  # ZMODEM 对 Docker 里的 lrzsz
cargo clippy --all-targets -- -D warnings   # 必须无警告
cargo fmt --check
```

工具链由 `rust-toolchain.toml` 固定；`Cargo.lock` 里的 `gpui-pre*` 和 `cc` 不要随手 `cargo update`。本终端的 `screencapture` 被拦，视觉检查靠 UI 测试或让用户看窗口。

## 架构

Cargo workspace，`cargo` 命令默认覆盖前四个包：

- 根包是桌面应用 `shellrs`：单个二进制 crate，带 `src/lib.rs`，好让 `tests/ui/` 驱动生产环境的 `Workspace`。
- `crates/shellrs-sync` 是同步核心（MIT 许可，以后手机端也链接它）：不依赖 GPUI，也不依赖应用的类型；文件格式见它的 `FORMAT.md`。
- `crates/shellrs-ssh` 是 SSH 核心（MIT 许可，桌面和手机共用）：登录（`SshConnector`、known_hosts、认证、跳板、代理）、连接上的通道（pty + shell、exec、SFTP）、探测主机系统、SFTP 传输引擎。不依赖 GPUI 和应用的类型，不产出文案。
- `crates/shellrs-hosts` 是主机核心（MIT 许可，桌面和手机共用）：主机、分组、凭据、端口转发规则、命令片段、项目终端的模型，`HostDatabase`（结构与迁移）、`HostData`（内存为准、写穿 SQLite）、登录快照、`keys/` 里的私钥、秘密接口（`SecretStore`）和不碰钥匙串的实现；同步的导出和应用记录、一轮（`run_round`）、各任务的主体和建同步时的校验。不依赖 GPUI（模型的字符串用它单独的 `gpui-pre-shared-string`）、rust-i18n 和钥匙串，不产出文案。
- `crates/shellrs-mobile` 是手机端的接口（MIT 许可，UniFFI 生成 Swift / Kotlin 绑定）：包着 SSH 核心和主机核心（主机、分组、凭据、同步），不在 `default-members` 里，要 `-p shellrs-mobile`；用法、构建和线程规则见它的 `README.md`。

模块按能力划分：功能模块不依赖 `workspace`（窗口壳），也不碰彼此的内部。

- `app/`：全部用户动作（`actions.rs`）、快捷键、数据目录路径、图标、退出守卫。
- `host/`：重新导出主机核心，加上应用自己的：GPUI 外壳 `HostStore`（`Deref` 到 `HostData`）、标签和错误的文案（`describe.rs`）、标签的连接状态、主机树和对话框、导入（`import/`）、`ssh://` 链接、快速连接。
- `connection.rs`：测试连接（问答类型在核心，这里按原来的名字重新导出）。`ssh/`：重新导出核心，加上应用自己的：错误和问答的文案（`describe.rs`）、远程终端传输、测试连接的实现。`telnet/`、`serial/`：各自的终端传输。
- `terminal/`：本地 PTY、驱动 `alacritty_terminal` 的引擎、`TerminalView`、关键字高亮、目录报告、ZMODEM、`exec --terminal`。
- `sftp/`：重新导出核心的传输引擎（批次、并行、断点续传、重连），加上本机侧（目录、废纸篓）、引擎事件的文案和接到 SSH 的胶水。`explorer/`：SFTP 标签和右侧栏「文件」的界面、查找文件、预览。`editor/`：编辑器标签。
- `forward/`：端口转发的运行和界面。`credential/`：凭据界面。`project/`：「项目」的树和对话框（模型在主机核心，打开、保存和恢复终端在 `workspace/projects.rs`）。
- `monitor/`、`gpu/`、`netstat/`、`processes/`、`services/`、`docker/`、`kubernetes/`、`tmux/`、`history/`、`snippets/`：右侧栏工具，各自是「命令 + 纯解析模型 + 面板」。
- `secrets/`：系统钥匙串（`KeychainSecretStore`），接口和其余实现在主机核心。`settings/`：设置与设置页（「云同步」页在 `sync_page.rs`）。`cli/`：外部 CLI。`update/`：在线升级。`analytics/`：匿名统计。
- `sync/`：重新导出主机核心的同步，加上应用自己的：`SyncService`（什么时候同步，以及创建、加入、改密码、移除设备、关闭这些任务）、对话框和错误的文案（`describe.rs`）；一轮怎么走在核心的 `sync/round.rs`，导出、应用记录在核心的 `data/sync.rs`，处理器在 `workspace/sync.rs`。
- `workspace/`：窗口壳，持有 store、`DockArea` 和**全部动作处理器**，按领域拆文件。`shared/`：多个功能共用的展示片段。

## 关键约定与坑

### 状态与持久化

- **内存说了算，写穿。** `HostData` 改内存 → 写库；`HostStore` 调完再 `settle`：失败发 `PersistFailed`、有改动才 `notify`。写失败时内存照样改，绝不静默吞掉。设置同理写穿 `settings.json`。
- **秘密只进钥匙串**，有守卫测试；`SecretStore` 是阻塞的，只在后台调用；秘密不放进派生了 `Debug` 的结构。ShellRS 保存的私钥在 `keys/`；用户自己的私钥文件从不改动或删除。
- **登录快照在 UI 线程解析**：工作线程拿到的是查好凭据和跳板的 `HostLogin`，不读 store。
- **对外只给 `PublicId`**，不给内部 id。
- **改表**：升 `SCHEMA_VERSION`、加一步迁移和「旧库升级后与新库比对」的测试；表都是 `STRICT`；库只往前升，迁移前先备份。
- **发布后不能改的标识**：更新清单地址和平台 key、统计计数的键和安装类型、凭据的 `keychain_id`、同步密钥和同步位置登录密钥（WebDAV 密码、S3 Secret Key）的钥匙串账户 `sync:keyring`、`sync:login`、`Shortcut` 的 `id`、`ToolKind::key`、主题 key、`panel_name()`、目录跟随的标记文本；同步的一切（`crates/shellrs-sync/FORMAT.md`：远程目录和文件名、`SRSY` 信封、`vault.json` 字段、记录的 kind 和字段名）。
- **一个数据目录只跑一个 ShellRS**：启动时先连外部 CLI 的套接字，连得上就交给已在运行的那个。CLI 协议只加变体，不升版本。

### 多语言

- 文案在 `locales/<模块>.yml`，最外层写键的全名，下面 `zh-CN`、`en` 并排。代码用 `crate::i18n::t!` / `tn!`（带数量，英文分 `.one` / `.other`）。
- 键说用途，一句话一个键，不拼片段；插值写 `%{name}`。以 `%`、`{` 开头或含 `: ` 的值要加引号，否则那条文案会悄悄没了。
- 中文是源语言，改中文时英文一起改。实体里不存翻译好的字符串，render 时翻译，语言切换即时生效。
- 英文按 Design Guides：sentence case、单字符 `…`、按钮写结果，不写 Please / successfully。
- `src/i18n.rs` 的守卫测试检查两种语言齐全、键都被用到、`src/` 和 `crates/shellrs-ssh`、`crates/shellrs-hosts`、`crates/shellrs-mobile` 里测试以外没有中文字面量（语言名那种要留的加 `// i18n: keep`）。

用词表（新文案照此）：

| 中文 | English |
|---|---|
| 主机（台）/ 分组 / 地址 / 用户名 | host / group / address / username |
| 凭据 / 密码 / 密钥 / 私钥 / 口令 | credential / password / key / private key / passphrase |
| 认证：密码 / 使用凭据 / 无密码 | Password / Credential / No password |
| 连接方式：直接连接 / SSH 跳板 / 代理连接 | Direct / Jump hosts / Proxy |
| 临时连接… / 开始页「快速连接」 / 外部连接 / 最近连接 | Quick connect… / Find a host / external connection / Recent |
| 本地终端 / 远程终端 | local terminal / remote terminal |
| 连接 / 断开连接 / 重新连接 | Connect / Disconnect / Reconnect |
| 目录、文件夹 | folder |
| 本机 | this computer |
| 项目 / 新建本地终端 | Projects / New local terminal |
| 删除 | Delete（不用 Remove） |
| 正在读取… / 读取失败 | Loading… / Couldn’t load |

### GPUI / gpui-kit

- **一个命令一个处理器**：按钮、菜单、快捷键都派发 `app/actions.rs` 里的动作，由 `Workspace` 处理，不写临时闭包改状态（对话框内部自己的状态除外）。
- **焦点**：`dispatch_action` 只到焦点路径上的处理器，工作区里必须有东西持有焦点；不要聚焦工作区根节点；中间区面板在 `set_active(true)` 里收下焦点；对话框关掉后焦点为空，要经工作区的 focus handle 派发。焦点监听器里改状态要 `defer_in`。
- **后台线程不直接唤醒前台任务**，由 UI 定时轮询事件。
- **渲染回调不读取实体**（树行、表格单元），只传快照。窗口还没挂上 `Root` 时不能弹通知（用 `notify_once_open`）。
- **对话框**：都要 `.overlay_closable(false)`；表单错误用 `form_error_notification`。
- **右键菜单挂在容器上，不挂在行上**；滚动条挂在不滚动的外层上；`Select` 要放进有尺寸的容器；`uniform_list` 的行必须等高。
- **按上一帧的尺寸重排**：render 拿不到布局尺寸，用 `canvas` 在 prepaint 记下、变了再重画。
- **颜色**：用主题 token 和 rem，`px` 只用在 API 要 `Pixels` 的地方；要彩色图标就不能用 `Icon`（它是单色遮罩）。
- **快捷键改了要重建整个 keymap**（GPUI 删不了单条绑定），见 `app/shortcuts.rs`。
- **标签页全部可关**，关闭走 `DockArea::remove_panel`。左右侧栏各是一个面板，不用 `set_dock` 换；当前标签只经 `Workspace::set_active_tab` 改。

### SSH

- 所有登录都走核心的 `SshConnector`；known_hosts 写在数据目录，不碰 `~/.ssh/known_hosts`。没人能回答的问答（测试连接、exec、重连）当场拒绝，不能挂住。
- 算法按 OpenSSH 默认（另加了老堡垒机要的 ECDH nistp 和 ≥ 2048 位的 DH）。
- 只有网络类错误才自动重连。
- 外部连接只开一个通道：用户的堡垒机多开通道就断会话。
- 右侧栏工具的命令写成一行 `sh -c '…'`，要经得过 bash、zsh、fish、csh；各工具都有用本机 `sh` 真跑脚本的测试。

### SFTP

- 每个标签独立连接、独立传输引擎，批次排队。
- **只发 WinSCP 也发的请求**：用户的堡垒机收到 `fsetstat`、`fsync@openssh.com` 等会断会话，所以不用任何扩展；`bastion_server` 测试守着这条。
- 下载严格按偏移顺序写 `.filepart`（续传只信它的长度）。文件操作从不跟随符号链接。
- 选择归面板、不归表格；「名称」格才是项目，其余地方拖动是框选。
- 编辑器和预览先读后开，走 SFTP 标签自己的连接。

### 端口转发

- 规则在 store，运行状态在 `ForwardManager`；首次登录可以问，重连不问；发现断线靠处理器被丢弃。

### 外部 CLI、升级、发布、平台

- 外部 CLI 经正在运行的应用执行，命令本身不读钥匙串、不开数据库；改动在 UI 线程逐条做。Unix 套接字 0600 并核对 uid，Windows 用带 DACL 的命名管道。exec 和传输都不许挂住。
- `exec --terminal` 的细节在 `terminal/run.rs`；真实 shell 的测试在 Docker 里（CI：`shells.yml`）。
- **Windows** 只在 CI 上编译和测试：`windows.yml` 按路径触发，新加 `cfg(windows)` 代码的文件要加进那个列表。发布版是 GUI 子系统，命令行由 `shellrs-cli.exe` 承担。C 运行库静态链接（`.cargo/config.toml` 的 `+crt-static`）：动态链接要 `VCRUNTIME140.dll`，LTSC 版和没装 VC++ 运行库的系统启动不了；`scripts/windows-runtime.ps1` 在 CI 里查两个 exe 的导入表。
- 在线升级和统计在 UI 测试里都没有真实服务，测试从不联网；统计新加计数要同步两份手册「隐私」一节的清单。
- **发布走 tag**（需人工批准）。更新日志由改版本号的提交写，中英两份（`CHANGELOG.zh.md`、`CHANGELOG.md`）都要有，功能提交不改。
- macOS 上关窗口是隐藏，⌘Q 才退出。

### 同步核心（`crates/shellrs-sync`）

- 时间和随机数只经 `Env` 取（守卫测试查），一轮的结果只取决于参数；测试用 `FixedEnv`。
- 一轮分两步：`pull` 什么都不写；应用在一个事务里应用改动并保存它返回的状态，期间记录有变就丢掉结果重来；之后才 `push`。
- 应用交出的每条记录带 `edited_ms`：只在用户改（或替用户改，如删凭据后主机改回自己登录）时变；同步写入的记录保持原值，新建的为 0。
- 秘密（`secret.*` 记录：登录密码、代理密码、凭据的密码、口令、私钥）一律同步，没有开关；明文只在加密的设备文件里：`SyncState` 里只有用数据密钥加密的密文（`to_bytes` 里找不到明文，有测试查），存进数据库的上传摘要也不哈希秘密内容；交给应用的 `Secret` 的 `Debug` 不显示内容、用完清零。
- 应用这一轮读不出秘密（钥匙串被锁、没有 Secret Service）要把 `secrets_read` 设成 false，读不出的单条（私钥文件）和一轮带来、还没写进去的秘密列进 `unreadable`：它们原样保留，绝不当成删除，`pull` 再交一次。没写进去的秘密按同步键记在数据库的 `sync_pending`（只有键），和改动同一个事务写下，写成功才删。
- 同步的密钥（`Keyring::to_bytes`，存在钥匙串的 `sync:keyring`）只带最新 8 代数据密钥：Windows 凭据管理器一条最多 2560 字节，约 25 代；更旧的在文件或状态用到时，用包裹密钥从 `vault.json` 取回，不用密码。`vault.json` 保留全部代。
- 钥匙串只读变过的秘密：编辑时间（`secret_edits`）等于 `SyncState::edit_seen` 的不读，列进 `unchanged`，由状态里存的那份顶上；用户删除秘密也要记编辑时间，否则删除传不出去。不然每轮读一遍全部秘密，未签名的开发版或点了「允许」而非「始终允许」的用户每轮都被钥匙串问一遍。
- 格式（`FORMAT.md`）发布后不能改；`testdata/v1` 的金样文件以后每个版本都要能读。发布前不留兼容旧数据的代码：每个字段都写出、读时缺了就算坏记录，不补默认值。
- WebDAV 后端（`store/webdav.rs`）只用 PROPFIND、GET、PUT、DELETE、MKCOL。ETag 不论强弱都当标签（Apache 给刚写的文件头一秒弱 ETag，`If-Match` 永远不匹配弱的），条件写被拒先 PROPFIND 核对再算冲突；服务器不给 ETag 就用修改时间加大小拼，离服务器的 `Date` 太近就带疑点标记；列目录不给、读文件却给 ETag 的（nginx），读写也改用时间加大小，否则每轮都把别的设备的文件重下一遍。测试起的 Apache 要在自己的进程组里（`process_group(0)`），它退出时会给整个进程组发 TERM，把 cargo 和 shell 一起杀掉。
- S3 后端（`store/s3.rs`）自己签名（SigV4，`hmac` + `sha2`，不引 SDK），有对照 AWS 官方示例的测试；它和本地文件夹后端是仅有的两处可以读系统时间的地方（签名要用，守卫测试里列着）。条件写（`If-None-Match: *`、`If-Match`）被服务回「未实现」就改成先 HEAD 再写；时钟偏差过大被拒时按服务的 `Date` 校正重签一次。
- 这个 crate 是 MIT：只放作者自己写的代码，不从别人贡献的 GPL 代码里复制。

### SSH 核心与手机端（`crates/shellrs-ssh`、`crates/shellrs-mobile`）

- **核心不产出文案。** 失败是 `anyhow` 链里的类型化叶子错误，链要留着：是不是网络错误、缺不缺凭据都沿链判断（SSH、SFTP 各一份 `is_network_error`，规则不同）。桌面端在 `ssh/describe.rs`、`sftp/describe.rs` 沿链逐节翻译、用「: 」连起来；手机端由 FFI 层把链归成扁平的 `FailureLink`，交给 App 翻译。核心类型不加 `#[non_exhaustive]`，加了变体 `describe` 编译就报错；FFI 的 `failure.rs` 也要跟着加，否则落到 `Other`。
- **核心不依赖应用。** 秘密经 `SecretSource` 读（阻塞，放进 `spawn_blocking`）；agent、默认私钥目录、NAT64 是 connector 的配置（手机上没有 agent 和 `~/.ssh`，要开 NAT64）。核心的 Rust API 在 workspace 里随时可改。
- **稳定的只有 `shellrs-mobile` 的接口**：改名、删字段就得和 App 一起改。导出的方法不能叫 `close`（Kotlin 的 `AutoCloseable` 占了，生成的代码编不过），结束用 `disconnect()` / `end()`。生成的 Swift、Kotlin 要编得过、跑得通：`tests/swift/run.sh`、`tests/kotlin/run.sh`（CI：`mobile.yml`）。
- **FFI 层**：每个数据目录一个 `Core`，它持有多线程运行时；导出的方法里不直接用 tokio，一律 spawn 到 `Core` 的运行时、包一层 abort-on-drop（Kotlin 取消协程会中止后台任务；Swift 取消 `Task` 不会，UniFFI 还没传过去）。事件用拉（`next_event`）：终端泵一直把通道读空、放进合并缓冲区（russh 没有背压，读慢了会卡住整条连接），超出上限丢最旧的并报 `OutputDropped`；提问走无界队列。
- **手机端的主机和同步**：`HostData` 放在 `Mutex` 里（`store.rs` 的 `Shared`），每次写完照桌面的 `settle` 做：钥匙串任务按顺序放到阻塞线程池、事件进 `HostStore::next_event`；保存主机、凭据的方法等钥匙串写完才返回；`connect_host` 连上后照桌面探测主机系统（只许一个通道的堡垒机除外）。钥匙串是 App 实现的外部 trait `Keychain`（账户名同桌面，含 `sync:keyring`、`sync:login`），秘密只经 store 写，App 自己写进去的同步不知道。同步的位置存在数据目录的 `sync.json`，什么时候同步由 App 决定（规则写在 README）。Android 上 HTTPS 证书靠 JVM 校验：App 先调 `Tls.setUp(context)`（`src/android.rs`、`android/Tls.kt`），再加 `rustls-platform-verifier` 的 AAR。
- 测试用的 SSH 服务器在核心的 `testing` feature（`shellrs_ssh::testing`），只给别的 crate 当 dev-dependency。
- 续传记录（数据目录的 `upload-resume/`、`download-resume/`）的文件名有金样测试钉着，手机端和桌面用同一套。

### 主机核心（`crates/shellrs-hosts`）

- **核心不产出文案。** label 和错误都是类型（`ProtocolKind`、`HostDraftError`、`Failure`、`SecretError`、`JobError`、`LoginError` 等），桌面在 `host/describe.rs`、`sync/describe.rs` 用扩展 trait 翻译（`KindLabel::label()`、`ErrorMessage::message()`，调用处 `use … as _`）。要起名字的地方（复制主机的「副本」、新本地终端的默认名）由调用方把名字传进来。
- **`HostData` 不通知、不碰线程。** 写方法改内存、写库，失败、转发设置的变化和 `Changed` 放进 `take_events()`，钥匙串写入放进 `take_secret_jobs()`（`SecretJob` 跨线程跑，结果交回 `secret_done`）。外壳上同名带 `cx` 的写方法优先被找到，所以绕不过 `settle`；新加写方法要加进 `host/store.rs` 的 `settled!` 列表。经 `DerefMut` 只调读方法和 `*_unnotified`。
- **一轮同步经 `RoundHost`**：`run_round` 只经 `with_data`（短暂拿数据）和 `background`（阻塞的步骤）和外面打交道；桌面用 `WeakEntity<HostStore>` 加 `AsyncApp` 实现，每次用完数据都 `settle`，手机用 `Mutex<HostData>` 加 `spawn_blocking`。什么时候同步留在应用的 `SyncService`。`background` 收装箱的闭包（`Blocking<R>`）、打开的存储包成 `Remote`：不然 rustc 证不出这一轮的 future 是 `Send`，放不上多线程运行时。
- **核心不依赖应用。** `Host.sidebar_tool` 存 `ToolKind::key` 的字符串（不认识的原样留着）；品牌色和按地址取的颜色是 `u32`，应用用 `rgb()` 转；User-Agent 和设备信息由应用交给 `SyncServices::system`。
- 测试夹具：`HostDatabase::in_memory` 在 `testing` feature，应用只在 dev-dependency 里打开；`HostData::seed()` 留在普通构建里，因为 `tests/ui/` 链接的是正常编出来的库。

### 终端

- ZMODEM 先于 alacritty 拿到输出；传输期间键只认 Ctrl+C。
- 查找条是终端 `div` 的兄弟，不是子元素，否则输入会发给 shell。
- 程序的查询（光标位置、颜色等）在解析线程上按顺序回答，别交回 UI 线程。
- 同步更新（DEC 2026）超时要解析线程自己收尾，否则屏幕会冻住。
- 终端字体、颜色、高亮规则走全局量（终端模块不依赖 settings）。

## 测试约定

- **一个测试程序**：UI 测试都在 `tests/ui/`（`main.rs` 按功能分模块，共用的在 `support/`），不在 `tests/` 下另开文件（每个都要再链接一遍 GPUI）。
- **全部注入**：`Workspace::new_with_services` 注入假实现；测试不碰用户的服务器、钥匙串、设置文件、真实套接字和网络。`HostStore::seed()` 是夹具。
- 被查询的元素带稳定的 id，自定义 `div` 还要加 `.test_support()`。
- 不用右键菜单驱动测试，直接派发菜单会派发的动作；测试模块显式导入类型（`use gpui_kit::*` 会遮蔽 `#[test]`）。
- 动作和 `set_active` 是延迟执行的：先 `run_until_parked` 再 `render_frame`；工作线程的事件要推进时钟或 `cx.wait_for`。
- 看不见的换个法子查：密码查 `InMemorySecretStore`，按钮禁用看能否取得焦点。
- 单元测试起子进程一律经 `crate::testing`。
