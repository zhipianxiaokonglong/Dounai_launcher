# 测试方法学

以下记录启动器测试中三个容易让测试本身失效的陷阱。这些不是产品缺陷，但会让“测试通过”变成假象。

## 增量编译缓存让结果失真

变异测试（故意注入缺陷，验证测试能否抓住）出现过以下序列：

1. 注入缺陷，测试失败。
2. 还原代码，测试仍失败。
3. 再次注入，测试失败。
4. 清理该库的构建产物后，测试通过。

原因是还原 `security.rs` 后，增量编译没有重新编译该库。测试链接的仍是注入缺陷后的旧 `rlib`。清理移除了 4.1 GiB 构建产物，行为随即恢复。

这会制造假阳性，也会让“修好缺陷但测试仍失败”被误读。

警告：做变异测试或修改安全关键代码后，先清理构建产物再验证。清理会删除本地编译缓存，重新构建耗时较长。

```powershell
cargo clean -p dounai-launcher
cargo test --all-targets
```

CI 每个 job 都是干净环境，不受此影响。

## 断言过弱会掩盖防线退化

`fuzz_safe_join_never_escapes_destination` 只断言被接受的路径仍在目标目录内。删掉 `safe_join` 里 `ParentDir` 的显式拒绝后，这条测试仍通过，因为末尾的包含性复核把越界兜住了。

纵深防御是双保险。第二道还在时，不能默许第一道失效。否则防线少一层，测试不会报警。

应对方式是新增 `fuzz_attack_strings_are_explicitly_rejected_by_design`。该测试要求攻击串返回语义明确的错误类型（`Traversal`、`Absolute`、`ReservedName`），而不是随便报错就算通过。注入缺陷后这条会准确失败，其余 7 条仍通过，说明该测试守的是那道具体的防线。

## fuzz 覆盖

`tests/fuzz_invariants.rs` 有 8 项测试，约 4300 次迭代。

| 不变量 | 覆盖方式 | 迭代数 |
|---|---|---|
| `safe_join` 不越界 | 拼路径池片段、随机分隔符、随机字符 | 2000 |
| 攻击串按设计拒绝 | 显式枚举 14 个真实攻击串 | 14 |
| 解压不越界、不 panic | 纯随机字节喂给 zip 解析与解压 | 300 |
| 恶意条目整体拒绝 | 14 个恶意条目各构造一个混合包 | 14 |
| 正常条目不被误拒 | 5 个正常路径反向验证 | 5 |
| 清单解析不 panic | 随机字节 | 500 |
| 清单结构化变异 | 以真实清单为基底做翻转、截断、插入 | 300 |
| base64 解码不 panic | 随机可打印字符 | 800 |

不用外部 fuzz 引擎的原因：

1. 不需要 nightly 与额外工具链，CI 直接可跑。
2. 种子固定（`0xC0FFEE`、`0xDEADBEEF`），失败可复现。
3. 覆盖真实攻击面（zip 头、路径字段、JSON 结构），而非纯随机噪声。

代价是不追求覆盖率最大化，只保证已知攻击面的不变量成立。

fuzz 最常见的失败模式是“全部拒绝”也能通过。因此有一条 `fuzz_good_entries_still_accepted`，确保正常路径不被误拒。

## 复现命令

```powershell
cd src-tauri
cargo test --all-targets
cargo test --test fuzz_invariants
```

做变异测试时先清理：

```powershell
cargo clean -p dounai-launcher
cargo test --test fuzz_invariants
```

注意：M1 抽出 `launcher-core` 后，包名会变化。命令里的 `-p dounai-launcher` 需随新包名调整。M1 的复现命令见下一节。

## M1 环境引擎验收（2026-11-10）

结论：六条验收里，第 1、3、5、6 条在本机实测通过；第 4 条是模拟通过，只约束 uv 离线、没有物理断网；第 2 条的路径项通过，注册表比对未实测；“没有 Python 的干净 Windows”这一环境前提也不成立。未实测项集中列在后面。

### 环境

| 项 | 实测值 |
|---|---|
| 系统 | Windows，已装系统 Python 3.11、3.12、3.13、3.14 |
| uv | 0.13.7（316c827e0 2026-11-03 x86_64-pc-windows-msvc），独立二进制；全局 uv 仍是 0.11.7 |
| 托管 Python | `cpython-3.12.16-windows-x86_64-none`，装在 `data_root/runtime/python` |
| 三个上游 | `E:/Dounai-ai/Dounai_input`、`OmniNexus`、`MoodVoice` |
| 夹具 | `src-tauri/core/src/bin/env_smoke.rs`，一次性工具，验收后可删除 |

注意：本机不是“没有 Python 的干净 Windows”。引擎用 `UV_MANAGED_PYTHON=1` 加 `UV_PYTHON` 绑定托管解释器，实测 `python_build_id` 是托管的 3.12.16，不依赖系统 Python。干净机与 `perMachine` 的管理员会话留到 M5 复跑。

### 六条结果

| ROADMAP 条目 | 结果 | 证据 |
|---|---|---|
| 1 三个模块 health 全 200 | 通过 | 三个实例 prepare 后 spawn，18101、18102、18103 的 health 全部返回 200 |
| 2 双模式与只读程序根 | 路径项通过，注册表项未测 | `current_user` 与 `per_machine` 各用一套独立根目录 prepare 成功；两次的程序根文件清单前后一致；把 `install_mode` 改成 `portable` 后，引擎拒绝并提示路径不一致。注册表比对未实测 |
| 3 篡改快照 | 通过 | 改 llm 锁文件一个字节后 `verify_instance` 报 `SnapshotMismatch`，锁文件未被覆盖；夹具随后还原字节 |
| 4 断网重跑 | 通过（模拟） | 带 `--offline` 重跑，三个实例 prepare 成功。本次没有物理断网，靠 `UV_OFFLINE=1` 与全程 `--offline` 约束 uv。“venv 未重建、托管 Python 未重复下载”只有 mock 单测断言，真机夹具没有采集这两项 |
| 5 三个上游 `git status` | 通过 | prepare 前后 `git status --porcelain` 输出一致。`OmniNexus` 与 `MoodVoice` 本来就有未跟踪文件，断言的是“前后不变”，不是“输出为空” |
| 6 dir 实例 `git status` | 通过 | 同第 5 条；`.venv`、`uv.lock`、`__pycache__` 等标记前后一致，没有新增 |

说明：第 4 条里的 `--offline` 走的是 `prepare_instance`（显式重跑准备），会重新生成锁文件与快照哈希。启动路径 `verify_instance` 只读状态，不重建 venv，也不重新解析依赖。

### 首次下载量的数据接口

M1 不做向导文案，只提供两个数。`env::cache_usage(paths)` 返回 `UV_CACHE_DIR` 的字节数与文件数，`UvBinary.downloaded_bytes` 是本次下载的 uv 资产字节数。M4 的向导从这里取数。

### 真机跑出的三处修正

1. `UV_MANAGED_PYTHON` 与 `UV_PYTHON_PREFERENCE` 不能同时出现。uv 0.13.7 报 `the argument UV_MANAGED_PYTHON (environment variable) cannot be used with --python-preference`。引擎只留 `UV_MANAGED_PYTHON`，另用 `UV_PYTHON` 绑定解释器。
2. `uv python list --only-installed` 会连系统 Python 一起列出，且输出路径可能相对数据根。引擎按安装目录标记筛选行，并直接采用输出里的解释器路径。
3. 托管 Python 的目录名不带补丁号（`cpython-3.12-windows-x86_64-none`），输出里的版本标识带补丁号（`cpython-3.12.16-windows-x86_64-none`）。按版本标识拼路径会找不到 `python.exe`。

以上三处已在 `python.rs` 与 [DESIGN.md](DESIGN.md) 的 7.1 节同步。

### 未实测项

1. 注册表 HKCU、HKLM 的 Python 键快照。本机安全策略禁止启动 `reg.exe`，夹具默认不做这项检查，报告里的 `registry_unchanged` 是 `null`。加 `--check-registry` 才会在允许该命令的环境里比对。
2. 没有 Python 的干净 Windows，以及 `perMachine` 的管理员会话与 UAC 时机。两项属于 M5 的真机项。
3. NSIS 安装包与便携 exe。M1 不产安装包。
4. `uv_lock` 分支的真机命令。三个上游都没有 `uv.lock`，单测用 `MockUvRunner` 断言 `--frozen`、`UV_PROJECT_ENVIRONMENT` 与 cwd，真机未跑。M2 打包出现 uv_lock 包时，按第 5 条复跑一次。
5. 本机主 workspace 的 Rust 构建。M3.0 已把 `async-executor` 钉到本机缓存的 1.14.0，只改 `Cargo.lock` 一处，`src-tauri/` 下的 workspace 现可离线编译。该项已关闭，CI 走 crates.io 不受影响。

警告：不要把 `registry_unchanged: null` 读成“注册表没有变化”。这个字段为 `null` 时只表示本次没有检查。

### 复现命令

