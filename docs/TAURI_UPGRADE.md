# Tauri 升级检查清单

升级 Tauri 前逐项复核本清单。依赖固定在 2.12.5，`unstable` API 不承诺小版本兼容，不能只改版本号就发布。清单的判断依据见 [DESIGN.md](DESIGN.md) 的 §4 与 §7.7。

## 固定版本

| 依赖 | 版本 | 约束 |
|---|---|---|
| `tauri` | `=2.12.5` | 精确固定，启用 `unstable` feature |
| `tauri-build` | `=2.7.4` | 构建期生成 ACL，随 `tauri` 一起复核 |
| `tauri-plugin-single-instance` | `2.5.4` | 用范围版本，升级时核对与 `tauri` 的兼容范围 |

`=` 前缀加在 `tauri` 与 `tauri-build` 上。插件靠本清单在升级时人工核对，避免范围版本把整套依赖悄悄带走。

## unstable 的使用面

M0 只启用 feature，并冻结 `AdminHost` 接口。`launcher-core` 不依赖 Tauri，接口见 `core/src/admin_host.rs`。M4 才在桌面壳里写 `WebviewBuilder` 与 `add_child` 调用，独立窗口保留为运行时 fallback，现有入口是 `commands.rs::open_admin_window`。

警告：把 `unstable` API 调用写进 `launcher-core` 会让 core 依赖 Tauri，CI 里的无窗口单测会失效。

## capability 现状

`src-tauri/capabilities/default.json` 保留 `core:default`，另显式开放 `allow-show`、`allow-hide`、`allow-set-focus`。这是过渡态：`core:default` 含启动器用不到的 menu、tray 与 image 权限，M4 界面重做时替换为逐项清单。

M4 加入子 WebView 时按以下规则改：

1. 显式列出 `core:webview:allow-create-webview`、`allow-reparent`、`allow-set-webview-position`、`allow-set-webview-size` 与 `allow-webview-close`。
2. 不放开 `core:webview:*`。
3. 不开放文件系统、Shell 与任意网络权限。
4. 挂载请求的校验继续走 `launcher_core::admin_host::validate_mount_request`。URL 只允许 `http://127.0.0.1:<端口>/<路径>`。

## 升级步骤

1. 改 `src-tauri/Cargo.toml` 里 `tauri` 的版本号与 `unstable` feature。
2. 运行 `cargo test -p launcher-core`，确认无窗口层不受影响。
3. 运行 `cargo clippy --workspace --all-targets -- -D warnings` 与 `cargo fmt --all -- --check`。
4. 查 Tauri 发版说明，列出本次变动的 `unstable` API 名称。
5. 对照本清单复核 `default.json` 的每条权限标识符是否仍然有效。
6. 在真机跑 [ROADMAP.md](ROADMAP.md) M4 的七条 WebView 冒烟断言。
7. 更新本清单的版本表与变动记录。

注意：步骤 6 不能跳过。编译通过不等于子 WebView 能挂载，也不等于连续切换管理页不卡死主线程。

说明：本清单只覆盖 Tauri 层。NSIS 的 `installMode`、`webviewInstallMode` 与 `installerHooks` 属于 M5 的分发配置，见 [ROADMAP.md](ROADMAP.md)。
