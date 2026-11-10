# Dounai 启动器

Dounai 启动器管理模块的安装、环境与进程生命周期。产品主体（悬浮窗与 UI 总装层）在 [Dounai](https://github.com/zhipianxiaokonglong/Dounai) 仓库。

## 当前状态

本仓库处于 v2 重构期。`refactor/launcher-v2` 分支已冻结 M0 契约，代码尚未按新契约改写。

- 仅支持 Windows 10 与 Windows 11。
- 不读取 v1 包格式，不设兼容层。v1 包格式从未打包分发。
- `src/` 与 `src-tauri/` 仍是 v1 代码，M1 起按 `contracts/` 重写。
- 本版本不能在线安装模块。官方 catalog 与 K1、K5 两把密钥尚未生成。

注意：安装程序启动时会索要管理员权限。安装模式定为 `both`，这一行为已接受并写进 M5 验收项。

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
npx --yes ajv-cli@5 validate --spec=draft2020 --strict=false \
  -s contracts/module-v2.schema.json -d "contracts/examples/*.v2.json"
```

## 文档

| 文档 | 内容 |
|---|---|
| [docs/DESIGN.md](docs/DESIGN.md) | 设计决策、架构与子系统行为约束 |
| [contracts/README.md](contracts/README.md) | M0 契约入口，含角色、包格式与状态 schema |
| [docs/ROADMAP.md](docs/ROADMAP.md) | M1 到 M5 的里程碑与实测验收 |
| [SECURITY.md](SECURITY.md) | 安全策略、信任模型与已知限制 |
| [docs/ADMIN_UI.md](docs/ADMIN_UI.md) | 模块如何声明自己的配置界面 |
| [docs/TESTING.md](docs/TESTING.md) | 测试方法学与三个易踩的坑 |

## 许可证

GPL-3.0-or-later。见 [LICENSE](LICENSE)。