在 `src-tauri/` 下运行。四个路径按本机布局替换。本机主 workspace 依赖解析失败时，Rust 测试改用文末的镜像 crate 命令。

```powershell
# 每次 PR 的 mock 单测
cargo test -p launcher-core

# 真 uv 集成测试（需联网，标了 #[ignore]）
$env:DOUNAI_TEST_UV = "C:/path/to/uv.exe"
cargo test -p launcher-core --test env_engine -- --ignored --nocapture

$smoke = @(
  "--install-root", "C:/temp/dounai-m1/app",
  "--data-root", "C:/temp/dounai-m1/data",
  "--uv", "C:/temp/uv-new/uv.exe",
  "--input", "E:/Dounai-ai/Dounai_input",
  "--llm", "E:/Dounai-ai/OmniNexus",
  "--output", "E:/Dounai-ai/MoodVoice"
)
# 第 1、2、5、6 条：三实例 prepare 与 health
cargo run -p launcher-core --bin env_smoke -- $smoke
# 第 3 条：篡改 llm 快照，夹具跑完还原锁文件
cargo run -p launcher-core --bin env_smoke -- $smoke --tamper --no-spawn
# 第 4 条：离线重跑，不物理断网
cargo run -p launcher-core --bin env_smoke -- $smoke --offline --no-spawn

# 本机主 workspace 解析失败时，用镜像 crate 复跑 Rust 测试
cd .internal/core-check
cargo test --offline
```

第 2 条的 `per_machine` 要先把 `$smoke` 里的 `--install-root` 与 `--data-root` 换成另一套目录，再加 `--mode per_machine`；两种模式不能共用同一个数据根。允许 `reg.exe` 的环境可在任一命令后加 `--check-registry`，检查跑不成会记进 `errors`，不会写成通过。

夹具的参数还有 `--offline`、`--verify-only`、`--tamper`、`--mode per_machine`、`--no-spawn`、`--port-base` 与 `--check-registry`。最近一次报告写在 `<data_root>/state/env_smoke.json`。本次 health 200 的副本留在 `.internal/m1-reports/health-200.json`，该目录不入库。

## M2 状态格式变更后的复核（2026-11-11）

依赖锁、元数据与 venv 都改成按 generation 落盘，`runtime.json` 成为唯一提交点。venv 先建在 `.staging` 目录，全部校验通过后才 rename。`prepare_instance` 接了 `source_kind`：zip 包硬比 pinned 哈希，dir 实例把哈希变化记进 `runtime.json`。契约字段与取值未变，只改落盘路径与说明文字，细节见 [contracts/README.md](../contracts/README.md)。

B1 新增 5 项单测，覆盖 zip 的缺 pinned、不符与相符三类。dir 侧覆盖哈希变化记录与旧 generation 保留。`write_bytes_once` 覆盖幂等与冲突，路径编码覆盖非法哈希。

B2 新增 2 项单测。一项断言新建失败后不留正式 generation 目录，另一项断言换 generation 时旧 venv 不被改写或删除。清理逻辑另有单测覆盖不完整目录会删、完整目录会留。

镜像 crate 里 111 个单测与 3 个集成测试通过，1 个真 uv 测试仍标 `#[ignore]`。

夹具的 `--tamper` 改为按当前哈希定位内容寻址锁，翻转一字节后仍断言 `SnapshotMismatch`，并用 Drop guard 还原。**真机 `env_smoke` 本轮没有重跑**，上面的 health 200 与六条验收仍是 11-10 的结果，不能当作新格式的实测证据。真 uv 子进程路径、离线文案分类与 cwd 规范化都还等一次实机或夜间 workflow 复核。

## 审查后的修复（2026-11-11）

`write_bytes_once` 改为临时文件加 rename，崩溃不再留下“文件名是完整哈希、内容只有半截”的 generation 文件。`commit` 前清理这类残损文件，让重跑能自愈；内容冲突与元数据读取失败改报 `SnapshotMismatch`。回滚测试把 `venv_python` 一并改回旧代，并补上旧 venv 缺失时应失败的反向断言。`entry.env` 键名校验收紧为全大写，删掉 M1 遗留的 `instance_venv_dir`。`validate.mjs` 增加断言总数下界，误删断言不再判绿。本轮新增 3 项单测，覆盖残损文件修复、跨 generation 元数据拒绝与键名大小写。

## M2 主体离线验收（2026-11-12）

M2 主体逻辑落在 `src-tauri/core/src/m2/`，含 10 个模块。网络只经 `HttpFetcher` trait 进入，测试用 FakeHttp 注入。M2 收尾时 Tauri 命令面未接线，M3 已全量接线。

`core/tests/m2_engine.rs` 的 9 个端到端测试串起双源、验签、安装、信任、状态与导入：

- 一源不可用时从另一源拉 catalog 并装完；catalog 版本回退、签名被改、两源都不可用都被拒。
- zip 改一个字节被拒；`files` 清单多一个、少一个、哈希不符都整体拒绝，安装目录不落盘。
- 删签名段、换签名公钥、发布者指纹变化都判硬拒。未登记公钥先按 B 级处理，TOFU 后升为 A，撤销后重回硬拒。首次本地导入填写的期望指纹不符，或填了指纹却删掉签名段，同样硬拒；没有包外锚时未签名包才走 C 级单次确认。
- 版本单调只在同 `variant` 内比较；契约不匹配的包不激活；清理保留 active 实例，只标记 `data_dir` 不删。`keep_versions=3`、pinned 与运行中实例的保护有单测。
- 目录导入不复制源码，HEAD 变化后要求重新确认。源码目录与启动器根目录的路径边界、无 `.git` 时改用快照哈希都有单测。

镜像 crate 里 143 个单测、3 个集成测试与 9 个 M2 端到端测试通过，1 个真 uv 测试仍标 `#[ignore]`。`cargo fmt --check` 与 `cargo clippy --all-targets -- -D warnings` 通过。契约校验器 107 项断言、CI 等价的 ajv schema 编译与状态样例数据级校验通过。

以上只验证逻辑层。真实两源重定向与签名 catalog 分发没有实测。UI 确认框、默认焦点与第三方模块卡片文案也没有实测。这些结果不能当作 [ROADMAP.md](ROADMAP.md) 的真机验收。

## 待真机或后续里程碑复核

- `uv pip compile --generate-hashes` 对同一输入两次输出是否逐字节一致。generation 哈希依赖这份输出；不一致会让每轮都新建 generation。
- `.staging` 路径按哈希固定，同一实例并发 prepare 会互相删除。`prepare_instance_env` 已按实例串行化，卸载与 purge 共用同一把锁，风险关闭；并发真机未测。
- CI 的数据级校验只覆盖 `examples/state/` 两份手写样例，挡不住 Rust 写端字段漂移。可在 Rust 侧把序列化结果喂给 schema，或由 CI 重新生成样例。
- `write_json_once` 写 install.json 仍直接写目标文件，中途崩溃会留下半截 JSON。下次启动读失败后重装即可，不像 generation 文件那样永久卡住，暂不改动。

## M3 生命周期与命令面离线验收（2026-11-13）

结论：M3 的静态门禁全绿，六条真机验收均未实测。按 [ROADMAP.md](ROADMAP.md) 的规则，离线断言不计通过。

静态门禁在 `src-tauri/` 下跑：`cargo fmt --all -- --check`、`cargo clippy --offline --workspace --all-targets -- -D warnings`、`cargo test --offline --workspace --all-targets`。M3.4 删掉 v1 后共 11 组测试目标零失败，其中 app lib 86 项、core lib 152 项、M2 端到端 9 项、生命周期 11 项。`cargo test --test fuzz_invariants` 8 项与 `--test sign_real_manifests` 2 项单独跑过，`ci.yml` 的 fuzz 前置依赖没有因删 v1 模块失效。前端 `npm run typecheck` 与 `vite build` 通过。契约 109 项断言、ajv 的六份状态 schema 编译与两份状态样例数据级校验通过。

M3 代码落在四处：core 的 `lifecycle/` 纯逻辑、app 的 `process/win.rs` 与 `lifecycle_runtime.rs`、`commands_v2.rs` 与 `state_v2.rs`。命令面保留 v1 的 21 个命令名与返回形状，另加 11 个 v2 命令，`src/bridge.ts` 零改动。

| ROADMAP M3 条目 | 状态 | 离线断言覆盖 |
|---|---|---|
| 1 强杀启动器无残留子进程 | 未实测 | Job Object 的 kill-on-close 有单测，没有强杀实测 |
| 2 并发两次启动只起一个 | 未实测 | `begin_start` 的 `AlreadyStarting` 有单测，`start_lock` 无并发真机断言 |
| 3 崩溃 5 秒内 UI 变红 | 未实测 | watcher 与事件推送有单测，没有计时实测 |
| 4 health 期间命令不阻塞 | 未实测 | 后台任务与内存快照有单测 |
| 5 第三方崩溃不影响官方 | 未实测 | 按实例 Job 与拓扑排序有单测 |
| 6 目录变更确认前不自启 | 未实测 | `require_confirmed` 有单测，启动编排调用路径未经真机 |

### 已知限制

