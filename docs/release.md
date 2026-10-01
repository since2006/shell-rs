# 发布

[返回 README](../README.md)

在 `CHANGELOG.md` 里写好 `## [x.y.z]` 一节，把 `Cargo.toml` 的版本改成 `x.y.z`，提交后推送 tag `vx.y.z`。`.github/workflows/release.yml` 依次：

1. 核对 tag 与 `Cargo.toml` 一致，截出更新说明，确认代码里有发布公钥；
2. 构建并打包：macOS 合成 universal，签名、公证、staple，产出升级用的 `.app.zip` 和首次安装用的 `.dmg`；Windows 用 Inno Setup 打安装程序；Linux 打 AppImage；
3. 在 `release` 环境里人工批准后：给每个包签名，上传到 R2（`dl.shellrs.com/releases/<版本>/`），建 GitHub Release，**最后**才替换更新清单 `dl.shellrs.com/update/v1/<通道>.json` 并清 CDN 缓存。清单里每个包有两个下载地址：先 R2，下载失败时客户端接着试 GitHub Release（仓库是公开的，下载不用登录）。

版本号带 `-` 的（如 `0.3.0-beta.1`）只发到 beta 通道；正式版同时成为 beta 通道的最新版。在 Actions 里手动运行这个 workflow 只构建打包、不发布，用来演练。客户端只接受比自己新的版本，所以发错的版本撤不回来：把清单改回上一版能阻止更多人升级，修复要发新的补丁版。

一次性准备：

- **更新签名密钥**：`minisign -G -p shellrs-update.pub -s shellrs-update.key` 生成两对（一对日常用，一对离线保存备用），把两个公钥（`.pub` 文件的第二行）填进 `src/update/build_info.rs` 的 `TRUSTED_KEYS`。私钥和口令只放进 GitHub 的 `release` 环境，另外离线备份；丢了私钥，已发出去的 ShellRS 就再也收不到更新。换钥匙时先发一版带上新公钥，再换签名用的私钥。
- **Cloudflare**：shellrs.com 的 DNS 托管在 Cloudflare；建 R2 bucket，绑定自定义域 `dl.shellrs.com`，关闭 `r2.dev` 地址；Cache Rules 让 `/releases/*` 和 `/update/*` 按源站的 Cache-Control 缓存（默认不缓存 `.json`、`.AppImage`）；一个只能写这个 bucket 的 R2 API Token，一个只能清这个 zone 缓存的 API Token。
- **Apple**：Developer ID Application 证书（导出 .p12）和 notarytool 用的 App Store Connect API Key。`packaging/macos/Info.plist.in` 里的 bundle id `com.shellrs.ShellRS` 和签名的 Team ID 一经发布就不能再改：钥匙串按它们授权，改了之后每条保存的密码都要重新允许。
- **图标**：`assets/logo/` 里的 `shellrs.icns`（macOS）、`shellrs.png`（1024×1024，Linux，macOS 没有 icns 时也由它生成）和 `shellrs.ico`（Windows 安装程序）。
- **GitHub secrets**：仓库级的 `APPLE_CERTIFICATE_P12`（base64）、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`APPLE_TEAM_ID`、`APPLE_API_KEY_P8`（base64）、`APPLE_API_KEY_ID`、`APPLE_API_ISSUER`；`release` 环境（设为需要审批）里的 `MINISIGN_SECRET_KEY`、`MINISIGN_PASSWORD`、`R2_ACCOUNT_ID`、`R2_ACCESS_KEY_ID`、`R2_SECRET_ACCESS_KEY`、`R2_BUCKET`、`CF_ZONE_ID`、`CF_API_TOKEN`。

清单格式见 `packaging/manifest.example.json`，由 `packaging/make-manifest.sh` 生成：一个文件里是清单原文和它的 minisign 签名，签名的 trusted comment 写明通道和版本。格式只增加字段、不改已有的；已发出去的 ShellRS 一直按 `/update/v1/` 这个地址检查。
