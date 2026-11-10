# 重构设计

启动器 v2 的设计决策与行为约束。角色枚举、manifest 字段、状态 schema、端口与契约探测路径以 [contracts/README.md](../contracts/README.md) 为单一真源。本文件不重复字段表，只写定下这些字段的理由，以及代码要遵守的规则。面向用户的信任披露见 [SECURITY.md](../SECURITY.md)。

## 1. 结论

沿用 Rust 与 Tauri 2，抽出不依赖 GUI 的 `launcher-core`，Tauri 层只做薄壳。重构在启动器单仓完成，三个上游不改代码。

依据有三条：

1. 环境管理、双源安装与进程生命周期都在 Rust 侧。换语言或换无 WebView 的 GUI 对这三块没有帮助，还会让现有下载器、安装事务与验签代码作废。
2. 现有 4000 余行引擎已有单元测试覆盖。`launcher-core` 独立后，CI 能跑真实断言，不必依赖图形环境。
3. 依赖解析改为三来源自适应，管理页改为顶层子 WebView。上游不必补 `uv.lock`，也不必放宽 CSP，跨仓依赖降为零。

`role` 直接沿用组装层 `DEFAULT_ENDPOINTS` 的 `input`、`llm`、`output` 键名，不需要翻译表。

## 2. 已定决策

| 决策 | 依据 |
|---|---|
| 启用 Tauri `unstable` feature | 同窗口子 WebView 是唯一不改上游 CSP 的内嵌路径。`admin-host` 薄层圈住 unstable API，升级时只改这一层。 |
| 信任根是用户确认的签名公钥 | 不设官方维护的审核名单，不需要长期运营的审核角色。下载源只负责传输，不承担授权语义。 |
| 安装包 `installMode: both`，并行交付便携 exe | 安装时可选当前用户或整机。`both` 必定索要 UAC，已接受；M5 必须实测并把行为写进 README 与向导首屏。 |
| 本版不做能力沙箱 | `permissions` 在 v2 仍只作声明。启动器提供来源、完整性、契约兼容可验证与进程隔离，不提供权限限制。 |
| 角色是五值封闭枚举，编译期内嵌 | 远端动态新增角色等于绕过契约自检。未知 `role` 硬拒，不降级成 `auxiliary`。新增数据面角色需要同时改组装层的端口、事件转发与解析代码，本版不做。 |
| 官方不保留短名 | 所有 `module_id` 用 `namespace/name`。namespace 只是命名，“官方”标签只由内置 K1 指纹验通授予。 |
| 旧版保留数量用户可配，默认 3 | pinned 实例不计配额。`data_dir` 永不自动删除。清理只在新版成功激活后执行。 |
| K1、K5 与打包分发由启动器开发者承担 | 两把密钥生成、catalog 签名与发布流水线都在启动器仓内。“官方”指启动器发布通道，不表示五个上游作者背书。 |
| 仅 Windows 10 / 11 | Job Object、`CREATE_NO_WINDOW`、NSIS 钩子、WebView2 引导器与注册表元数据都是 Windows 专属。只抽进程 trait 覆盖不了这些差异，还会让文档看起来已支持跨平台。 |
| 不读取 v1 包格式 | v1 包从未打包分发。`schema_version: 2` 直接定死，不设读取层与过渡期。 |
| 离线只做单包导入，并支持本地目录 | 目录实例的源码随时可变，完整性语义与 zip 不同，按源码工作区处理。 |
| 第三方 `shell` 可以填槽 | 核查依据见 [contracts/README.md](../contracts/README.md) 的“第三方界面”一节。披露要求见 [SECURITY.md](../SECURITY.md)。 |

## 3. 选型依据

| 候选 | 结论 | 依据 |
|---|---|---|
| Rust + Tauri 2，启用 `unstable` | 选定 | 下载器、安装事务与验签代码可直接复用。 |
| Rust + egui / slint | 排除 | 无 WebView 时，模块管理页不能在启动器窗口内显示。 |
| Go + Wails | 不推荐 | Ed25519 验签、zip 事务与命令拆分需要重做，上游也难以评审。 |
| CLI + 系统浏览器 | 排除 | 管理页不在启动器窗口内。 |
| A + React / Vue | 推荐的前端子选项 | 现有 52 KB 原生 TS 与手写 CSS 没有组件化，向导与内嵌容器成本更高。 |