1. 日志只记录启动器事件（拉起、退出码、探测结果），子进程的 stdout/stderr 默认丢弃，需显式设置 `DOUNAI_LOG_CHILD_OUTPUT=1` 才接管管道；开关交给界面属 M4。日志目录没有显式 ACL，随数据根继承权限。
2. 内置 K5 公钥是真实值。官方 catalog 已于 2026-11-13 起在 SourceForge 签发（2026-11-14 按 M8 新契约重签为 `sequence=2`，见 [ROADMAP.md](ROADMAP.md) Backlog 的操作记录）。签发流程当时的口径：`refresh_catalog` 对验不过、过期或字段不合法的 catalog 一律按 `unavailable` 返回，不降级放行。当时 `set_k5_pubkey` 可在本次会话内替换公钥，界面入口在 M4；M8 已删除该命令——能在会话内换掉验签公钥是一条绕过通道。
3. `switch_active` 已按 `activation_steps` 执行停旧、起新、探测、提交与回滚。限制有两条：目标实例由外部进程提供时直接拒绝（验不了契约、也无法回滚）；目标已是 active 且在跑时只重新探测，不重启进程。
4. 卸载的 `remove_files` 会清包目录、venv 与日志，`data_dir` 按契约保留；`purge_local_data` 先停托管实例再删目录，外部实例停不掉，跳过并记进 `errors`。
5. `purge_local_data` 的 `state` 只重置 catalog 水位，保留实例表，与 v1 的清状态语义不同。
6. 实例视图只走 `list_instance_views`：它已内嵌生命周期快照、端口、active、prepared 与清单摘要（`name`/`admin_ui`），`list_module_states` 与 `list_instances` 已删除。

## M3 收尾四项（2026-11-13）

结论：四项都只有离线断言，真机行为未实测。

1. 每实例诊断日志。新增 `src/diag_log.rs`：`LogSink` 按 2 MiB 轮转、保留 3 份，复用 core 的 `rotation_plan` 决定删哪些文件；`read_tail` 从最新文件往回读，最多 300 行与 256 KiB，越过窗口即标记 `truncated`。`Supervisor` 在拉起、spawn 失败、退出码、探测结果、停止请求与停止结果六处写日志。`read_instance_logs` 改为回读真实文件，并新增 `files`、`truncated` 两个字段说明窗口。子进程 stdout/stderr 默认丢弃，只有 `DOUNAI_LOG_CHILD_OUTPUT=1` 才走管道，管道按字节读到换行再按 UTF-8 有损解码，非 UTF-8 输出不会打断日志。单测 6 项：行格式、超长行截断、轮转保留份数、尾部读取与截断标记、目录不存在、开关字面量。
2. `switch_active` 事务化。按 `activation_steps` 执行停旧、在规范端口起新、健康与契约探测、提交槽位、清理；失败按 `rollback_steps` 停候选、重拉旧实例并报告。探测是同步等待，上限 8 秒，失败或崩溃立即返回。外部实例、规范端口被外部服务占用、同一实例已跑但契约探测超时都有明确报错。单测 2 项：四种输入形状的步骤序列、提交必须排在探测之后且后面只剩清理。
3. 实例级互斥。`AppStateV2::instance_slot` 按 `instance_id` 返回同一个 `Arc<Mutex<()>>`，`prepare_instance_env`、`uninstall_module` 与 `purge_local_data` 共用。单测 1 项：同实例槽位同一、不同实例互不阻塞。
4. 卸载与清理收尾。卸载的 `remove_files` 现在清包目录、`venvs`、`logs` 三处，`data/` 按 `never_auto_delete` 保留；`purge_local_data` 先调 `stop_all` 按拓扑逆序停掉托管实例再删，外部实例跳过并写明原因。

门禁：`cargo fmt --all -- --check`、`clippy --offline --workspace --all-targets -- -D warnings`、`cargo test --offline --workspace --all-targets` 全绿，app lib 从 87 项增至 96 项，11 组测试目标零失败。`npm run typecheck` 通过。

未实测：日志轮转在真实长跑实例上的表现、管道泵在子进程输出量极大时的背压、`switch_active` 在真实模块上的停旧起新与回滚、卸载正在被外部进程占用 venv 时的失败路径。

## 上游 main 修复移植（2026-11-13）

来源：`origin/main` 从 `a3e688f` 推到 `b8ab274`，四个提交（2026-11-12 18:50 到 21:10），对应上游的《测试与审计报告》。没有合并 main：v2 已删的 `commands.rs`、`runtime.rs`、`state.rs` 正是那批提交改的文件，合并会带回 v1 死代码并产生删除冲突。按功能面挑四项移植，分类越权修复按决定忽略。

| 项 | v2 落点 | 与上游的差异 |
|---|---|---|
| 显存 4 GiB 截断、磁盘硬编码 D 盘 | `src-tauri/src/env_probe.rs` | `probe_environment` 直接注入 `AppStateV2.paths.data_root`，不新增 `probe_environment_here` 命令；盘符提取抽成 `data_root_drive`，只接受 `X:` 形态，UNC 返回空。`bridge.ts` 的 `RustEnvReport` 补两个可选字段 |
| 外部已运行服务被误报启动失败 | `lifecycle_runtime.rs` 的 `external` 集合、`commands_v2.rs` 的 `StartOutcome` | 上游改进程表与 `RunView`；v2 把合成快照并进 `Supervisor::snapshot()`，`running()` 仍只数托管进程，停止与卸载明确拒绝，整批端口分配留在 `start_all` |
| 管理窗口暴露 Tauri 注入 | `open_admin_window` 的逐帧初始化脚本 | 与上游同一段脚本 |
| CI 缺前端依赖审计 | `ci.yml` 的 frontend job | 与上游同为生产依赖阻断、开发依赖报告两步 |

门禁：`cargo fmt --all -- --check`、`clippy --offline --workspace --all-targets -- -D warnings`、`cargo test --offline --workspace --all-targets` 全绿，11 组测试目标零失败，app lib 87 项，比 M3.5 记录的 86 项多 1 项盘符提取单测。`npm run typecheck` 通过，`npm test` 65 项全过，`vite build` 通过。契约 109 项断言与 ajv 六份状态 schema 编译通过。CI 工作流用临时安装的 YAML 解析器解析成功：5 个 job，frontend 8 步，含两步审计。上游 `b8ab274` 修的就是注释写坏导致工作流在解析阶段终止，这一步不省。

四项都未实测。显存取值依赖真实显卡与注册表，磁盘取值依赖真实卷。外部服务识别依赖真实 HTTP 服务，注入清理依赖真实 WebView。按 [ROADMAP.md](ROADMAP.md) 开头的规则，离线断言不计通过。

## K1 与 K5 密钥生成、打包与验签离线验收（2026-11-13）

结论：K1 与 K5 均已生成并内置。K1 签出的包在 M2 运行时判为 A 类，K5 签出的 catalog 能通过 `CatalogDocument::parse`。真机安装与 SourceForge 拉取未实测。

生成侧：`pack keygen` 加了防覆盖、用途标注与公钥登记文件。两把钥匙写到仓库外不同目录，ACL 移除继承权限后只授予当前用户。登记文件 `keys/K1.json` 与 `keys/K5.json` 只含公钥、指纹与用途，不含私钥材料。`state_v2.rs` 换上真实的 K1、K5 公钥与指纹，单测核对两枚指纹各自由内置公钥算出，并断言两把钥匙不相等。

运行时侧：`src-tauri/tests/signing_roundtrip.rs` 用发布工具打包与签索引，再送进 `verify_zip`、`CatalogDocument::parse` 与 `classify`。固定种子与真实密钥各跑一遍，3 项断言全过。真实密钥那一遍同时核对内置指纹与私钥匹配。篡改 `entry.command` 后判 D 类硬拒，篡改 catalog 条目后 `parse` 报验签失败。另用真实 K1 把 `contracts/examples/omninexus.v2.json` 打成 zip，`pack verify` 的验签与逐文件哈希通过；用真实 K5 签一份指向 SourceForge 的 catalog，`sign_catalog verify` 通过。

SourceForge 上传通道已实测。2026-11-13 用本机 SSH 公钥通过 `tonyjh07@frs.sourceforge.net` 认证。在该项目的 `/home/frs/project/dounai-releases/` 目录上传、列出并删除一个临时文件，三步都成功。目录已恢复为空。正式包与 catalog 尚未上传。

未实测：真机安装、从 SourceForge 拉取 catalog 与包、上传后的回读校验、私钥与上传凭据的保管过程。后两项属人工操作，离线断言不覆盖。按 [ROADMAP.md](ROADMAP.md) 开头的规则，这些结果不计真机验收通过。

说明：`algo` 的契约值已改为 `Ed25519`。签名输入是 `canonical_payload` 的字节，Ed25519 内部用 SHA-512。SHA-256 只用于逐文件哈希与公钥指纹。旧字面量 `SHA-256 + Ed25519` 由 `validate.mjs` 的负例断言拒收，断言总数从 107 增至 109。口径见 [SIGNING.md](SIGNING.md)。

