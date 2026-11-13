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
2. 内置 K5 公钥是真实值，但还没有已签发的 catalog，`refresh_catalog` 按 `unavailable` 返回，不降级放行。`set_k5_pubkey` 可在本次会话内替换公钥，界面入口在 M4。
3. `switch_active` 已按 `activation_steps` 执行停旧、起新、探测、提交与回滚。限制有两条：目标实例由外部进程提供时直接拒绝（验不了契约、也无法回滚）；目标已是 active 且在跑时只重新探测，不重启进程。
4. 卸载的 `remove_files` 会清包目录、venv 与日志，`data_dir` 按契约保留；`purge_local_data` 先停托管实例再删目录，外部实例停不掉，跳过并记进 `errors`。
5. `purge_local_data` 的 `state` 只重置 catalog 水位，保留实例表，与 v1 的清状态语义不同。
6. `list_module_states` 仍按 `module_id` 返回单行，多实例细节走 `list_instances`。

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