iframe 与独立窗口的取舍见 [ADMIN_UI.md](ADMIN_UI.md)。子 WebView 是顶层浏览上下文，不受 `frame-ancestors` 约束，三个上游不需要改 CSP。

三条 API 约束来自 Tauri 官方文档与 `docs.rs/tauri 2.12.4`：

1. `Window::add_child` 需要 `tauri` 的 `unstable` feature。M0 起在 `Cargo.toml` 固定 Tauri 小版本。
2. `add_child` 仅 desktop 可用。Windows 上在同步命令或事件处理器里创建会死锁，必须用 `spawn_blocking` 加独立线程。
3. 子 WebView 默认不随父窗口缩放。需要 `auto_resize()`，或由 UI 在 resize 时统一下发位置与尺寸。

## 4. 分层与平台边界

分层规则有四条：

1. `launcher-core` 不依赖 Tauri。纯 Rust，CI 里能跑真实单测与端到端测试，不启动窗口。
2. Tauri 命令面只校验参数、转换结果，不写业务逻辑。现有 `commands.rs` 有 1836 行，业务逻辑混在命令里。
3. UI 只通过命令面访问 core，不直接读写文件系统。
4. `admin-host` 对外只暴露 `trait AdminHost { mount / unmount / reload }`，隔离 unstable API 的升级面。独立窗口保留为运行时 fallback。

Windows 专属代码圈进 `process/win` 单一模块边界。`launcher-core` 其余代码保持平台无关，不写跨平台 trait 的空壳。将来做跨平台时按这个边界替换。

说明：M0 之前的 17 处集成用例用 `if (!available) return` 空跑。core 独立后可以配真实断言门禁。

## 5. 分发形态与双根路径

每次 release 产出三个文件，共用同一版本号与同一份 `SHA256SUMS`：

| 产物 | 适用 | 要求 |
|---|---|---|
| NSIS 安装包 `DounaiLauncher_x.y.z_x64-setup.exe` | 绝大多数用户 | 挂 `installerHooks` 做卸载清理，自带 WebView2 引导器 |
| 便携 exe `dounai-launcher.exe` | 离线搬运、不想装机的环境 | 单文件，每次 release 一同产出并跑同一套冒烟断言 |
| `SHA256SUMS` | 双源分发 | 两个 exe 共用一份签名清单，避免便携版漂移 |

安装包预置 `bin/uv.exe`。首次运行不依赖外网即可起步。卸载钩子在 `both` 模式下要识别 HKCU 与 HKLM 两套注册表位置。

`perMachine` 把二进制装到 `%ProgramFiles%`，普通用户对该目录没有写权限。托管 Python、venv、模块包、catalog 缓存与子进程日志都要写，所以程序根与数据根必须分开。

```
[程序根 install_root：只读]
  currentUser  -> %LOCALAPPDATA%\DounaiLauncher\
  perMachine   -> %ProgramFiles%\DounaiLauncher\
  便携版       -> dounai-launcher.exe 所在目录
  ├─ dounai-launcher.exe
  └─ bin\uv.exe

[数据根 data_root：可写，始终用户级]
  ├─ runtime\python\
  ├─ instances\<instance_id>\   （venv\ data\ logs\）
  ├─ state\
  └─ webview\
```

路径规则有四条：

1. 所有路径由 `launcher-core::paths` 一处计算。UI 与 Rust 都不自己拼字符串。
2. `paths` 由安装模式与便携标志共同初始化，结果写进 `state/install.json`。后续启动一律读取，不重新推断。v2.3 只有 `both` 一种安装模式分支，安装时选择落在 `install_mode`。
3. `perMachine` 只放只读二进制。Python、venv、模块与状态每个 Windows 用户各一份，避免 ACL 问题与“一个用户装模块影响所有人”。
4. `UV_PYTHON_INSTALL_DIR` 与 `UV_CACHE_DIR` 都指向数据根。便携版优先 exe 同级，写探针失败再回退到 `%LOCALAPPDATA%`。

