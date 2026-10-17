# 局部重绘与终端增量渲染

终端缓存与刷新调度适用于所有平台；窗口级局部重绘适用于 Windows 的
所有应用窗口，包括主机列表、侧栏、设置、菜单和对话框，不限于终端区域。

## Windows 窗口级局部重绘

- 保留完整的离屏 BGRA 画面，按空间区域比较有序绘制图元，只清除、重绘
  和提交变化的矩形。变化区域内的背景、文字、阴影和浮层按原顺序重画。
- 最多提交 16 个矩形，避免相距很远的变化扩大重绘面积。区域过多时合并，
  覆盖约 70% 画面时完整重绘；没有变化时跳过提交。
- 窗口缩放、设备恢复、清屏颜色变化和未知表面完整重绘。缺少 D3D11.1
  矩形清除支持时走完整绘制；局部绘制失败后停用优化并重试完整帧。
- 每个窗口额外保留宽 × 高 × 4 字节的图像；4K 窗口约占 32 MiB。

补丁固定基于 `gpui-pre-windows 0.3.2`，不升级其他 GPUI 依赖。来源、
修改范围和测试边界见
[`README.shellrs.md`](../vendor/gpui-pre-windows/README.shellrs.md)。

## 终端增量渲染与调度

- 消费 Alacritty 的变化行，复用未变化行的字符数据和排版结果。光标闪烁、
  移动不再重新构造整屏文字；字体、缩放、颜色和尺寸变化使对应缓存失效。
- URL 与关键字高亮只重查变化的完整逻辑行；换行边界变化时同时刷新旧、新
  逻辑行。选区、搜索、跨行链接及宽字符选择保持原有语义。
- 普通输出、选区和光标刷新留在终端视图；连接状态、标题、尺寸、延迟和
  重连代次变化仍通知面板及工作区。
- 有输出的可见终端每 16ms 检查，空闲后退至 32/64ms，隐藏标签为 100ms。
  输入、获得焦点和重新显示会唤醒检查；所有可见分屏都按可见终端处理。
  后台连接、传输与终端解析继续运行。
- 增量绘制取代软件渲染下固定的 50ms 限帧及强制停用动画、光标闪烁。
  软件渲染识别仍保留，供「关于」页面展示。

## 对比与验证

设置下面任意一个环境变量后重新启动应用，可分别关闭终端缓存或 Windows
局部绘制；取消设置后恢复默认。关闭局部绘制仍使用保留画面路径，不等于
未打补丁的后端；完整对比应使用同一上游基线的原版与优化版。

```powershell
$env:SHELLRS_DISABLE_TERMINAL_CACHE = '1'
$env:SHELLRS_DISABLE_PARTIAL_RENDER = '1'
cargo run --locked
```

在相同 RDP 分辨率、窗口大小和分屏数量下，对比空闲、输入、持续输出、
选区、搜索、菜单、对话框、切换标签和缩放。持续滚动或全屏变化会降低收益。
像素回读测试验证绘制结果，不测量 RDP 编码、网络或客户端解码延迟。

```powershell
cargo test --locked --lib terminal::
cargo test --locked --test ui terminal::
cargo test --locked --test ui connection::
cargo test --locked --test ui terminal_groups::
cargo test --locked --test ui zmodem::
cargo test --locked -p gpui-pre-windows --lib
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```