## M4 界面重写离线验收（2026-11-13）

结论：界面骨架、六个视图与深浅双主题落地，前端门禁全绿。首次向导、内嵌子 WebView 与真机 WebView 冒烟未做。

结构：`index.html` 只剩骨架（左侧栏 232px、页头 60px、正文、状态栏 30px、模态挂点）。`src/ui/main.ts` 做内存路由与全局刷新，`store.ts` 管数据与派生查询，`dom.ts` 管转义与模态，`theme.ts` 管主题，`icons.ts` 提供内联 SVG。六个视图在 `src/ui/views/`。

新增两个后端只读命令：`list_instance_views`（实例 + 端口 + active + 快照合并成一屏所需的最小集合）与 `settings_snapshot`（信任表、保留策略、K1 与 K5 指纹）。两者都在 `main.rs` 注册，`commands_v2` 有单测覆盖合并且端口分配的边界。

文案：所有界面字符串按 STC `for-web-dev` 的 W1–W10 检查一遍。改动三处——`它们` 改写成「模块」、`去导入` 统一成「登记本地目录」、三条报错补上「接下来能做什么」。

视觉核对：用无头 Chrome 加载构建产物，逐页截图检查模块、套装、环境、日志、设置、导入本地六页，深浅两套各一轮；另按 860×600 复核窄窗口。修掉三处：几何字符在缺字体环境下渲染成方块（改内联 SVG）、窄窗口安装清单把「必需」拆成两行（行内说明改为省略不换行）、设置页默认停在「信任与公钥」而首项是「外观」。

门禁：`npm run typecheck` 通过，`npm test` 65 项全过，`vite build` 通过（产物 68 KB JS / 18 KB CSS），`cargo test --offline --workspace --all-targets` 全绿（app lib 97 项），契约 109 项断言未受影响。

未实测：真机窗口内六个视图的实际渲染、子 WebView 相关七条断言、首次向导、第三方模块在真机上的「非沙箱」展示、主题跟随系统在 WebView2 下的行为。截图来自无头 Chrome，不等于 WebView2 渲染结果。按 [ROADMAP.md](ROADMAP.md) 开头的规则，这些不计验收通过。

## 环境检测卡顿与闪窗修复（2026-11-13）

现象：真机点「重新检测」时界面整段点不动，同时连弹几个命令行黑窗。

两条独立成因：

1. **同步命令跑在主线程**。Tauri 的同步 `#[tauri::command]` 在主线程（事件循环所在线程）执行，`probe_environment` 内部是六次子进程探测，串行累加最长可到几十秒——期间 WebView 既不能重绘也不能响应输入。修法：命令改 `async`，探测体放进 `tauri::async_runtime::spawn_blocking`；六项探测互不依赖，用 `std::thread::scope` 并行，总耗时等于最慢的一项而非累加。`EnvReport` 结构不变，前端只补了「检测中」的进行中反馈。
2. **子进程没禁止控制台窗口**。Python、Node、nvidia-smi、PowerShell、curl、taskkill 都是控制台程序，从 GUI 进程 spawn 时 Windows 会给每个子进程分配新控制台，于是黑窗逐个闪一下。修法：`env_probe::hidden_command` 统一加 `CREATE_NO_WINDOW`；`core/src/env/console.rs` 提供同一套 helper，`runner.rs`（uv）与 `bootstrap.rs`（curl）接上；`process/win.rs` 的 `taskkill` 补同一标志。core 侧写常量而不引 `windows-sys`，避免为一个 flag 拉进整包绑定。

顺带修首屏：`boot()` 原先在四个后台请求（含环境检测）之后才 `wireNav()`，数据没回来时左侧导航点不动。改为先接导航与事件、先渲染首屏，再等数据回来重绘。

单测：`probe_environment_returns_report_without_panic` 覆盖并行改造后的报告完整性；`missing_runtime` 让探测线程 panic 时退化为「没有跑完」而不是冒充「未安装」，但当前没有稳定的注入点，未写断言。

门禁：`cargo fmt --all -- --check`、`clippy --all-targets -- -D warnings` 通过，`cargo test --lib` 100 项全过，`cargo test -p launcher-core` 全绿，`npm run typecheck`、`npm test` 65 项、`vite build` 通过，`cargo tauri build` 产出 2.90 MiB NSIS 安装包。

未实测：闪窗消失与卡顿解除需要在真机 GUI 会话里点一次「重新检测」确认；并发多次检测的行为未压测。

## 本地包导入入口（2026-11-13）

现象：界面只有「扫描目录 / 登记目录」，找不到装本地 .zip 的地方；模块页空状态却写着「或用离线包安装」。

根因是这条链路从来没接上过：`install_module` 命令与 `bridge.ts` 的封装都在，但新界面里没有任何调用点——`install_module` 要的是 `zip_base64`，前端得先把文件读成 base64，而桌面端既没有文件选择器也没有读文件的权限，所以那半个入口一直悬空。

修法：

- 新增 `inspect_local_package` / `install_local_package` 两条命令，直接收**路径**，由 Rust 自己读文件、算哈希、解包、判级，不走 base64 过桥。两者共用一个 `inspect_bytes`，判级只有一份。
- 检查与安装分离：先看清单、角色、签名状态与整包哈希，再决定装不装。安装时带上检查得到的哈希，文件在中途被替换会以「整包哈希不符」失败。
- B/C 类走确认框并展示完整公钥指纹，D 类不给确认按钮。
- 原本 `install_module` 的收尾逻辑抽成 `commit_install`，离线包与本地包共用，避免两套判级各自漂移。
- 文件选择器接 `tauri-plugin-dialog`，能力只加 `dialog:allow-open`；不放开 `fs` 插件，路径拿到后由命令自己读。

单测：`local_package_needs_confirmation_then_installs`（C 类拒绝未确认、确认后装成功、重复安装返回 replaced）、`local_package_hash_mismatch_is_rejected`（检查后文件被换掉必须失败）。app lib 102 项。

未实测：真机点「选择文件…」弹出的系统对话框、装完后的启动与依赖准备。命令层测试覆盖了判级与落盘，覆盖不了系统对话框。

## 本地包的拖拽与路径输入（2026-11-13）

只做「选择文件…」不够用：从浏览器下载完的包，用户手上就是一个路径，再走一次系统对话框是多余的一步。

- **拖拽**：Tauri 在 OS 层拦截拖拽，HTML5 的 `dragover` / `drop` 不会触发，必须听 `tauri://drag-enter|drag-over|drag-leave|drag-drop`（事件由 `emit_to_webview` 发出，`listen` 不带 target 即可收到，`paths` 就是绝对路径）。监听在模块加载时注册一次——事件挂在窗口上，每次重渲染都注册会叠加回调。拖入时按 `data-drag` 高亮投放区；落在别的页面则先切到导入页再检查，否则用户看不到结果。
- **路径输入**：检查前从输入框读值，而不是只信 `input` 事件——右键粘贴可能不触发，按 Enter 也能直接检查。Rust 侧剔掉首尾引号，兼容「复制为路径」带引号的形态。
- 只认第一个 `.zip`；拖进目录或其它文件时说明「模块包是一个 .zip，不是解压后的目录」。

门禁：`npm run typecheck`、`npm test` 65 项、`vite build` 通过。截图核对（无头 Chrome 1200×780，深浅两套）确认投放区、检查结果与两处路径输入框的排版正常。

未实测：真机拖拽事件与高亮。无头浏览器不产生 OS 级拖拽，这段只能在装好的启动器里验证。

## 自打包装不上自己的包：包内路径两套规则（2026-11-13）

现象：导入 `dounai-input-0.2.1.zip` 时界面报「读包失败：契约数据不合法：非法的包内路径：.dockerignore」。

根因不是包坏了，是两处规则各写了一套：

- `core::m2::manifest::validate_relative_path` 要求每个包内路径**首字符是字母或下划线**，安装与检查都走它，`.dockerignore` 直接拒；
- `release::safe_package_path` 是打包侧另一套，字符白名单里含 `.`，`release.rs` 的测试里还明确断言 `.gitignore` **应当通过**。

于是 `pack` 打出的包天然过不了 `pack` 自己的校验，只是打包后那次自检走的是 `verify_zip`，没经过清单解析，所以没暴露。任何带 `.gitignore`、`.dockerignore`、`.env.example` 的上游仓库都会踩。

修法：规则收敛到一处。`validate_relative_path` 改为按段校验——空路径、反斜杠、控制字符、首尾斜杠、空段与 `.`/`..` 一律拒，绝对路径、盘符与 Windows 保留名交给 `security::safe_join`，点开头的名字放行；`release::safe_package_path` 不再自己实现，直接调用 core 那个函数并转换错误类型。安全性没有下降：拦穿越的是 `safe_join`，首字符限制从来没负责过这件事。

单测：core 新增 `relative_paths_accept_dot_files`、`relative_paths_reject_traversal_and_devices`、`manifest_parses_when_files_contain_dot_names`；app 侧把 `unsigned_package()` 改成带 `.gitignore` 的包，`local_package_needs_confirmation_then_installs` 因此覆盖了「点开头文件能装进去」。另用临时集成测试把三个真实包（77 / 69 / 104 个文件）过了一遍清单解析，确认全部通过后已删除该测试文件。