警告：升级或切换安装模式时，先重建 venv。uv 明确说明更改 Python 安装位置不会自动反映到现有虚拟环境。`state` 要记 `install_mode`、`python_install_dir` 与 `python_build_id`，不一致时提示重建，不能让模块带着失效解释器启动。

体积预算分三层，每个里程碑实测一次：

| 层 | 指标 | 预算 |
|---|---|---|
| 交付物 | NSIS 安装包（含引导器） | ≤ 25 MB；便携 exe ≤ 15 MB |
| 常驻 | 启动器空闲内存与 CPU | 无任务时不得轮询忙等 |
| 一次性 | 首次向导下载量 | UI 明示 MB 数并允许改镜像，不静默下载 |

Python、venv、wheels 与模块包不计入交付物体积，但要在向导里写清楚。三个产物都过体积门禁，便携版不得跳过。

## 6. 现有代码处置

下表的行号会随 M1 改写失效，只记处置结论。

| 资产 | 处置 | 理由 |
|---|---|---|
| `download.rs` 多源下载器 | 复用 | 已有点试、断点续传、镜像回退、sha256 校验与失败切源 |
| `install.rs` 安装事务 | 复用并扩展 | 已有 staging、commit、rollback 与两遍路径检查；需补逐文件哈希清单与解压总大小上限 |
| `signature.rs` 验签 | 直接复用 | 签的是剔除 `signature` 后的整份规范化 manifest，`files` 清单天然在覆盖内，加逐文件哈希不用改算法 |
| `commands.rs::peek_manifest_from_zip` | 直接复用 | 只读 zip 内真 manifest，不信任请求里传的对象 |
| `commands.rs::precheck_install` | 复用并收紧 | 已有“哈希必填 → zip 哈希 → 包内 manifest → 验签 → module_id 一致性”顺序 |
| `commands.rs::admin_url_for` | 直接复用 | 已做回环校验、`/settings` 路径校验与按运行状态判断 |
| `commands.rs::open_admin_window` | 直接复用 | 已记录 Windows 同步命令建窗死锁的规避方式 |
| `trust.rs` | 改造语义 | 已有“有签名必须验通”的单测，但只看 `value` 非空，且分不开“未信任”与“被篡改” |
| `catalog.rs` | 改造 | `CatalogEntry` 只有 5 个字段；精确 host 白名单挡不住 SourceForge 的动态镜像域名 |
| `env_probe.rs` | 升级 | 已探测 uv，但注释只把 uv 当加速项 |
| `runtime.rs` | 重写进程管理 | 惰性 `try_wait`、同步 `sleep`、无 Job Object、无启动互斥。多实例会放大竞态 |
| `commands.rs` 命令面 | 拆分削薄 | `ORDER` 常量写死启动顺序，发布版信任策略写死 `strict(Vec::new())` |
| `src/ui/main.ts` | 重做 | 原生 TS 加手写 CSS，无组件化 |
| `bin/sign_manifests.rs` | 已删除，替换为 `pack.rs` 与 `sign_catalog.rs` | 原工具只签单个 JSON，无 `files` 清单、无打包，没有给 catalog 签名的工具 |
| `tauri.conf.json` | 分两步改 | `unstable` 与固定版本已在 M0 落到 `Cargo.toml`，升级检查清单见 [TAURI_UPGRADE.md](TAURI_UPGRADE.md)；`nsis.installMode`、`webviewInstallMode` 与 `installerHooks` 留 M5 |

## 7. 子系统行为约束

### 7.1 环境引擎

安装包预置 `bin/uv.exe`，便携版首次下载。uv 是单二进制、无 Python 依赖，不写系统 PATH，不需要 UAC。

托管 Python 装到数据根：

```
UV_PYTHON_INSTALL_DIR = <data_root>/runtime/python
UV_PYTHON_NO_REGISTRY = 1
UV_PYTHON_PREFERENCE  = only-managed
uv python install 3.12 --install-dir <data_root>/runtime/python
```

