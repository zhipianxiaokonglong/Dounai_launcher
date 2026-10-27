# Dounai 启动器

Dounai 的安装、环境与模块管理工具。负责「装、配、修、卸」，悬浮窗负责「跑」。

> 产品主体（悬浮窗与 UI 总装层）在 [Dounai](https://github.com/zhipianxiaokonglong/Dounai) 仓库；
> 本仓库只管环境与模块的生命周期。

---

## ⚠️ 当前状态：没有软件源（重要）

**本版本无法在线下载或安装模块。** 这是当前最大限制，请先读完再动手。

| 项 | 现状 |
|---|---|
| 模块下载源 | **未部署**（仓库里没有任何可访问的模块包地址） |
| 出网白名单 | **空集**，启动器不允许任何出网连接 |
| 在线安装 | ❌ 不可用 |
| 一键套装自动安装 | ❌ 不可用（依赖在线源） |
| 本地 zip 安装 | ✅ 可用 |
| 启动 / 停止 / 卸载已装模块 | ✅ 可用 |
| 环境体检、数据清除 | ✅ 可用 |

### 具体表现

点「开始安装」时不会假装在下载，而是逐条列出真实阻塞项：

```
暂时无法开始安装，待解决：
  · 未配置签名公钥：未签名模块会被拒绝安装
  · 未配置模块下载源：无法取到模块包
```

环境页的「刷新模块清单」也会明确返回 `unavailable` 并说明是白名单空集所致。

### 为什么是空集而不是"随便放开"

清单与模块包决定「装什么、启动什么命令」。在没有可信源的情况下开放联网，
等于给供应链投毒留门。所以宁可装不上，也不装来路不明的模块——
这与 `SECURITY.md` 里「发布版未配置公钥时 fail-closed」是同一个取舍。

详见 [出网白名单](docs/ALLOWLIST.md)。

### 现在能怎么用

1. **体检环境**：环境页可检测 Python / Node / uv / 显卡 / 磁盘，指出缺什么；
2. **用本地 zip 装模块**：套装页底部「从本地文件安装」，会先算哈希再装；
3. **启停已装模块**：卡片「启动 / 停止」，环境页「一键启动 / 全部停止」；
4. **清除数据**：环境页「维护」→「清除本地数据」。

> 依赖不会自动装（需要在线源）。体检报告会告诉你缺什么，可按报告自行安装。

> 想接入真实软件源：需要先部署模块目录服务与下载源，然后在
> `src-tauri/src/main.rs` 的 `Allowlist::new([...])`、`tauri.conf.json` 的
> CSP `connect-src`、以及 [出网白名单](docs/ALLOWLIST.md) 三处同步配置。

---

## 为什么单独做一个启动器

Dounai 由多个独立模块组成（输入接入、LLM 编排、语音输出、UI 总装），
每个模块都有自己的运行时依赖。手工装一遍要处理：

- Python / Node / uv 版本对不对
- 一堆库依赖装没装全
- 端口占没占、服务起没起
- 大模型权重下到哪、校验没校验
- 装坏了怎么退回去

启动器把这些收敛成一个界面。

---

## 核心原则：最小安装

**安装器只装"跑起来必需的东西"；大体积模型权重一律不预装、只推荐。**

| 类别 | 内容 | 行为 |
|---|---|---|
| 必装 | 启动器本体、核心模块、运行依赖 | 自动装，不可勾除 |
| 推荐不预装 | 本地 LLM、本地 TTS、离线语音包 | 只显示推荐角标，默认不装 |
| 可选自选 | BYOK 厂商配置、风格映射 | 用户决定 |

这不是提示语，而是**结构性保证**：manifest 里 `dependencies` 与 `model_packs`
是不同字段，加载器强制校验；套装里推荐项与安装项同时出现会直接报错。

自检方法：在套装页选「最小可用」，安装计划里「模型包」一定是 0 个。

---

## 功能

| 功能 | 说明 | 状态 |
|---|---|---|
| **启动/停止** | 拉起、停止单个模块；一键启动整套；健康检查；进程退出自动回落 | ✅ 可用 |
| 环境管理 | 检测 Python / Node / uv / 显卡 / 磁盘，给出体检结论 | ✅ 可用 |
| 模块管理 | 卡片式界面，查看依赖与权限，启用/停用/卸载 | ✅ 可用 |
| 本地安装 | 从本地 zip 安装，适配离线机器与自建包 | ✅ 可用 |
| 依赖安装 | 多源下载、失败重试、断点续传、哈希校验、失败回滚 | ⚠️ 引擎就绪，**无源可用** |
| 一键套装 | 预设组合，按依赖顺序展示安装计划 | ⚠️ 计划可算，**装不了** |
| 推荐下载 | 大体积资源只推荐、不预装 | ⚠️ 随在线源一并不可用 |
| 联网信息 | 受出网白名单约束；离线回退本地缓存 | ⚠️ 白名单空集，仅返回缓存 |
| 模块化 | 统一 manifest + Ed25519 签名校验 | ✅ 可用 |

> 「引擎就绪、无源可用」不是措辞技巧：下载、校验、解压、回滚、签名验证
> 都有测试覆盖并通过，缺的只是可访问的模块仓库地址。

---

## 快速开始

> **先看上面那段**：本版本没有软件源，**装不了在线模块**。
> 构建出来的程序可以体检环境、启停已装模块、用本地 zip 安装，
> 但「在线安装」与「一键套装」要等源部署后才能用。

### 环境要求

- Windows 10 及以上（当前主要验证平台）
- WebView2 Runtime
- 构建需要：Node 20+、Rust 1.80+、MSVC 2022

### 从源码构建

```powershell
npm install
npm run build          # 前端
npx tauri build        # 打包可执行程序
```

产物在 `src-tauri/target/release/dounai-launcher.exe`（约 8 MB）。

仅编译调试版、不打包：

```powershell
npx tauri build --no-bundle
```

> 注意：直接 `cargo build` 会得到空白窗口——前端产物需要先由 vite 构建到 `dist/`。

### 开发模式

```powershell
npm run dev            # 启动 vite 开发服务器（浏览器里预览界面）
npx tauri dev          # 启动桌面端（可用全部功能）
```

---

## 安全

启动器会执行本机命令、解压外部压缩包、按 manifest 决定启动什么，
因此安全要求高于普通模块。

| 威胁 | 措施 |
|---|---|
| 恶意压缩包写出目标目录 | 解压前逐条校验路径；`..`、绝对路径、UNC、Windows 保留设备名一律**整体拒绝**，不做部分解压 |
| 下载内容被篡改 | SHA-256 校验且**不可跳过**；不匹配即删除并换源 |
| 模块被植入后门 | manifest Ed25519 签名；篡改 `entry.command` 或 `permissions` 会验签失败 |
| 装到一半失败留半残环境 | 先写临时目录，成功才原子替换；覆盖旧版先备份，失败自动还原 |
| 出网绕白名单 | CSP + 域名白名单 + 逐源校验三层收口；拦截后**不发起请求** |

详见 [SECURITY.md](SECURITY.md) 与 [出网白名单](docs/ALLOWLIST.md)。

签名与验签：

```powershell
cd src-tauri
cargo run --bin sign_manifests -- keygen --out-private priv.b64 --out-public pub.b64
cargo run --bin sign_manifests -- sign   --key priv.b64 --manifest ../manifests/omninexus.json
cargo run --bin sign_manifests -- verify --pubkey pub.b64 --manifest ../manifests/omninexus.json
```

> 发布构建在未配置签名公钥时是 **fail-closed**：任何未签名模块都会被拒绝安装。
> 这是刻意取舍——宁可装不上，也不装来路不明的模块。

---

## 技术栈

| 项 | 选择 | 理由 |
|---|---|---|
| 外壳 | Tauri 2 | 与悬浮窗同一技术栈；体积小、内存低 |
| 核心逻辑 | Rust | 解压/校验/下载需要性能与内存安全 |
| 界面 | 原生 TypeScript + Vite | 无框架，减小体积 |

---

## 目录结构

```
src/                 前端逻辑
  manifest.ts        manifest 解析与校验
  bundle.ts          套装解析与安装计划
  bridge.ts          Tauri 命令桥接
  localinstall.ts    本地安装的纯逻辑（哈希/编码/请求构造）
  ui/main.ts         界面
src-tauri/src/
  security.rs        路径安全与哈希
  install.rs         安装事务与回滚
  download.rs        下载（续传/镜像/校验）
  signature.rs       manifest 验签
  trust.rs           信任策略（按构建类型）
  runtime.rs         模块启停：进程表、健康检查、退出监听
  state.rs           模块状态管理
  catalog.rs         联网清单与白名单
  env_probe.rs       环境探测
  commands.rs        Tauri 命令层
  bin/sign_manifests.rs  离线签名工具
manifests/           模块 manifest（参考实现）
bundles/             套装定义
reserved/            Reserved 插件桩（默认禁用）
docs/                用户文档、验收、交接、测试说明
```

---

## 测试

```powershell
cd src-tauri && cargo test      # Rust：177 项
npm test                        # 前端：57 项
cd reserved/memory && pytest    # Reserved 插件：24 项
```

覆盖范围包括路径穿越 fuzz、安装链路端到端、签名篡改拒绝、
安装卸载循环无残留、断点续传、恶意包整体拒绝等。

测试方法学说明见 [docs/TESTING.md](docs/TESTING.md)。

---

## 文档

| 文档 | 内容 |
|---|---|
| [用户文档](docs/USER_GUIDE.md) | 安装、套装、界面、常见问题 |
| [**模块运行（启动/停止）设计**](docs/RUNNING.md) | **v0.2.0 设计稿：启动/停止命令、健康检查、一键启动、验收标准** |
| [出网白名单](docs/ALLOWLIST.md) | 允许访问的域名与自查方法 |
| [安全策略](SECURITY.md) | 漏洞披露、SLA、已实现防线、已知限制 |
| [验收总表](docs/ACCEPTANCE.md) | 逐项结论与证据 |
| [权限交接清单](docs/HANDOVER.md) | 凭据与密钥交接项 |
| [测试说明](docs/TESTING.md) | 测试方法学与两个易踩的坑 |

---

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。
