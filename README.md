# Dounai 启动器

在 Windows 上管理 Dounai 模块的安装、环境与进程生命周期的桌面应用。产品主体（悬浮窗与界面总装层）在 [Dounai 仓库](https://github.com/zhipianxiaokonglong/Dounai)。

[![CI](https://github.com/zhipianxiaokonglong/Dounai_launcher/actions/workflows/ci.yml/badge.svg)](https://github.com/zhipianxiaokonglong/Dounai_launcher/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/zhipianxiaokonglong/Dounai_launcher)](https://github.com/zhipianxiaokonglong/Dounai_launcher/releases/latest)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
![Windows](https://img.shields.io/badge/platform-Windows%2010%2F11-blue.svg)

## 快速开始

### 下载与安装

打开[最新版发布页](https://github.com/zhipianxiaokonglong/Dounai_launcher/releases/latest)，按用途下载一个文件。

| 文件 | 用途 |
|---|---|
| NSIS 安装包 | 装到 `%LOCALAPPDATA%\Dounai Launcher`，带开始菜单入口 |
| 便携 exe | 单文件运行，数据根落在程序目录旁 |
| `SHA256SUMS.txt` | 两个 exe 的 SHA-256 |

安装步骤：

1. 下载 NSIS 安装包。
2. 双击安装包。
3. 按提示完成安装。
4. 从开始菜单启动 Dounai 启动器。

注意：安装器会索要管理员权限。实际安装模式写在 `state/install.json`，取值为 `current_user` 或 `per_machine`。便携 exe 写 `portable`。

### 首次使用

1. 打开左侧“下载”页。
2. 拉取目录。
3. 安装需要的模块。
4. 打开“模块”页。
5. 点击“一键启动”。

说明：官方模块包与目录托管在 [SourceForge 的 dounai-releases 项目](https://sourceforge.net/projects/dounai-releases/)。代理可在“设置”页配置，支持关闭、使用系统代理和手动配置。

## 功能

- **多源下载**：官方源与社区源并存。每个源绑定自己的目录签名公钥，添加时必须确认完整指纹。跨源同名条目并列展示，安装目标由选中的条目决定。
- **内置 Nightly 源**：`Tonyjh07/Dounai_nightly` 每天构建四个模块，按日期版本号发布。目标机器不用连接 GitHub API 也能拉取。
- **当前实例**：同一角色只有一个当前实例占用规范端口。每个实例行都有“当前实例 / 设为当前”控件。切换前需确认，失败时自动回滚。
<!-- stc-disable-next-line: 下一行含 M6 产品术语，STC G6 判为套话属误报 -->
- **启动编排**：默认链路按角色拓扑从当前实例现算。启动步骤之间不等待健康结论。单步失败不中止其余步骤。也支持保存自定义链路。
<!-- stc-disable-next-line: 下一行含 M6 产品术语，STC G6 判为套话属误报 -->
- **链路管理**：自定义链路保存在 `state/chains.json`。删除实例后，链路保留原值并标出缺失步骤。
- **资源采样**：概览页按实例显示 CPU 与内存。外部启动的实例没有进程句柄，恒显示 `—`。
- **日志**：模块进程与装依赖细节写入 `instances/<key>/logs/`。启动器操作摘要写入 `logs/`，单文件超过 2 MiB 轮转，保留 3 份。
- **签名验证**：K1 公钥验模块包，K5 公钥验目录。目录签名公钥与包信任表物理隔离，加源流程不会写包信任表。

## 已知限制

- 仅支持 Windows 10 与 Windows 11。
- 不读取 v1 包格式。v1 包格式从未打包分发。
- 官方目录有效期上限 7 天。过期后官方源报“已过期”，不降级放行。
- 目录实例（本地源码）在源码变更后需要手动确认再启动。
- 资源采样首轮没有 CPU 基准，启动后 1～2 秒内显示 `—`。

## 契约

[contracts/](contracts/README.md) 是角色、包格式与本地状态的单一真源。Rust 类型、跨仓一致性检查与发布工具都以这里的文件为准。

关键口径：

- `role` 是五值封闭枚举：`input`、`llm`、`output`、`shell`、`auxiliary`。未知值硬拒，不降级成 `auxiliary`。
- zip 包内的 manifest 自带 32 字节 Ed25519 公钥。签名覆盖 `files` 清单。信任单位是公钥，不是下载源。
- 同一 `module_id` 可并存多个 `instance_id`。当前实例占规范端口，其余实例从 `18101` 起使用备用端口。
- `shell` 允许第三方模块填槽，不能自动成为当前实例。
- 目录与 Git 仓库可导入为源码工作区，固定按未验证处理。

## 项目结构

- `src-tauri/src/`：命令面与进程托管。
- `src-tauri/core/`：不依赖 Tauri 的独立 crate。
- `src/`：界面。视图与共享层在 `src/ui/`。
- `contracts/`：契约与状态 schema。

## 开发

### 构建前置

仓库不存随包的 `uv.exe`（31.8 MB）。首次构建或重新克隆后运行：

```bash
python scripts/fetch-uv.py
```

说明：`cargo tauri build` 需要 `uv.exe` 作为 bundle 资源。缺这个文件时发布构建会失败。哈希与来源见 [src-tauri/bin/README.md](src-tauri/bin/README.md)。

### 校验

```bash
npm run typecheck
npm test
node contracts/validate.mjs
```

契约 schema 与样例的完整校验命令见 [contracts.yml](.github/workflows/contracts.yml) 与 [contracts/README.md](contracts/README.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 设计决策、架构与子系统行为约束 |
| [contracts/README.md](contracts/README.md) | 契约入口，含角色、包格式与状态 schema |
| [docs/ROADMAP.md](docs/ROADMAP.md) | 里程碑与实测验收 |
| [docs/SECURITY.md](docs/SECURITY.md) | 安全策略、信任模型与已知限制 |
| [docs/SIGNING.md](docs/SIGNING.md) | 模块签名方法、密钥生成保管与轮换规程 |
| [docs/ADMIN_UI.md](docs/ADMIN_UI.md) | 模块声明配置界面的方法 |
| [docs/TESTING.md](docs/TESTING.md) | 测试方法与验收记录 |
| [docs/TAURI_UPGRADE.md](docs/TAURI_UPGRADE.md) | Tauri 固定版本、unstable 使用面与升级复核步骤 |

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。