三个上游 `requires-python` 都是 `>=3.11`，现有 venv 实测都是 Python 3.12.0，统一 pin 3.12。托管 Python 不写注册表、不写 PATH、不污染系统 Python，卸载时整个 `runtime/` 目录可删。

依赖解析按优先级三选一，启动器自己识别，上游不用改：

1. 有 `uv.lock` 时执行 `uv sync --frozen`。
2. 有 `requirements.txt` 时执行 `uv pip compile requirements.txt`，再 `uv pip sync`。
3. 只有 `pyproject.toml` 时按 `[project.dependencies]` 编译，再 `uv pip sync`。

解析结果写成 `state/deps/<instance_id>.lock`，内容是钉死版本加哈希的 pinned snapshot。启动器把快照写进自己的 `state/`，不写回模块目录。每次启动做一次 `uv pip sync` 校验，快照哈希变化时显式报错，不静默重建。

`optional-dependencies` 必须显式选择。三家上游都有 `dev`，`Dounai_input` 另有 `keyring` 与 `local-asr`。`runtime.extras` 默认 `[]`，不默认安装 dev。

说明：v1 把“三上游补 `uv.lock`”列为 M0 阻塞项。v2 改为可选项。有 lock 更严格，没有也能运行。

### 7.2 catalog 与密钥

GitHub Releases 与 SourceForge Files 放同一份 K5 签名的 `catalog.json`。启动器选一源拉索引、验签、按 URL 下包、验包哈希，不需要理解两套发布 API。

两个源的已知约束：GitHub REST API 未认证时每小时每 IP 限 60 次，不能每次启动查 `/releases/latest`；asset 会重定向，白名单要覆盖目标域名。SourceForge 的 `/download` 跳动态镜像域名，没有稳定 host，改用域名后缀集加逐跳重定向校验，最终以 sha256 为准。

`SourceAdapter` 按 catalog URL 注册，官方两源内置，第三方源由用户在设置里显式添加 URL 后生效。添加源不需要配置源公钥，信任来自包内签名。接口与不变式见 [contracts/source-adapter-v1.md](../contracts/source-adapter-v1.md)。

catalog 用 K5 签名，签的是索引、版本单调性与新鲜度。K1 只用于签官方 zip 包。

两钥分离修掉一处自证漏洞。共用一把钥匙时，K1 泄露后攻击者能签一份不含 `revoked` 的 catalog。`revoked` 撤不掉同一把密钥签出的旧 catalog。

轮换路径有两条：

1. K1 泄露时，用 K5 签 catalog，把 K1 指纹列入 `revoked`。
2. K5 泄露时，靠不超过 7 天的 `expires` 限制窗口，加带外公告与强制升级 exe。

警告：两条路径都必须有带外撤销，即公告加新版启动器内嵌新公钥。catalog 的 `revoked` 不能单独承担撤销。内置公钥由新版启动器写死禁用，不声称 catalog 能单独撤销。

私钥离线保管，不进 CI 变量、不入库。GitHub token 只允许用户在设置里自填，绝不内置。

### 7.3 包信任

zip 根下放 `manifest.json` 与模块文件。签名放 manifest 的 `signature` 段，不另加 `.sig` 文件，避免签名与内容脱钩。manifest 不写 `instance_id`，否则同一个 zip 装不了第二次。

`signature.rs::canonical_payload` 签的是剔除 `signature` 后的整份 manifest。`files` 清单在覆盖内，逐文件哈希覆盖实际字节，执行文件因此被间接签上。这修掉了 v1“只签元数据、执行文件无绑定”的问题，验签算法不改。

签名段必须带 32 字节 `pubkey`。Ed25519 的 `verify` 只返回布尔值，分不开“包完整但公钥未登记”与“包被篡改”。判定顺序固定：

1. 用 `signature.pubkey` 验签。验通表示包完整，且由这把钥匙签发。
2. 算该公钥的 SHA-256 指纹，查信任列表。
3. `signature.pubkey` 自己验不通时硬拒，不给确认入口。

`key_id` 与 `publisher.name` 只作展示。指纹一律从签名公钥字节推出，不让包自述。`trust.rs` 因此从“遍历已配公钥验签”改成“包内公钥验签加指纹白名单”。四级结果的分流见 [SECURITY.md](../SECURITY.md) 的安全边界。

