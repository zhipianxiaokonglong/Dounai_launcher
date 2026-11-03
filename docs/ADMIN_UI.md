# manifest 的 `admin_ui`：管理界面声明

本文件面向**模块与插件开发者**，说明如何在 manifest 里声明自己的
配置界面，让启动器自动为你的模块渲染对应管理页。

## 为什么放在 manifest

启动器**不硬编码**"哪个模块对应哪个界面"——它读 manifest 里的 `admin_ui`
决定渲染什么。这样：

- 你新增一个模块，只要在自己的 manifest 里声明，启动器就认得；
- 启动器不需要为每个新模块改代码；
- 界面的能力边界（能做什么、不能做什么）由模块自己声明，权责清晰。

## 位置与签名

`admin_ui` 必须写在 **manifest 内部**，随 manifest 一起被签名覆盖。

**不允许**由启动器或第三方在外部追加——否则等于绕过签名校验机制。

## 字段定义

```json
"admin_ui": {
  "kind": "llm",
  "title": "模型厂商（LLM）",
  "capabilities": ["list", "create", "update", "delete", "toggle", "test"],
  "config_source": { "kind": "module_api", "path": ".env" },
  "endpoints": {
    "custom": "/v1/custom-providers",
    "builtin": "/v1/providers",
    "defaults": "/v1/provider-defaults"
  },
  "note": "给其他开发者看的说明，UI 不强制展示"
}
```

### `kind`（必填）

决定启动器用哪种界面模板渲染：

| 取值 | 含义 |
|---|---|
| `llm` | 模型厂商管理（支持增删改查、探测、测试） |
| `tts` | 语音厂商管理 |
| `input` | 输入层 / ASR / 多模态厂商管理 |
| `app` | 模块自身设置（没有厂商概念） |

**不声明 `admin_ui`** = 该模块没有管理界面，启动器不渲染。

### `title`（必填）

界面标题。缺省时启动器回退到 manifest 的 `name`。

### `capabilities`（建议填）

能力白名单。启动器**只暴露你在数组里列出的操作**，其余按钮不渲染。

可用取值：

```
list / create / update / delete / toggle / test / discover
set_default / export / import / write_env
```

这样声明是为了安全：你只读的模块不该出现"删除"按钮。

### `config_source`（必填）

密钥与配置存哪里，决定启动器用哪种方式读写：

| `kind` | 语义 |
|---|---|
| `env_file` | 启动器直接读写模块根目录的配置文件（默认 `.env`） |
| `module_api` | 启动器调你提供的 HTTP API；**密钥不由启动器持久化** |

`path` 可选，仅 `env_file` 时用，缺省 `.env`。

**重要**：如果你的模块自己管理密钥存储（例如加密落库），
必须声明 `module_api`，否则启动器会去改 `.env`，而你的模块并不读它。

### `endpoints`（`module_api` 时必填）

各操作对应的 HTTP 路径。**必须是以 `/` 开头的本机相对路径**，
启动器会拒绝绝对 URL（防止被当成任意出网地址）。

启动器只用它拼装请求，不缓存任何上游状态，每次操作都现取。

### `note`（可选）

给其他开发者的说明文字，启动器不一定展示。

## 启动器的渲染规则

1. **只渲染已安装的模块**——模块未安装时，对应管理界面完全不出现
   （不是灰色占位）；
2. 标题优先用 `admin_ui.title`；
3. `capabilities` 未列出的操作不渲染；
4. `endpoints` 只接受 `/` 开头的路径；
5. 结构不合法（如 `kind` 取值未知）时，**该模块的管理界面不渲染，
   但模块本身照常显示**——一个坏的管理声明不该让整个模块消失。

## 密钥归属（所有插件必须遵守）

本项目约定：**启动器与 Dounai 悬浮窗都不存储任何密钥副本**，
密钥由各模块自己保管。

因此在 `module_api` 模式下：

- 启动器只是**转发**你填的密钥，一次调用后即丢弃；
- 你的 API 应当自己加密存储（推荐 AES-GCM 之类）并只返回**掩码**；
- **不要**期望启动器帮你持久化密钥——它不会，也不该。

在 `env_file` 模式下：

- 启动器直接把值写进模块根目录的配置文件；
- 你的模块负责在启动时读取它；
- 启动器写完即不再持有该值。

## 完整示例

一个模型层插件的最小声明：

```json
{
  "module_id": "my-llm-plugin",
  "name": "我的模型插件",
  "version": "0.1.0",
  "category": "optional",
  "entry": { "type": "command", "command": "python -m my_llm", "working_dir": "" },
  "dependencies": [],
  "model_packs": [],
  "permissions": { "network": [], "filesystem": [], "devices": [] },
  "admin_ui": {
    "kind": "llm",
    "title": "我的模型厂商",
    "capabilities": ["list", "write_env"],
    "config_source": { "kind": "env_file", "path": ".env" }
  },
  "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
}
```

这个声明表示：启动器只需为它渲染一个"厂商列表 + 写 `.env`"的界面，
不需要增删改查与测试能力。

## 兼容性说明

- `admin_ui` 是**可选字段**。既有 manifest 不声明也能正常工作；
- 新增字段不破坏旧版启动器（旧版会忽略它）；
- 若你在发布版上使用，记得重新签名——
  因为 `admin_ui` 在签名覆盖范围内。