门禁：core 155 项、app lib 102 项、`clippy --all-targets -- -D warnings`、契约 109 项通过。

未实测：真机重新导入这三个包并启动模块。规则层已证明可通过，端到端仍要在界面上点一次。

## 启动链路三个连着的毛病（2026-11-13）

真机反馈三件事：单实例点「启动」报「模块 dounai/dounai-input@stable 未安装」；一键启动报「已启动 0 / 3 个实例」，随后窗口卡十几秒；装完包不装依赖也能点启动。

**一、启动按 module_id 找实例，界面传的是 instance_id**

`start_module` / `stop_module` 收参数叫 `module_id`，内部调 `resolve_instance(module_id)`，只按 `module_id` 过滤。M4 重写后界面按实例操作，传的是 `dounai/dounai-input@stable`，于是永远匹配不到，报「未安装」。

修法：`resolve_instance` 先按 `instance_id` 精确匹配，再退回 `module_id`（active 优先）。两个命令面共用一套解析，界面传哪种键都对；找不到时的文案改成「没有找到实例或模块」，不再冒充「未安装」。

**二、生命周期命令跑在主线程**

Tauri 的同步命令在主线程执行。`start_all` 对每个实例先做端口短探（`PRESTART_TIMEOUT_MS`）再 spawn，三个实例串起来就是十几秒不重绘；`prepare_instance_env` 更糟，建 venv、跑 `uv sync` 可能几分钟。上一轮只修了 `probe_environment`，同类问题在启动链路上还在。

修法：`start_module` / `stop_module` / `start_all` / `stop_all` / `restart_instance` / `prepare_instance_env` / `switch_active` 一律改 async，实活进 `tauri::async_runtime::spawn_blocking`，状态从 `AppHandle::state` 取。命令名与参数形状不变，`main.rs` 注册表不用动。

**三、「已启动 0 / 3」是计数错了，不是没启动**

`Supervisor::start` 返回的快照是 `Phase::Starting`，健康检查在后台继续；界面只数 `run === "running"`，正在起的全被算成没起来。改成 `running` 与 `starting` 都算启动成功，并单独说明「其中 N 个正在等健康检查」。

**四、装完包没有装依赖这一步**

`build_start_request` 早就拒绝未准备的环境，但报错写的是「请先执行 prepare_instance_env」，界面又把「启动」摆在最显眼的位置，用户只能撞到这条开发者措辞。

修法：`list_instance_views` 增加 `prepared` 字段（`verify_instance` 探真实 venv，shell 角色恒真）。未就绪时实例行的主按钮就是「安装依赖」，附「依赖未装」徽标；`startOne` 若发现未就绪会先装再启，不让用户先撞一次错。菜单里两个入口：离线安装、允许联网安装。安装完成的提示改成「回模块页先点『安装依赖』」。

## 卡片菜单盖住卡片内容（2026-11-13）

现象：打开卡片底部「⋯」后，出现一块白板压住卡片正文，看着像卡片被挖空。

实测（无头 Chrome 1200×780，用 `getBoundingClientRect` 取值）：卡片 `x=254 w=290`，菜单面板 `x=181 w=168`——**面板左边越出卡片 73px**。原因是 `.menu-body` 用 `right: 0` 按 `⋯` 的右边缘向左展开，而 `.card-foot` 里的 `⋯` 紧跟「详情」排，没有贴右，于是面板整体左移压在正文上；浅色主题下 `rgba(0,0,0,.35)` 的投影几乎看不出来，边界只剩一圈 0.2 透明度的边，视觉上就成了「白板」。

修法：`.card-foot .menu { margin-left: auto }` 让 `⋯` 贴右，面板随之落回卡片内（实测 `x=513 w=168`，卡片右界 695）；浅色主题的投影改成 `rgba(15,23,42,.14)`；菜单项内边距 6→5px，面板高度 205→193px，展开后上边缘与卡片顶对齐不再顶到分组标题；`.cards` 的 `minmax(288px,…)` 提到 320px，实例行允许换行，`dounai/dounai-input@stable` 不再被截成 `…@s…`。

门禁：app lib 103 项、core 155 项、契约 109 项、`tsc`、`npm test` 65 项、`vite build`、`clippy -D warnings` 通过。深浅两套截图核对过菜单、依赖未装状态与实例行排版。

未实测：真机点「安装依赖」跑通 `uv sync`、以及三个实例真起来后的健康检查；无头浏览器造不出子进程与系统对话框。

## 遗留代码清理与在线安装接通（2026-11-13）

这一轮先做了只读审计，再按「零引用 → 有测试引用 → 数据模型」的顺序改，每一步都跑门禁。

**一、删掉的死代码**

`install.rs`（v1 安装事务）、`catalog.rs`（v1 清单解析，不验签且可回退磁盘缓存）、`trust.rs`（二态信任策略）、`module_probe.rs`（第三个手写 HTTP 客户端）、`src/localinstall.ts`（base64 装包通道）在审计里都已没有产品侧调用者。删前把 `fuzz_invariants` 的不变量搬到 v2 活路径上：

- 任意字节不 panic → `verify_zip`，并新增「验包阶段不落盘」的断言；
- 恶意条目名整体拒绝 → `verify_zip`，另加一条**正向**用例：用 `valid_package()` 造一个字段完整、`files` 哈希正确的包，走 `install_package` 落盘，断言目标目录的父目录下只多出目标目录本身。没有这条，「全部拒绝」也能让 fuzz 通过；
- `parse_catalog` 不 panic → `CatalogDocument::parse`。

`install.rs` 一删，`base64_decode` 与 `install_module` 就近失去了唯一调用者，一起收掉。

**二、删掉的死命令**

`list_module_states`、`install_module`、`list_instances`、`hash_file`、`get_lifecycle_snapshot`、`download_package` 六个命令在注册表里，但界面零调用。逐个确认后删除；`ModuleView` / `InstallStateView` / `InstallResponse` 三个 DTO 保留，`add_local_module` 与 `install_local_package` 仍在用。`admin_host` **没删**：它是 M0 冻结契约，M4 的内嵌子 WebView 要用，删了等于让 M4 重新推一遍。

**三、SourceForge 全链路 302**

接通在线安装时先撞到这一条：`master.dl.sourceforge.net` → `downloads.sourceforge.net` → `zenlayer.dl.sourceforge.net`，每一跳都是 301/302。原先两个客户端都是 `redirect: none` 且把 3xx 当错误，于是**目录和包都取不到**。

修法：两处都改成手动跟跳，每跳校验 host 是否仍落在 `ALL_HOST_SUFFIXES` 内，上限 5 跳。用 reqwest 的自动跟随不行——看不到中间跳，等于绕过白名单。

**四、清单数据模型对不上，管理页按钮永远不出现**

`manifests/*.json` 是 v1 清单：`module_id` 是 `dounai-input`，而实例是 `dounai/dounai-input`；`store.manifests` 用前者建键，`manifestOf(instance.module_id)` 于是恒为 null。连带三处症状：卡片描述退化、**「打开管理界面」按钮永不渲染**、未安装清单把已装模块又列一遍。

修法：`InstanceRuntimeView` 增加 `name` 与 `admin_ui`（读自模块根 manifest 的 `admin_ui.web_ui`），前端不再自备一份清单；`manifests/`、`src/manifest.ts`、`tests/admin-ui.test.ts` 与未安装清单一起删除。`sign_real_manifests.rs` 改指 `contracts/examples/*.v2.json`，并补上 v2 必填字段与 `module_id` 必须含一个 `/` 的断言。

**五、套装页删除，在线安装改放模块目录**

套装页的数据是 v1 的 `module_id` 列表，安装单位是 `module_id@variant`，「开始安装」一直是 disabled。按决定删掉整套功能（`bundle.ts`、`bundles/*.json`、`views/bundles.ts`、导航入口、对应测试），视图由六个变五个；在线安装放进模块页右上角的「模块目录」弹窗。

**六、`enable_dev_mode` 从未被调用**

四个本地模块命令都 `require_dev_mode`，但前端只显示状态、按钮只是跳转，等于「登记本地目录」整条支线不可达。设置页那颗按钮现在会先真的开开关，再跳转。

**门禁**

app lib 67 项、core 6 项、其余集成 20 项（`live_source` 2 项按设计 ignored）、契约 109 项、`tsc`、`npm test` 20 项、`vite build`、`clippy --workspace --all-targets -D warnings` 全绿。

**真源冒烟（`cargo test --test live_source -- --ignored`）**

2026-11-13 19:10 实跑通过：

```
目录可用：3 个模块，expires=2026-11-16T11:07:53Z
下载成功：dounai/dounai-input -> 1702593 字节，尝试 1 次
```

即：线上 catalog 用内置 K5 验签通过、三个条目的 sha256/size 与本地打包产物一致、整包下载后哈希与目录声明一致且能过 `verify_zip`。这两条不进 CI——CI 不该依赖第三方站点可用性。