安装顺序如下：

1. 校验 catalog 的 `sha256(zip)`。本地 zip 导入时该哈希由启动器自己算，只防传输损坏。
2. 用 `peek_manifest_from_zip` 读 manifest，不落盘。
3. 判定信任结果。需确认的走确认框，硬拒的立即终止。
4. 解压到 staging。保留现有两遍路径与符号链接检查，补解压前按 zip 元数据算总大小上限。
5. 逐文件核 `files` 哈希。多一个、少一个、不符都回滚。路径统一用 `/`，禁 `..`、绝对路径、反斜杠与 Windows 保留名。
6. 提交事务。启动前再核入口文件哈希，堵住验签与 spawn 之间的 TOCTOU。

`files` 只为 zip 发布包服务，由 `pack` 工具生成。目录实例不校验 `files`，也不参与信任判定。字段在 manifest v2 里一律存在，目录实例可用空对象，这样两种来源共用一份 schema。

两个包外锚补的是包内签名自证不了的两件事：

- **签名剥离**。攻击者删掉 `signature.value` 就能把“已信任”降成“未验证”。三道防线：catalog 对该版本声明 `signature_required` 与 `expected_signer_fp`；`installed.json` 记住每个实例的判定结果与发布者指纹，更新时“曾签名 → 现无签名”或指纹变化都硬拒；首次本地导入允许粘贴期望签名者指纹，填写后不符或无签名直接硬拒。
- **新鲜度**。包内签名不证明这是最新版，旧版合法签名包可以重放。TUF 规范把这类攻击叫 rollback 与 freeze，靠单调版本号加元数据过期时间防御。catalog 保留 `version` 单调、`expires`、`min_version` 与 `revoked`，本地记 `last_seen_version`。

注意：首次本地导入时，前两道防线都还不生效。删掉签名段的官方包可能被确认安装，用户不知道期望指纹时无法根除。UI 必须明示这个窗口，披露见 [SECURITY.md](../SECURITY.md)。

公钥管理写在 `state/trust.json`。用户粘贴 base64 公钥或导入文件，确认前展示完整 SHA-256 指纹。撤销立即生效：已装实例不卸载，但标记“发布者已不再受信任”并禁止自动更新。

### 7.4 标识与多实例

四层标识的判据是：两个包能同时装、能互相切换，就是不同变体或不同实例；一个应该替换另一个，就是不同版本。

`version` 只在同发布者指纹、`module_id`、`variant` 内比较。跨变体、跨 `module_id`、跨发布者一律不比，否则第三方写个 `99.0.0` 就能永远显示“有更新”。`variant` 默认 `stable`，禁止把变体塞进版本串。

目录、venv、`data_dir`、日志、端口、进程句柄与信任记录全按 `instance_id` 索引。`active[role] = instance_id` 是原子事务：停旧实例、起新实例并占规范端口、做 health 与契约自检、提交。任一步失败回滚到旧实例。这补上了 v1 的 `is_active → spawn → insert` 无互斥与 `purge` 未停先删。

第三方实例与目录实例都不能自动成为 active，用户显式切换才允许。默认各实例数据隔离，“共享会话数据用于对比”列为显式高级选项，SQLite 并发写会冲突。

旧版保留以 `state/retention.json` 的 `keep_versions` 为准，默认 3，设为 0 表示不保留。清理只删包文件、venv 与日志，且只在新版成功激活后异步执行。正在运行的非 active 实例跳过。

警告：`data_dir` 与用户源码目录永远不在自动清理范围内，只标记孤儿并提供手动入口。v1 的 `purge` 曾未停先删，这条边界不能放宽。

### 7.5 生命周期

