# 模块运行（启动/停止）设计与验收标准

> 本文定义启动器「**跑**」的能力：把已安装模块按 manifest 声明的入口拉起进程、
> 健康检查、停止回收、一键启动整套 Dounai。
>
> **状态：设计稿（v0.2.0 范围）** —— 当前 v0.1.0 未实现本功能。
> `RunState` 枚举与 `set_run` 已存在但无调用方，UI 不渲染运行状态，
> `entry.command` 定义了入口却没有任何代码去执行它。本文补齐这一缺口，
> 并明确验收标准，避免重蹈「文档承诺 > 实现」的覆辙。

---

## 1. 背景与目标

### 1.1 为什么需要

`USER_GUIDE.md` §1 的承诺：「启动器装好环境、修好配置，**然后启动悬浮窗**」。
但 v0.1.0 只实现了「装、配、修、卸」的状态管理，**「跑」完全没有**：

| 现状 | 说明 |
|---|---|
| `entry.command` 定义了命令 | `dounai-input serve` / `python -m omninexus` / `dounai.exe` … |
| `RunState` 定义了运行状态 | `stopped / starting / running / failed` |
| `set_run()` 存在 | **全仓无人调用**；状态文件加载时一律置 `Stopped` |
| UI 模块卡片 | 只有启用/停用、卸载，**没有启动/停止按钮** |

### 1.2 目标

1. 用户能在启动器里**拉起/停止**单个已安装模块，并看到真实运行状态；
2. 一键启动整套 Dounai（三个上游 + 悬浮窗），兑现「启动悬浮窗」承诺；
3. 进程崩溃时状态回落，不出现「显示在跑、实际已死」的假象；
4. 保持既有安全立场：**只执行已验签 manifest 声明的入口**，不引入任意命令执行。

---

## 2. 语义澄清：启用 vs 启动

这两个概念在 v0.1.0 里已经出现但容易混淆，先明确边界：

| 概念 | 字段/状态 | 含义 | 谁决定 |
|---|---|---|---|
| **启用** | `enabled`（持久化） | 模块是否**允许**被拉起 | `set_module_enabled`（v0.1.0 已有） |
| **启动** | `run`（内存态） | 模块进程**当前是否在跑** | 本设计新增的 start/stop |

- 「启用/停用」决定**能不能**跑；「启动/停止」决定**在不在**跑。
- `reserved` 类：默认禁用且不可启用 → 永不参与启动；
- `core` 类：不可停用 → 始终允许启动；
- `optional` 类：停用后不可启动（`enabled=false` 时拒绝 start）。

> 设计意图：把「是否允许」和「是否在跑」分开，避免用一个布尔值承担两种语义
> （v0.1.0 的 `enabled` 曾经想暗示运行状态，导致 UI 显示「已停用」但进程可能还活着）。

---

## 3. 命令契约

### 3.1 Rust 侧（`src-tauri/src/runtime.rs` + `commands.rs`）

| 命令 | 参数 | 返回 | 说明 |
|---|---|---|---|
| `start_module` | `module_id` | `RunView` | 启动单个模块，等待健康检查收敛到 `running/failed` |
| `stop_module` | `module_id` | `RunView` | 停止单个模块，等待进程退出 |
| `start_all` | — | `Vec<RunView>` | 按依赖顺序一键启动所有 `enabled` 的模块 |
| `stop_all` | — | `Vec<RunView>` | 停止所有正在运行的模块 |

`RunView` 结构（前端可直接渲染）：

```json
{
  "module_id": "omninexus",
  "run": "running",            // stopped | starting | running | failed
  "pid": 12345,                // 未运行时为 null
  "last_error": null,          // 启动/运行失败时的原因
  "healthy": true              // 最近一次健康检查是否通过
}
```

前置校验（`start_module` 拒绝条件，逐条返回可读错误）：

1. 模块不存在或未安装 → `NotInstalled`；
2. `enabled == false` → `Disabled`（optional 停用后不允许启动）；
3. `entry.type` 不是 `command`（如 reserved 的 `plugin` 桩）→ `NotExecutable`；
4. 模块已 `starting/running` → 幂等：直接返回当前状态，不重复拉起。

### 3.2 前端（`src/bridge.ts` + `src/ui/main.ts`）

| 函数 | 对应命令 | 用途 |
|---|---|---|
| `startModule(id)` / `stopModule(id)` | `start_module` / `stop_module` | 卡片按钮 |
| `startAll()` / `stopAll()` | `start_all` / `stop_all` | 环境页「一键启动/全部停止」 |

浏览器模式沿用 `BridgeUnavailableError` 提示「需桌面端」，与现有行为一致。

---

## 4. 启动流程

### 4.1 入口解析

从该模块的已安装 manifest 读取 `entry`，做**占位符替换**：