**未实测**：真机点「模块目录」→「安装」→ 自动装依赖 → 启动的完整一条；catalog 7 天过期后的表现只能靠改系统时间或等过期验证。

## M1 收口：install.json 自举与内置 uv（2026-11-13）

用户报的原始症状是 `安装依赖失败：读取 install.json 失败：… launcher\state\install.json 读写失败：系统找不到指定的文件。 (os error 2)`。查下来不是文件被写坏，是**从来没人写过**：`install.json` 按设计由安装器（M5）写一次，M5 还没开始，启动器也不兜底。

**一、为什么不能只补一条 `initialize`**

`initialize` 遇到已存在的文件会返回 `AlreadyExists`，而现有用户的数据根里 `installed.json`、`trust.json`、`retention.json` 都在、只缺 `install.json`，看起来「接近正常」。真正需要区分的是四种情况，于是加了 `InstallState::ensure`：缺失则新建；四项一致则不写盘；只差 uv 三元组（升级了随包 uv）则只改三元组；读不出来或两个根不一致则先备份 `install.json.bak` 再重写。第三条的实现踩过一次坑——`fresh` 先被 `Ok((fresh, ...))` 移走、后面又借它拼说明字符串，编译器直接拒了，改成先拼字符串后移动。

**二、uv.exe 从哪来**

目标机器连不上 `github.com`（实测 21 秒超时），而 `locate_uv` 的兜底正是从 GitHub 下载，所以「允许联网补齐」也是死的。实际取数：阿里云 PyPI 的 `uv-0.13.7-py3-none-win_amd64.whl`（wheel sha256 `2835e2ab…`），里面 `uv-0.13.7.data/scripts/uv.exe` 就是 31 803 184 字节的可执行文件，`sha256:1791d7a9…`，实跑输出 `uv 0.13.7 (316c827e0 2026-11-03 x86_64-pc-windows-msvc)`。

`extract_uv_exe` 原本就按「任意层级下的 `uv.exe`」匹配，wheel 的脚本目录因此不需要改解包逻辑，直接可用。补齐顺序改成 `UV_DOWNLOAD_URLS`：GitHub 优先，镜像兜底，任一成功即停。

**三、三段落地，缺一段就白搭**

`Paths::bundled_uv_path()` 只认「exe 同级的 `bin/uv.exe`」。NSIS 安装后由 `bundle.resources` 落到 `$INSTDIR/bin/`，但 `tauri dev` 与直接跑 `target/release/*.exe` 时没有任何东西负责这个位置——不做 `build.rs` 这一段，开发与本地验收下的「随包 uv」等于不存在，测试会一直走联网分支而不自知。`build.rs` 于是把 `bin/uv.exe` 复制到 `target/<profile>/bin/`（大小没变就跳过）。实测 `target/debug/bin/uv.exe` 已生成，与源文件逐字节一致。

二进制按决定不入库：`.gitignore` 掉 `*.exe`，取数脚本 `scripts/fetch-uv.py` 校验两层哈希后原子写入。清空 `bin/uv.exe` 后重跑脚本，产物与原文件 `cmp` 逐字节相同。

**四、默认联网**

`prepare_instance_env` 的 `allow_online` 缺省从 `false` 改成 `true`。首次装依赖必然要拉解释器与 wheel，离线默认等于按钮天生不可用。界面侧：主按钮「安装依赖」默认联网，模块菜单保留「安装依赖（离线）」显式选择；`startOne` 在缺依赖时先装（联网）再启。

**五、验证方式**

`tests/bundled_uv.rs` 三条，都走生产代码，程序根取仓库 `src-tauri`、数据根取临时目录，不碰用户真实数据：

1. `AppStateV2::new` 补写 `install.json` → `InstallState::load` 读回三元组 → `locate_uv` 命中随包文件 → `uv --version` 真跑通。注入的 `UvFetcher` 一旦被调用就 panic，所以「没联网」是断言出来的，不是假设。
2. 删掉 `install.json` 再启动能自愈（M1 验收第 7 条）。
3. 把文件改成非法 JSON 再启动：`install.json.bak` 存在且内容就是被改坏的原文，随后正常重写。

另加一条常量对账：`state_v2::tests::pinned_uv_hash_matches_the_staged_binary` 在仓库里存在 `bin/uv.exe` 时核对长度与 sha256，防的是「升级了文件忘了改常量」。

**门禁**

core 159 项 lib + 23 项集成、app lib 69 项、`bundled_uv` 3 项、契约 109 项、`tsc`、`npm test` 20 项、`vite build`、`clippy --workspace --all-targets -D warnings` 全绿。

**未实测**：真机上从「安装依赖」到三个模块 health 200 的完整一条（需要用户机器上跑，且要真的联网拉包）；NSIS 安装包体积增量待构建结果填入 ROADMAP。

## M1 返修：zip 包缺 pinned 快照 + 依赖安装进度（2026-11-13）

真机复现的第一句是 `安装依赖失败：依赖准备失败：SnapshotMismatch: zip 实例缺少 manifest.runtime.snapshot_sha256，无法锚定依赖快照`。三个已装实例全部命中：SourceForge 上发布的三包 `manifest.runtime` 只有 `deps`、`python`、`venv`，没有 `snapshot_sha256`。

**一、口径为什么改，而不是给包补字段**

`kind = uv_lock` 的包，锚点本来就是签名包内那份 `uv.lock`：装包时验过 zip 的 sha256，同步前 `prepare_venv` 还会再比对一次「项目根 uv.lock」与解析出的快照哈希。发布者再写一个 `snapshot_sha256` 只是把同一个哈希抄两遍。旧口径（缺字段即硬拒）等于让所有 `uv_lock` 的在线包都装不了依赖，而字段本身是可选的。

改成：声明了就必须一致，不一致照旧硬拒（那才是篡改信号）；没声明则放行，把这次快照记成 `snapshot_pinned = false`，界面在实例行标「快照未锚定」，详情里说明「重装可恢复锚定」。`requirements` / `pyproject` 的 zip 包没有天然锚点，缺字段同样降级记录，不静默假装锚定。

`InstanceRuntime` 新增 `snapshot_pinned`，读旧 `runtime.json` 时缺省为 `true`——能写进去的 zip 实例当时都过了 pinned 比对。`list_instance_views` 只读一次 `runtime.json` 填 `env_pinned`，不参与任何判定。

**二、进度为什么要走旁路**

`EnvContext` 是 8 处结构体字面量的公开构造点，往里加回调字段会把「怎么显示进度」扩散到每个测试夹具。改用线程本接收器：`prepare_instance_with` 在同一个线程同步跑完，组装层用 `with_sink` 挂回调（带 Drop guard，panic 也不泄漏），没有接收器时全部空操作，单测与 `env_smoke` 行为不变。

阶段固定六步：`uv` → `python` → `resolve` → `sync` → `verify` → `done`，每步一节标题加一行细节。**不做百分比**：uv 不提供「已装 N 个」的稳定接口，按输出行猜出来的数字是假的，宁可只按阶段推进。

「正在装哪个包」靠流式读 stderr：有接收器时 `SystemUvRunner` 改成 spawn + 读线程，`sync_request` 追加 `-v`（没有接收器不加，测试参数不变）。行过滤只认「带 `==` 的 installing/downloading/prepared 行」，认不出就丢；同一行去重，整条准备最多 400 行。组装层再按 150ms 合并细节变化——阶段或标题变了立刻发。

前端只做两个端点加一条渲染：`beginEnvJob` 在点下去时先落「准备开始」，invoke 返回或抛错就是结束；失败原因留在卡片上（toast 会消失），成功整行抹掉。进度事件只改对应卡片那一行 DOM，不整页重绘——装包时这个事件每 150ms 就可能来一次，整页重绘会把滚动位置和展开的菜单都打回原样。

**三、验证**

core 新增 5 项：缺 pinned 放行且记 `snapshot_pinned = false`（用 `instance_snapshot_pinned` 复核）、pinned 不符仍 Hard reject 且不写 `runtime.json`、六阶段全部上报且最后一阶段是 `Done`、`lock_packages` 读 uv.lock 与 requirements 两种格式、`LineReporter` 只留包名行且去重。原 `zip_instance_requires_pinned_snapshot_and_rejects_mismatch` 拆成两条，旧的「缺字段 → 硬拒」断言按新口径改写。

前端新增 `tests/env-job.test.ts` 3 项：begin 立刻可显示、新事件保留开始时间并清掉旧错误、成功抹掉/失败留原因。

**门禁**：core 166 lib + 23 集成、app lib 69、`bundled_uv` 3、契约 109、`tsc`、`npm test` 23、`clippy --workspace --all-targets -D warnings` 全绿。

**未实测**：真机上「安装依赖」全程跑完并让三个模块 health 200——需要用户在安装包上跑一次，且要真的联网拉 wheel。`-v` 的包名行格式按 uv 0.13.7 的 stderr 写的，若上游改格式，表现为卡片只显示阶段不显示包名，不影响安装本身。

## M1 返修：启动被判超时与四倍启动请求（2026-11-13 20:20）

