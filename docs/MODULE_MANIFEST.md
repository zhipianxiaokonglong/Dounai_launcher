# 模块 manifest 格式规范

> 面向**模块与插件开发者**。
> 本文是公开规范：任何按本文格式编写的模块，启动器都能识别并管理。
> 但**能被识别 ≠ 被信任**——安装时仍需通过签名校验。

---

## 一、manifest 是什么

每个模块在**自己的根目录**下放一个 `manifest.json`（或 `dounai-manifest.json`），
声明：怎么启动、需要什么、有哪些权限、有没有管理界面。

启动器只执行 manifest 里声明的入口，**不接受前端传入任意命令**。

## 二、文件名与位置

> **重要：启动器仓库里的 `manifests/` 不是本规范的权威来源。**
>
> | 位置 | 作用 | 谁读 |
> |---|---|---|
> | **模块根 `manifest.json`** | **执行与展示的权威来源** | Rust `entry_spec` / `read_module_manifests` |
> | 启动器 `manifests/*.json` | 商店元数据 + 未安装项占位 | 前端（仅当模块未登记时） |
>
> 已登记模块的卡片信息**一律读模块根那份**，因此卡片显示的版本/命令/依赖数
> 与进程实际跑的完全一致。`manifests/` 里的条目对已装模块不起作用。


| 项 | 说明 |
|---|---|
| 文件名 | `manifest.json` 优先，`dounai-manifest.json` 作为备选 |
| 位置 | **模块根目录**下（与模块的可执行文件同层） |
| 编码 | UTF-8 |

模块根由启动器记录（安装/登记时确定）。**manifest 内不要写死绝对路径**——
见第五节「路径与占位符」。

## 三、最小可用示例

```json
{
  "module_id": "my-module",
  "name": "我的模块",
  "version": "0.1.0",
  "category": "optional",
  "entry": {
    "type": "command",
    "command": "{module_dir}/my-module.exe",
    "working_dir": "",
    "env": {},
    "health": { "url": "http://127.0.0.1:8123/healthz", "timeout_ms": 3000 }
  },
  "compat": { "app_version": ">=0.1.0" },
  "dependencies": [],
  "model_packs": [],
  "permissions": { "network": [], "filesystem": [], "devices": [] },
  "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
}
```

## 四、字段逐项说明

### 4.1 必填字段

| 字段 | 类型 | 说明 |
|---|---|---|
| `module_id` | string | 模块唯一标识。安装时会校验它与请求一致，防"张冠李戴" |
| `name` | string | 展示名，缺省回退到 `module_id` |
| `version` | string | 版本号，用于版本漂移比对 |
| `category` | string | `core` / `optional` / `reserved` 三者之一，其他值拒绝 |
| `entry` | object | 启动入口，见 4.2 |
| `dependencies` | array | 运行依赖声明，**必须存在**（可为空数组） |
| `model_packs` | array | 模型权重声明，**必须存在**（可为空数组） |
| `signature` | object | 签名段，见 4.6 |

**注意**：`dependencies` 与 `model_packs` 必须**分开声明**，缺一不可——
这是刻意的，用于区分"运行依赖"与"模型权重"两类资源。

### 4.2 `entry` 段

| 字段 | 类型 | 说明 |
|---|---|---|
| `type` | string | 目前只有 `command`；缺省即 `command` |
| `command` | string | 完整命令行（程序 + 参数，空格分隔） |
| `working_dir` | string | 相对模块根的路径；**空字符串表示模块根本身** |
| `env` | object | 额外环境变量，值支持占位符 |
| `health` | object | 健康检查，见下 |

`health`：

| 字段 | 说明 |
|---|---|
| `url` | 健康端点。`null` 表示该模块没有健康端点（如纯 UI 程序） |
| `timeout_ms` | 单次检查超时，缺省 3000 |

### 4.3 **`command` 的安全约束**（务必阅读）

启动器对 `command` 的第一个 token（程序部分）做如下校验：