| 现状 | 目标 |
|---|---|
| `ORDER` 常量写死四个 id | 顺序、端口、env 与 health 全部由 manifest 声明，`depends_on` 按 role 拓扑排序加冲突检测。加第三方模块不改启动器源码 |
| 只在 `view()` 时 `try_wait` | 独立 exit watcher 线程，进程退出即刻更新 UI 状态 |
| 同步 `sleep` 阻塞命令线程 | 健康检查放到异步后台任务，命令立即返回 |
| 启动无互斥 | 单实例一把锁加全局启动互斥，active 切换按 7.4 事务化 |
| 无 Job Object，靠 `taskkill` 兜底 | Windows Job Object 的 kill-on-close 加 `CREATE_NO_WINDOW`；先 CTRL_BREAK 优雅退出，超时再 kill |
| 不记子进程 stdout | 每实例 stdout 与 stderr 落盘并轮转，UI 可查看诊断 |

第三方模块、目录实例与官方模块走同一状态机，状态卡片按验签结果着色。卸载时先停进程，再删 venv，最后删包目录。`data_dir` 单独询问，用户源码目录不参与卸载。目录实例额外挂一条“启动前检测 HEAD，源码变更时禁止自动启动”。

### 7.6 界面

现有 UI 的问题是装完还不能聊：默认套装的 `model_packs` 为空，模型层默认系统提示词为空。首次向导要走完整条链：

1. 体检。
2. 确认安装目录，说明双模式与 UAC。
3. 安装或复用 Python 3.12。
4. 选套装。
5. 从双源装模块。
6. 解析依赖并建 venv。
7. 一键启动。
8. 做契约自检。
9. 引导配置模型。
10. 发一轮真实对话验证。

主界面分五块：按 role 分组的模块卡片、环境页、内嵌管理页容器、下载与日志、设置。卡片内列实例、active 徽标与切换入口。设置里有下载源、镜像、代理、已信任公钥与完整指纹、`keep_versions`、`both` 与 UAC 说明、遥测开关。另有两个导入入口：“导入本地 zip”与“导入本地文件夹（Git 仓库）”，确认框都显示“无新鲜度锚、不自动更新”。

七条真机 WebView 冒烟断言必须实测：

1. 收起后状态正确。
2. 悬浮球可恢复。
3. 缩放后输入框可见。
4. 子 WebView 随窗口缩放不脱版。
5. 未验证实例的确认框默认焦点在“取消”。
6. 第三方模块卡片与安装框可见“非沙箱”文案。
7. 切换 active 失败后旧实例仍在运行，徽标未变。

密钥边界见 [ADMIN_UI.md](ADMIN_UI.md)。内嵌管理页不等于代存密钥：子 WebView 是独立 origin，Cookie 与 localStorage 天然隔离，启动器不读写配置。

管理页看起来像启动器自带的页面，容易让人误解密钥归属。启动器主 UI 要渲染一条常驻横幅，写明边界。

### 7.7 子 WebView

渲染路径：主窗口用 `Window::builder` 创建无 WebView 的壳，主 UI 作为一个子 WebView。点击某实例的“管理”时，在内容区 `add_child` 一个指向回环 URL 的 `WebviewBuilder`，尺寸由 UI 一处计算、core 一处下发。

四条行为约束：

1. URL 只能来自 `admin_url_for`。子 WebView 只允许 `http://127.0.0.1:<声明端口>/...`，不接受前端传任意 URL。非 active 实例用备用端口时，URL 取该实例的实际分配端口。
2. 在独立线程或 async 命令里创建。M4 要在真机验证连续切换多个模块管理页不卡死主线程。
3. capability 按最小集显式开启 `create_webview` 等权限，不整包放开 `core:webview:*`。
4. 生命周期绑定进程状态。实例未启动显示本地占位页，health 变 200 时 reload，进程退出时切回占位，卸载时销毁子 WebView。label 按 `instance_id` 派生，重复点击复用。

子 WebView 创建失败时退化为独立窗口，不丢状态。三个上游不需要改 CSP 或加反向代理，理由见 [ADMIN_UI.md](ADMIN_UI.md)。

## 8. 与 SECURITY.md 的分工

[SECURITY.md](../SECURITY.md) 写面向用户的披露：四级验签结果、已知限制、密钥归属与漏洞报告流程。本文件写给开发者的行为约束。

安装确认框、模块卡片与 README 三处都要写明同一件事。

文案是“本版不提供能力沙箱，模块拥有当前用户权限”。不得用“安全”“已隔离”这类模糊措辞代替。