真机日志：12:20:51 四行 `[lifecycle] start role=Input ...`，只有一行 `spawned pid=40112`；12:20:57 `[probe] health=Unhealthy contract=Unhealthy`；随后 health 已能 200，再点启动提示「已在外部运行，本次没有重启」。

**一、四行 start 是前端重复绑定**

`renderModules` 每次刷新都调 `bindBodyActions(body)` / `bindActions(head, …)`，而 `body`、`head` 是常驻元素，`bindActions` 里直接 `addEventListener`。刷新一次多一个监听器，点一次按钮就把 `data-act` 处理函数跑 N 次。状态机 `begin_start` 的互斥把后三个挡成 `AlreadyStarting`，所以只有一个 `spawned`，但用户看到的是「启动失败：实例已在启动中」。

改法：`bindActions` 用 `WeakMap<HTMLElement, Map<act, run>>` 存表，容器只挂一个监听器，重复调用只替换表。`startOne`/`stopOne`/`restartOne` 另加按实例的在途标记，一键启停共用 `bulkBusy` 闸门——按钮在异步返回前仍可点，双击同样会发第二个请求。

**二、六秒判 Unhealthy 是预算口径错误**

`PROBE_BUDGET` 固定 4 秒，`timeout_ms`（3000）是单次请求超时，两者都不是「进程起来要多久」。Python 冷启动要加载解释器与全部 import，四五秒才监听是常态。日志里从 spawn 到 `probe` 6 秒，正好是「4 秒预算 + 一次卡满 3 秒的请求」。

改为 roles 契约里的 `startup_timeout_ms`：`input` 60 秒，`llm`/`output` 90 秒，缺省 60 秒。探测顺序也改了：先只等 health，通了再探契约——health 都没通时探契约只是白等一个请求超时。间隔前 5 秒 50 ms、之后 400 ms，避免长预算下每分钟上千个请求跟刚起来的进程抢 CPU。

**三、判失败不能丢掉 pid**

旧代码走 `mark_failed`，它把 `pid` 清成 `None`。界面于是认为没有进程，用户再点启动时 `start_instance` 的前置短探发现端口健康，就把这个实例标成「外部启动」——启动器自己拉起的进程，却失去了停止权，也解释不了「已在外部运行」这句提示。

- 新增 `mark_probe_timeout`：阶段 `failed`、保留 `pid`、`last_error` 写「启动超时：N 秒内健康检查未通过，进程仍在运行」。
- 新增 `mark_recovered`：只认「`failed` 且 `pid` 还在」的条目，收回到 `running`，`contract` 保持 `not_checked`（只确认了 health）。
- `Supervisor::owns` 与 `start_instance` 的 owns 分支：自己还持有进程时，健康就直接收回状态，不健康就明确要求先停止，绝不 `mark_external`。`mark_external` 自己加守卫，作为第二道保险。
- 界面 `hasLiveProcess` 让「启动超时」的实例显示「停止」而不是「启动」，模块级 chip 用 `启动超时`（warn）而不是 `启动失败`（fail）。

**四、验证**

- core 新增 3 项：超时保留 pid 且迟到探测不改写、`mark_recovered` 回到 `running`、无 pid 时拒绝收回。lib 166 → 169。
- 前端新增 `tests/bind-actions.test.ts` 3 项：重复绑定只留一个监听器、重绑后走最新处理函数、未登记的 act 不触发。
- 门禁：core 169 lib + 23 集成、app lib 69、契约 109、`tsc`、`npm test` 26、`clippy --workspace --all-targets -D warnings` 全绿。

**未实测**：真机上点一次「启动」只发一个请求、60 秒内转 `running`、以及超时后「先停止再启动」能走通。需要用户在安装包上复跑。

## M1 返修：启动即退出、端口撞车与「没有停止按钮」（2026-11-13 21:15）

真机复现两段日志：

```
[lifecycle] start role=Llm program=...\venvs\711b65f32b8a8f4e\Scripts\python.exe health=http://127.0.0.1:18101/v1/healthz
[lifecycle] spawned pid=37532
[lifecycle] exited code=3
[probe] health=Unhealthy contract=Unhealthy after=90316ms
```

再点启动返回「进程还在运行，但健康检查没通过。先点「停止」，再启动」，而卡片上只有「启动」。

**一、三个包都是 src 布局，而 venv 里没有包自身**

引擎 `uv sync` 带 `--no-install-project`（避免联网时还要构建本地项目），三个已发布包的项目根分别是 `src/omninexus`、`src/emotional_tts`、`src/dounai_input`。`python -m <pkg>` 的 `sys.path[0]` 是工作目录，不是 `src`，于是子进程立刻 `No module named <pkg>` 退出。

逐包手动复现（`venv\Scripts\python.exe -m ...`，工作目录为包根）：不带 `PYTHONPATH` 是 `No module named`；带上 `<包根>/src` 后三个包的 health 依次 200（`/v1/healthz`、`/api/health`、`/healthz`）。因此修在启动器侧，不动 `--no-install-project`：这样离线也能起，不需要 setuptools/wheel 的构建缓存（实测离线补装会报 `setuptools was not found in the cache`）。

`prepend_python_path` 只在 `<包根>/src` 存在时动手，flat 布局一律不加——包根进 `PYTHONPATH` 可能遮蔽标准库同名模块。manifest 自己声明了 `PYTHONPATH` 就拼在原值前面，不覆盖。

**二、所有非 active 实例都被分到 18101**

`single_instance_port` 调 `assign_ports(..., std::slice::from_ref(instance))`，而备用端口是「基数 + 序号」分配的：只传一个实例时序号恒为 0，每个非 active 实例都拿到 `18101`。第一个起来的占住端口，后面的绑不上就退出——`netstat` 当时还能看到 `127.0.0.1:18101` 的残留连接。

端口分配改为统一按 `all_instances(state)`（整张已登记实例表，含停用的）计算，单实例启动、`start_all` 与实例视图共用同一份结果。`start_all` 原来先过滤停用实例再分配，同样会让「列表显示的端口」与「真正使用的端口」不一致，一并改掉。

**三、死进程句柄让 `owns()` 永远为真**

watcher 只把状态改成 `exited`，`processes` 表里的 `WinProcess` 一直留着；`owns()` 是 `contains_key`，于是：

1. 快照 `pid` 已被 `mark_exited` 清成 null → 前端 `hasLiveProcess` 为假 → 只渲染「启动」；
2. 后端 `start_instance` 看到 `owns()` 为真 → 去探 health → 不通 → 回「进程还在运行，先点停止」。

两边结论相反，用户被卡死。改法：

- `WinProcess::is_alive()`：槽为空即已退出，槽还在时用 `try_wait` 兜住「刚退出、watcher 还没轮到」这段窗口。不清槽，退出原因仍由 watcher 统一记录。
- `owns()` 复核存活，死的就地摘掉；`stop()` 先看存活，不再对结束的进程报「停止失败」。
- `stop_all()` 额外收掉「判超时但进程还活着」的实例（它们不在 `running()` 里，但仍是启动器托管的进程）。
- 探测线程拿 watcher 槽做哨兵：进程退出后立刻返回，不再空跑满 90 秒预算，日志里也不再出现误导性的 `health=Unhealthy`。

**四、验证**

- 手动端到端：三个包分别用 `PYTHONPATH=<包根>/src` + 独立端口（18199/18200/18201）启动，health 全部 200；顺带确认退出码 1 的原文就是 `No module named <pkg>`。
- 新增 2 项单测：`standby_ports_are_unique_across_instances`（三个非 active 实例必须拿到三个不同端口，且 `all_instances` 覆盖全部登记实例）、`src_layout_is_added_to_pythonpath`（src 布局补路径、已有 `PYTHONPATH` 前置拼接、flat 布局不动）。
- 门禁：core 169 lib + 23 集成、app **71 lib**（+2）、`bundled_uv` 3、契约 109、`tsc`、`npm test` 26、`clippy --workspace --all-targets -D warnings` 全绿。

**未实测**：装上新包后点一次启动，三个模块应各自拿到 18101/18102/18103 并在预算内转 `running`；以及失败实例再点启动能直接重来、不再要求先停止。

## M1 返修：active 槽自动补位（2026-11-13 21:35）

启动问题修完后，检查真机状态发现 `installed.json` 的 `active` 是 `{}`：三个实例的 role 分别是 `input`/`llm`/`output`，verdict 全是 `A`，却一个 active 都没有。`assign_ports` 因此把它们全部分到 `18101-18103`，而组装层（Dounai 主程序）只认 8101、8102、8103——界面显示「运行中」，链路实际一条都连不上。

**一、为什么要在四个地方补，而不是只在安装处补**

「有实例、没有 active」不是可用的中间状态，但从四个入口都能造出来：

1. 在线目录安装 / 本地 zip 安装（`commit_install`）——装完本该上位；
2. 本地目录导入（`add_local_dir`）——dir 实例恒为 C 类，按契约不自动上位，但用户亲手挑的目录本身就是显式指名；
3. 卸载或解除登记——卸掉的若是 active，槽位空出来，同 role 剩下的实例还在备用端口；
4. 升级上来的老实例表——以上三条都不覆盖。