| 情况 | 结果 |
|---|---|
| 裸命令名（如 `python`、`cmd`、`uv`，不含 `/`、`\`、`:`） | **放行**，交给系统 PATH 解析 |
| 带路径，且解析后**在模块根内、文件存在** | 放行 |
| 带路径，但**解析后在模块根外** | **拒绝**，报「entry.command 必须位于模块目录内」 |
| 带路径，但在模块根内却**文件不存在** | **拒绝**，报「指向的文件不存在」 |
| 空字符串 | **拒绝** |

设计意图：公开格式后，恶意或手误的 manifest 会变多。若不限制，
一个 manifest 就能让启动器执行 `C:\Windows\System32\cmd.exe` 之类的任意程序。

**建议**：把自己的可执行文件放进模块根，用 `{module_dir}/xxx.exe` 引用。

### 4.4 `working_dir` 约束

`working_dir` 解析后**必须仍在模块根内**，否则拒绝。
空字符串等于模块根。

### 4.5 `compat` 与 `dependencies`

```json
"compat": { "app_version": ">=0.1.0" },
"dependencies": [
  { "name": "Python", "version": ">=3.11", "type": "runtime" },
  { "name": "fastapi", "version": ">=0.115", "type": "library" }
]
```

`type` 取值：`runtime`（运行时）/ `library`（库）。

### 4.6 `signature` 段

```json
"signature": { "algo": "SHA-256 + Ed25519", "value": "<base64 签名>" }
```

- `algo` 目前唯一支持 `SHA-256 + Ed25519`，其他值拒绝
- `value` 是对**除 `signature` 字段之外的整个 manifest 规范化 JSON** 的 Ed25519 签名
  （按 key 升序重新序列化，保证书写顺序不影响结果），base64 编码
- 未签名（`value` 为空）时：**发布版启动器会拒绝安装**；开发版（debug）放行

### 4.7 `permissions` 段（⚠ 当前不强制）

```json
"permissions": {
  "network": ["api.example.com"],
  "filesystem": ["{data_dir}/mymod"],
  "devices": ["microphone"]
}
```

**必须明确说明：当前版本中该字段仅作声明，运行时不做任何强制。**

不要依赖它来限制模块行为。这一限制会在后续版本补齐；
届时此处会更新为"已强制"并写明具体语义。

### 4.8 `admin_ui` 段（可选）

声明模块的管理界面。详见 [`ADMIN_UI.md`](ADMIN_UI.md)。

最小示例：

```json
"admin_ui": {
  "kind": "llm",
  "title": "模型厂商管理",
  "capabilities": ["open_web_ui"],
  "web_ui": { "path": "/settings", "label": "打开设置", "requires_running": true }
}
```

不声明 `admin_ui` = 该模块没有管理界面，启动器不渲染入口。

### 4.9 `reserved` 段（内部使用）

供 `category: "reserved"` 的保留插件使用，普通模块无需声明。

---

## 五、路径与占位符

### 5.1 可用占位符

| 占位符 | 含义 | 规范名 |
|---|---|---|
| `{module_dir}` | **模块根目录** | ✅ 推荐使用 |
| `{repo}` | 同 `{module_dir}`（历史别名，保持兼容） | ⚠ 旧写法 |
| `{data_dir}` | 该模块的私有数据目录 | ✅ |
| `{app_local}` | 启动器数据根 | ✅ |

**`{module_dir}` 与 `{repo}` 完全等价**，都指向模块根。
新写 manifest 请用 `{module_dir}`——`{repo}` 这个名字容易误解成"源码仓库"。

### 5.2 为什么禁止绝对路径

绝对路径在开发机上能跑，在客户机上**必然失败**（路径不存在）。
因此：

```json
// ❌ 错误：写死开发机路径
"command": "D:/Program/OmniNexus/.venv/Scripts/python.exe -m omninexus"

// ✅ 正确：相对模块根
"command": "{module_dir}/.venv/Scripts/python.exe -m omninexus --port 8102"
```

### 5.3 分隔符

Windows 上启动器会把 `/` 归一化为 `\`（`CreateProcessW` 对混合分隔符解析不稳定）。
**manifest 里统一写 `/` 即可**，启动器会处理。

---

## 六、启动器如何执行（行为契约）

1. 读模块根下的 manifest，解析 `entry`
2. 替换占位符 → 校验 `command`（见 4.3）→ 校验 `working_dir`（见 4.4）
3. 拆分命令为「程序 + 参数」，**拒绝 shell 元字符**（不经过 shell 拼接）
4. 启动后按 `health.url` 轮询，直到通过或超时
5. **不记录子进程 stdout**（上游可能把请求体打进 stdout，落盘即泄露对话）

### 6.1 运行状态的判定

启动器判定模块"运行中"有两个来源：

- **进程表**：启动器自己拉起的进程
- **健康探测**：进程表里没有、但健康端点通的，标为 `external: true`

`external` 的模块**无法用启动器停止**（它不归启动器管），UI 会禁用「停止」并提示原因。

---

## 七、签名与信任（当前状态）

| 项 | 状态 |
|---|---|
| 签名工具 | 已就绪：`cargo run --bin sign_manifests -- keygen/sign/verify` |
| 生产密钥 | **尚未生成** |
| 发布版行为 | `strict(Vec::new())` = **拒绝一切安装**（fail-closed） |

**当前发布版装不了任何模块**，这是刻意的取舍：宁可装不上，也不装来路不明的模块。
配置可信公钥后即可正常安装。

---

## 八、常见错误

| 报错 | 原因 | 处理 |
|---|---|---|
| `模块 X 缺少 manifest.json，无法解析启动入口` | 模块根下没有 manifest | 补 manifest |
| `entry.command 必须位于模块目录内` | 命令指向模块根外的路径 | 改用 `{module_dir}/...` |
| `entry.command 指向的文件不存在` | 路径对但文件缺失 | 检查是否漏打包文件 |
| `不安全的工作目录` | `working_dir` 越出模块根 | 改为相对路径或留空 |
| `未知取值 X`（category） | 用了不支持的 category | 用 `core`/`optional`/`reserved` |
| `manifest 缺少 signature 段` | 发布版要求签名 | 用 signing 工具签名 |

---

## 九、版本与兼容

- 本规范对应启动器 `0.3.1`
- 新增字段属**向后兼容**：旧版启动器会忽略不认识的字段
- 但若你依赖新字段的行为，需在 `compat.app_version` 里声明最低版本

---

## 十、获取帮助

- 管理界面声明：[`ADMIN_UI.md`](ADMIN_UI.md)
- 签名与密钥交接：[`HANDOVER.md`](HANDOVER.md)
- 用户视角的配置说明：[`USER_GUIDE.md`](USER_GUIDE.md)