| 占位符 | 解析为 | 示例（omninexus） |
|---|---|---|
| `{repo}` | 模块安装根 `{data_root}/modules/{module_id}` | `…\modules\omninexus` |
| `{data_dir}` | 模块数据目录 `{data_root}/data/{module_id}` | `…\data\omninexus` |
| `{app_local}` | `app_local_data_dir` | `…\com.dounai.launcher` |

- `command` 按平台默认解析：Windows 下交给 `cmd /C` 的语义拆分执行
  （`dounai-input serve` → 程序 `dounai-input`，参数 `serve`），**不允许拼接 shell 管道/重定向**；
- `working_dir` 经 `safe_join` 语义校验后，必须落在 `{repo}` 内（复用 `security.rs` 的路径防线）；
- `env` 替换占位符后注入子进程，作为 `PATH` 之外的补充环境变量。

### 4.2 拉起与健康检查

```
start_module(id)
   │  1. 校验（§3.1）
   │  2. spawn（stdout/stderr 重定向到日志，见 §6）
   │  3. run = Starting（内存态）
   ├─ entry.health.url 存在
   │      4. 每 timeout_ms 间隔 GET health.url（默认 3000ms）
   │      5. HTTP 2xx 且响应体非空 → run = Running
   │      6. 超过 max_probe（默认 5 次）仍未通过 → run = Failed + last_error
   └─ entry.health.url 为 null（如 dounai-ui）
          4'. 进程仍存活 → run = Running
          5'. 进程退出 → run = Failed + 退出码
```

- **进程退出监听**：spawn 后立即接管退出通知。进程提前退出时，无论当前
  是 `Starting` 还是 `Running`，一律回落到 `Failed`（带退出码），
  并在 UI 上一秒清除「正在运行」。
- **健康检查在后台线程做**，不阻塞命令返回；命令返回时给出**当前快照**，
  前端可轮询 `list_module_states`（复用现有命令，`RunView` 并入其中）。

### 4.3 启动顺序（start_all）

按「依赖先于被依赖」的固定清单，逐个启动并**等待健康通过后再启动下一个**；
任一模块失败**不阻塞其余模块**（与降级哲学一致）：

```
dounai-input(:8101) → omninexus(:8102) → moodvoice(:8103) → dounai-ui(悬浮窗)
```

> 说明：顺序来自实际端口/启动约束（上游先起，悬浮窗最后）。
> `start_all` 跳过 `enabled=false` 与 `reserved` 模块；只启动 `installed` 的模块。

---

## 5. 停止流程

```
stop_module(id)
   │  1. 查进程表，无则幂等返回 Stopped
   │  2. 优雅停止：向进程组发终止信号（Windows: taskkill 无 /F）
   │     └─ 超时未退出（默认 5s）→ 强制终止（taskkill /T /F 终止进程树）
   │  3. 等待退出码回收子进程句柄（防僵尸）
   │  4. 从进程表移除，run = Stopped
```

- `stop_all` 按进程表逆序逐个停止（先悬浮窗，再上游）；
- 卸载/清除数据前必须保证模块已停止（`uninstall_module` 增加前置检查）。

---

## 6. 安全与隐私约束

| 约束 | 实现 |
|---|---|
| **只执行 manifest 声明的命令** | 启动参数全部来自已安装 manifest（安装时已验签+哈希），不接收前端传入的任意命令/路径 |
| **路径不越界** | `working_dir` 与日志路径复用 `security.rs` 的 `safe_join`/`is_safe_filename` 判定 |
| **不做任意 shell 拼接** | 命令按「程序 + 参数」拆分执行，拒绝 `|` `&` `>` 等 shell 元字符 |
| **日志不含对话内容** | 日志只记录启动/停止事件、PID、退出码、健康检查结果；**不记录子进程 stdout 内容**（上游可能把请求打进 stdout，落盘即泄露对话） |
| **运行状态不持久化** | `run` 保持内存态（state.rs 已有 `skip_serializing`），进程重启后一律 `Stopped`——天然避免「死进程显示为运行」 |

---

## 7. UI 交互

### 7.1 模块卡片（`renderModules` 扩展）

- 新增**运行状态徽标**：`running` 绿色「运行中」/ `starting` 黄色「启动中」/
  `stopped` 灰「已停止」/ `failed` 红「启动失败」+ 悬停显示 `last_error`；
- 操作按钮按状态切换：
  - `stopped` → 「启动」；
  - `starting/running` → 「停止」；
  - `failed` → 「重试」（= 先 stop 后 start）；
  - `enabled=false`（optional 停用）→ 「启动」禁用，提示「已停用，先启用」；
  - 未安装 → 保持现状「未安装」禁用。

### 7.2 环境页