于是逻辑收成一个 core 方法 `InstalledState::fill_vacant_active(path, role, preferred, explicit)`，四个调用点都只负责「传什么」：安装传刚装的实例 ID + `explicit = false`，目录导入传 `explicit = true`，卸载传 `None`，启动时对三个 role 各扫一次。

**二、怎么选上位实例**

`preferred` 在册且角色相符就用它（用户刚装的就是要用的），否则按 `instance_id` 字典序取第一个——结果确定，不依赖 `BTreeMap` 之外的东西。槽已占用时直接返回 `None`，绝不抢已经在生效的那个。

**三、C 类的例外只开一道缝**

`switch_active(..., explicit=false)` 拒绝把 C 类设为 active。`fill_vacant_active` 沿用这个判断，只在 `explicit = true` 时放行。安装 B/C 类 zip 时用户的确认针对的是「安装」，不是「让它接管链路」，所以那条路径传 `false`；本地目录导入是用户亲手指定的一整个源码目录，与菜单里的「设为 active」等价，传 `true`。

**四、启动时自愈**

`heal_active_slots(state)` 在 `main.rs` 的 setup 里、`app.manage(state)` 之前跑一次，返回补上位的实例 ID，非空时往 stderr 打一行。这一步只写 `installed.json`，不碰任何进程。

**五、验证**

- core 新增 4 项：槽空时第一个实例上位；槽已占用时新实例不动槽位（且 `active` 仍指旧值）；`preferred` 不在册时按字典序回退、C 类在 `explicit=false` 下拒绝而 `true` 下放行；`shell`/`auxiliary` 永不设槽（`auxiliary` 以前会因 `role_key` 报错，现在按「不设槽」静默返回）。
- lib 169 → **173**。门禁其余项不变：core 23 集成、app lib 71、`bundled_uv` 3、契约 109、`tsc`、`npm test` 26、`clippy --workspace --all-targets -D warnings` 全绿。

**六、本机现状已就地修正**

`installed.json` 已补上三个槽（备份在 `state/installed.json.bak-preactive`）：

```json
{"input": "dounai/dounai-input@stable", "llm": "dounai/omninexus@stable", "output": "dounai/moodvoice@stable"}
```

**未实测**：装上新包后三个模块应落在 8101/8102/8103；以及卸载某个 active 实例后，同 role 的另一个实例自动接位。

## M8 下载中心与可配置源离线验收（2026-11-14）

M8 的 12 条验收要求真机逐条实测，离线断言只证明逻辑层。本节记已跑的离线结果与**未实测**项，两者不混。

**一、契约层**

新增 `contracts/sources-v1.schema.json`（源注册表 + 每源 `sequence` / `expires` / 域名白名单 / 独立 catalog 指纹），`contracts/examples/state/sources.json` 给出样例。`validate.mjs` 断言从 109 增到 **129**，新增的负例逐条对应 ROADMAP 验收 11：

- 源条目缺 `catalog_fingerprint` → 拒；
- 内置源 `deletable: true` → 拒；
- 社区源 `auto_update: true` → 拒；
- 源条目自述 `pubkey` → 拒（验签公钥只认本地注册表）；
- 空 `host_suffixes` → 拒；
- 公共后缀 `com` → 拒（白名单元素必须是完整域名）；
- 非 https 的 `base_url` → 拒；
- 撤销公钥后仍 `enabled: true` → 拒；
- 源 id 唯一，官方主源与镜像共享同一水位组；
- `allowlist` 只由已启用源的 host 后缀派生；
- 样例源表的公钥与 Rust 内嵌 K5 一致。

同时改了三处契约（详见 [ROADMAP.md](ROADMAP.md) M8「契约在 M8 期间改过三处」）：`catalog-v2` 的 `signature.signer_fp` 改必填、`key_id` 降级为展示字段；`source-v1` 的 instance 增 `source_id`、停用 `catalog_state.sequence` / `expires`。

**二、后端**

core 新增 2 项，都针对「源自己的钥匙」：

- `community_catalog_only_verifies_with_its_own_registered_key`：社区源用自己登记的公钥签的 catalog 能验过；换成另一把钥匙签的同内容 catalog 被拒。
- `catalog_cannot_smuggle_its_own_pubkey`：catalog 自带的 `signature.pubkey` 不被采信——源表里登记的才是判定依据。

app 侧改动：`catalog.rs` 的 `parse` 拒收 catalog 自述公钥、改为按源的 `expected_signer_fp` 验签；`validate()` 改用每源水位；`observe()` 只推进 `last_seen_version`；`adapters.rs` 按源注入公钥与白名单；`installed.rs` 的 instance 增 `source_id`。命令面删掉 `set_k5_pubkey`、`catalog_adapter` 与 `BUILTIN_CATALOG_*` 常量，`refresh_catalog` 改为按已启用源集合拉。

**三、前端**

「下载」是侧栏独立视图，不是模态。新增 `tests/download-view.test.ts` 11 项，钉住两条核心语义：

- 跨源同名 `module_id` 分成两组，不做隐式优选；
- 同一 `module_id@variant` 的多个版本并列，按版本从新到旧排，且不依赖传入顺序。

另有筛选的 6 项：按源、按 role、按关键词（名称 / 模块 ID / 变体 / 来源名）、只看可安装、叠加取交集。

`tests/bridge.test.ts` 同步改两处签名：`install_from_catalog` 从 `(moduleId, variant, confirm)` 改成 `(entryKey, confirm)`——安装目标由用户点选的那一行决定，界面不做隐式优选；`refresh_catalog` 从传 URL 改成无参数，新增 `catalog_cache` 只读会话缓存。

**四、删掉的三条绕过通道**

1. 设置页的会话级 catalog URL 输入框——能拉白名单外主机的目录；
2. `set_k5_pubkey` 命令——能在会话内换掉验签公钥；
3. `DOUNAI_CATALOG_*` 环境变量——绕过源注册表指定目录。

**门禁**

Rust：app lib 80、core lib 188、pack 6、sign_catalog 2、bundled_uv 3、fuzz_invariants 7、sign_real_manifests 2、signing_roundtrip 3、env_engine 3、lifecycle_engine 11、m2_engine 11，合计 **316** 通过；`live_source` 2 项与 `env_engine` 1 项按设计 ignored。前端 `npm test` **71** 项（8 个文件）。契约 **129** 项。`tsc --noEmit`、`vite build`、`clippy --workspace --all-targets -D warnings` 全绿。

**未实测（12 条真机验收全部未执行）**

以下都没有跑过真机，不能当作验收通过：

- 「下载」视图的切换、筛选保持与装包时主界面可操作；
- 添加源向导的完整指纹展示与确认框默认焦点（默认焦点在「取消」这条只写进了代码，没在界面上验过）；
- 用真源拉取、真机安装（真机安装在新界面上仍未跑过一次）；
- 撤销源后已装实例标「发布者已不再受信任」的显示；
- 禁用内置官方源后本地 zip 导入仍可用；
- 两个源同时启用时条目并列与各自标来源；
- 离线时远程请求数为零；
- 加源与撤销源流程对 `trust.json` 零写入（离线断言只覆盖逻辑，未在真机数据根上核过文件 mtime）；
- 社区源实例不自动更新。

真机验收按 [ROADMAP.md](ROADMAP.md) M8 的 12 条逐条走，走完再更新本节。

## Backlog 清偿与隐藏镜像源（2026-11-14 下午）

ROADMAP「Backlog：现网修复与签发流程欠账」的代码侧 1–4 与源侧第 5 条当天落地，本节记离线结果：

- **隐藏 `official-gh`**：`SourceRegistry::builtin` 只注册官方 SourceForge 源；`load` 剔除旧文件里内置的 GitHub 镜像条目（`HIDDEN_OFFICIAL_IDS`）。测试从「两个内置源」改为「一个内置源 + 旧文件剔除」。
- **签发预检**：`release.rs` 新增 `preflight_catalog`（`sources[].id` 均匀唯一、条目 `source_id` 必须在顶层 `sources`、id 限于允许集合），`sign_catalog` 的 `sign` 与 `verify` 都在签名/验签前跑预检；release lib 测试增 4 项。
- **逐源 last_error**：`SourceEntry` 增 `last_error` / `last_error_at`，刷新失败逐源记录、成功清除，全部失败也落盘；下载页源卡显示最近失败原因。契约断言 129 → **131**。
- **签发脚本**：`scripts/release-catalog.py` 端到端演练通过（拉现网 → sequence+1 → 7 天窗 → 签名 → 复验 → 打印 sftp 命令），产物未上传。
- **真源冒烟**：`live_source` 两个测试补逐条目 `source_id` 与内置源表一致的断言（仍默认 ignored）。

门禁：Rust app lib **84**、core lib **189**（合计与集成共 **325**）通过；前端 **71**；契约 **131**；`tsc` / `vite build` / `clippy -D warnings` / `check-upstreams` 全绿。真机 12 条验收状态不变。
