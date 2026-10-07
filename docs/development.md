# 开发与测试

[返回 README](../README.zh-CN.md)

## 构建

装好 [rustup](https://rustup.rs) 即可：`rust-toolchain.toml` 固定了 Rust 1.98.1，第一次构建时 rustup 会自动装上它，不改动机器上默认的工具链。首次构建要编译整个 GPUI 栈，需要一段时间。

Linux 需要先装这些开发包（Debian / Ubuntu，与发布构建用的一致）：

```sh
sudo apt-get install -y cmake clang pkg-config libfontconfig-dev \
  libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libx11-xcb-dev libxcb1-dev libvulkan-dev libdbus-1-dev libssl-dev
```

Windows 只在 CI（`.github/workflows/windows.yml`）上编译和测试。

## 运行和验证

工具链固定 Rust 1.98.1，`russh-sftp` 固定 3.0.0；保持仓库锁定的 GPUI 与 russh 版本。

```sh
cargo run
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

数据目录默认是系统应用数据目录中的 `shellrs`，可通过 `SHELLRS_DATA_DIR` 覆盖。主机、端口转发规则和凭据（不含密码）保存在 `shellrs.db`，主机信任记录保存在该目录的 `known_hosts`，凭据里粘贴或生成的私钥保存在该目录的 `keys/` 下，密码和私钥口令使用系统钥匙串（服务名 `shellrs`；账户名是主机端点的 `password:<用户>@<地址>:<端口>`、私钥的 `passphrase:<路径>`、密码凭据的 `credential:<随机 ID>` 和代理的 `proxy:<用户>@<代理地址>:<端口>`）。

`src/sftp/tests.rs` 使用临时目录与故障注入覆盖上传和下载的文件内容、空目录、链接、覆盖、权限、取消、源文件变化、乱序与短读、续传及替换各阶段恢复，以及删除、重命名、新建和递归改权限不跟随链接。Unix 协议测试使用本机 OpenSSH `sftp-server`（macOS `/usr/libexec/sftp-server`，Linux `/usr/lib/openssh/sftp-server`）；工作线程测试仅监听 `127.0.0.1` 的随机端口，使用测试凭据。限制本地套接字的沙箱会跳过该部分，需要在允许回环套接字的环境运行。

`src/forward/protocol_tests.rs` 用进程内的 SSH 服务器和本机的回显服务测试真实的转发工作线程：三种转发的往返、目标不可达、服务器禁止转发、端口被占用、空闲断线后重连、重试耗尽和停止后的清理，同样只监听 `127.0.0.1` 的随机端口。

`tests/ui/` 里的 UI 测试驱动真实 `Workspace`，通过 `Workspace::new_with_services` 注入 SFTP、本地目录、终端和端口转发服务，不连接用户服务器或真实钥匙串；在线升级的测试给 `workspace.updater()` 注入假的更新服务器和安装器，不联网、不改任何安装。Finder 的系统原生拖放仍需在真实 macOS 窗口中补充人工验收；无头测试覆盖原生文件拖入事件和面板内部拖放。

开发构建（`cargo run`）不检查更新。要在本机走一遍更新流程，用 debug 构建加环境变量 `SHELLRS_UPDATE_CHANNEL=beta` 编译，运行时用 `SHELLRS_UPDATE_URL=http://127.0.0.1:8000/{channel}.json` 指向本地的静态服务器（`python3 -m http.server`），清单用 `packaging/make-manifest.sh` 和自己生成的测试密钥签名，测试公钥放在环境变量 `SHELLRS_UPDATE_PUBLIC_KEY` 里。release 构建不认这两个环境变量。

上传交互参考 [WinSCP 上传流程](https://winscp.net/eng/docs/task_upload)，协议适配参考 [russh-sftp 请求接口](https://docs.rs/russh-sftp/3.0.0/russh_sftp/client/struct.RawSftpSession.html) 与 [OpenSSH 扩展规范](https://raw.githubusercontent.com/openssh/openssh-portable/master/PROTOCOL)。

## README 截图

`docs/screenshots/` 里的截图来自真实窗口（macOS 的 `screencapture -o -l <窗口号>`，2 倍分辨率）。截图时不要用自己的数据：把数据目录复制一份，改掉分组、主机的名称和地址，用 `SHELLRS_DATA_DIR` 指向这份副本启动 ShellRS；钥匙串里的密码按「用户@地址:端口」保存，所以副本里没改地址的主机照样能真实连接。截好后检查状态栏、对话框和终端输出里有没有真实的 IP、主机名或业务信息，有就打码。
