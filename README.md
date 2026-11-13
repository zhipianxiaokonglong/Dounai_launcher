# Dounai 启动器

Dounai 启动器管理模块的安装、环境与进程生命周期。产品主体（悬浮窗与 UI 总装层）在 [Dounai](https://github.com/zhipianxiaokonglong/Dounai) 仓库。

## 当前状态

本仓库处于 v2 重构期。`refactor/launcher-v2` 分支已冻结 M0 契约。M1 到 M3 的代码已提交到该分支，含环境引擎、双源包信任与生命周期命令面。v1 的进程管理、状态库与命令面已删除，Tauri 命令面由 `src-tauri/src/commands_v2.rs` 提供。界面重做在 M4。

- 仅支持 Windows 10 与 Windows 11。
- 不读取 v1 包格式，不设兼容层。v1 包格式从未打包分发。
- `src-tauri/src/` 是 v2 命令面与进程托管，`src-tauri/core/` 是不依赖 Tauri 的独立 crate；`src/` 前端仍是 v1 界面，M4 重做。
- 本版本不能在线安装模块。K1 已生成并内置指纹，K5 与官方 catalog 尚未生成。
- 内置 K5 公钥仍是占位值，远程 catalog 验签按不可用处理，不降级放行。命令面已提供 `set_k5_pubkey`，界面入口在 M4。
- M3 不落盘进程日志。上游 stdout 与 stderr 继续丢弃，诊断结论只进内存快照；M4 加显式诊断开关后才写文件并轮转。

注意：安装程序启动时会索要管理员权限。安装模式定为 `both`，这一行为已接受并写进 M5 验收项。`both` 只是安装器的运行时选项；落盘到 `state/install.json` 时写成 `current_user` 或 `per_machine`，便携 exe 写 `portable`，`install-v1` 契约不收 `both`。

## 契约

`contracts/` 是角色、包格式与本地状态的单一真源。Rust 类型定义、跨仓一致性 CI 与发布工具都以这里的文件为准。入口见 [contracts/README.md](contracts/README.md)。

关键口径：

- `role` 是五值封闭枚举：`input`、`llm`、`output`、`shell`、`auxiliary`。未知值硬拒，不降级成 `auxiliary`。
- zip 包内的 manifest 自带 32 字节 Ed25519 公钥。签名覆盖 `files` 清单，信任单位是公钥，不是下载源。
- 同一 `module_id` 可并存多个 `instance_id`。每个槽只有一个 active 实例占规范端口，其余实例使用 `18101` 起的备用端口。
- `shell` 允许第三方模块填槽，但不能自动成为 active。
- 目录与 Git 仓库可导入为源码工作区，固定按未验证处理。

修改任一契约文件后，先运行校验，再同步依赖该文件的代码与样例。

## 校验

```bash
node contracts/validate.mjs
npm install --no-save --no-package-lock --prefix .ci ajv-cli@5 ajv-formats@3
.ci/node_modules/.bin/ajv validate --spec=draft2020 --strict=false \
  -c .ci/node_modules/ajv-formats \
  -s contracts/module-v2.schema.json -d "contracts/examples/*.v2.json"
.ci/node_modules/.bin/ajv validate --spec=draft2020 --strict=false \
  -c .ci/node_modules/ajv-formats \
  -s contracts/install-v1.schema.json -d contracts/examples/state/install.json
.ci/node_modules/.bin/ajv validate --spec=draft2020 --strict=false \
  -c .ci/node_modules/ajv-formats \
  -s contracts/deps-v1.schema.json -d contracts/examples/state/deps.json
```

命令与 [contracts.yml](.github/workflows/contracts.yml) 一致：ajv 编译六份状态 schema，并对 manifest 与状态样例做数据级校验；`-c ajv-formats` 让 `date-time` 生效。字段与时间格式断言写在 [contracts/README.md](contracts/README.md)。

## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 设计决策、架构与子系统行为约束 |
| [contracts/README.md](contracts/README.md) | 契约入口，含角色、包格式与 M0、M1 的状态 schema |
| [docs/ROADMAP.md](docs/ROADMAP.md) | M1 到 M5 的里程碑与实测验收 |
| [SECURITY.md](SECURITY.md) | 安全策略、信任模型与已知限制 |
| [docs/SIGNING.md](docs/SIGNING.md) | 模块签名方法、密钥生成保管与轮换规程 |
| [docs/ADMIN_UI.md](docs/ADMIN_UI.md) | 模块如何声明自己的配置界面 |
| [docs/TESTING.md](docs/TESTING.md) | 测试方法学、M1 到 M3 的验收记录与已知限制 |
| [docs/TAURI_UPGRADE.md](docs/TAURI_UPGRADE.md) | Tauri 固定版本、unstable 使用面与升级复核步骤 |

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。