- 「维护」分组新增两枚按钮：**一键启动 Dounai**、**全部停止**；
- 输出到 `maint-log`：逐模块 `启动/健康通过/失败` 一行，最终给汇总。

---

## 8. 状态机

```text
          start                    健康检查通过
  Stopped ───────► Starting ────────────────────► Running
     ▲                │  ▲                           │
     │                │  │ 进程退出                    │ 进程退出 / stop
     │   stop / 退出   │  │ (max_probe 超时)          ▼
     └────────────────┴──┴── Failed ◄────────────────┘
```

| 转移 | 触发 |
|---|---|
| Stopped → Starting | `start_module` |
| Starting → Running | health.url 2xx 或（无 url 时）进程存活 |
| Starting → Failed | 健康检查超时 / 进程提前退出 |
| Running → Stopped | `stop_module` |
| Running → Failed | 进程非预期退出（健康检查失败） |
| Failed → Stopped | 进程已回收后的兜底回落 |
| 任意 → Stopped | **启动器进程重启**（加载状态文件时强制） |

崩溃恢复已在 `state.rs::open` 实现（`Installing → Broken`、`run = Stopped`），
本设计不新增持久化状态，只补**内存进程表**：

```rust
// runtime.rs（新增）
struct ProcessManager {
    procs: Mutex<HashMap<String, ManagedProcess>>,
}
struct ManagedProcess {
    child: Child,
    module_id: String,
    run: RunState,
    last_error: Option<String>,
    healthy: bool,
}
```

---

## 9. 验收标准

> 标注法沿用 `ACCEPTANCE.md`：**通过**=有可复核证据；**转人工**=需人工点按。

### 9.1 单元与端到端（可自动）

| # | 验收项 | 预期 | 证据 |
|---|---|---|---|
| R1 | `start_module` 拉起已安装模块，健康通过 → `running` | 进程表有 PID，`healthz` 可达 | `runtime.rs` 单测 + 真实上游端到端 |
| R2 | 健康检查超时 → `failed` + `last_error` | 指向死端口的模块在 max_probe 后报错 | 单测（伪造超时） |
| R3 | `stop_module` 回收进程，端口释放 | `run=stopped`，PID 消失 | 端到端 |
| R4 | 进程被外部 kill → 状态回落 `failed`，不显示「在跑」 | 退出监听触发 | 单测（kill 子进程） |
| R5 | 未安装 / 已停用 / 非 command 入口 → 拒绝启动且原因可读 | 逐条报错文案 | 单测 |
| R6 | `start_all` 顺序正确且失败不阻塞 | 按 §4.3 顺序，缺一模块其余照常 | 端到端（真实上游缺 MoodVoice） |
| R7 | 启动幂等 | 重复 start 不产生第二个进程 | 单测 |
| R8 | 停止幂等 | 对未运行模块 stop 返回 `stopped` 不报错 | 单测 |
| R9 | `working_dir` 越界拒绝 | 篡改 manifest 路径后启动失败 | 单测 |
| R10 | 日志不含 stdout 内容 | 运行后检查日志文件无请求/回复体 | 端到端 + 文件检查 |

### 9.2 人工验收（需要桌面环境）

| # | 验收项 | 预期 |
|---|---|---|
| R11 | 卡片按钮「启动/停止」真实拉起/回收进程 | 按钮状态与徽标随之切换 |
| R12 | 一键启动 Dounai → 悬浮窗出现并可对话 | 三个上游 + `dounai.exe` 全部 `running` |
| R13 | 全部停止 → 进程表清空、端口释放 | `stop_all` 汇总日志 |

---

## 10. 实现范围（v0.2.0）

### 做

- `runtime.rs`：进程表 + spawn/stop/健康检查/退出监听（核心）；
- `commands.rs` 增 4 个命令并注册；`RunView` 并入 `list_module_states`；
- `bridge.ts` + `main.ts`：卡片按钮、徽标、环境页一键启动；
- `uninstall_module` / `purge_local_data` 增加「先停止」前置检查。

### 明确不做（后续版本）

- 进程守护/自动重启（崩溃后不自动拉起，只报状态）；
- 资源限制（内存/CPU 配额）；
- 启动时环境变量差异校验（依赖仍走 `probe_environment` 体检）；
- 多实例管理（悬浮窗自身的单实例保护归 `dounai.exe` 管）。

---

## 11. 与现有代码的对接点

| 现状 | 变化 |
|---|---|
| `state.rs` `RunState` / `set_run()` 无调用方 | `runtime.rs` 接管调用 |
| `ModuleView.run` 存在但前端不渲染 | `main.ts` 渲染徽标与按钮 |
| `main.rs` 注册 10 个命令 | 新增 4 个 |
| `commands.rs` `AppState` 无进程表 | 注入 `ProcessManager` |
| manifest `entry` 从未被读取执行 | `runtime.rs` 首次使用 |
