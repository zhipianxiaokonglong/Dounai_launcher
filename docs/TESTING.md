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
5. 本机主 workspace 的 Rust 构建。本机阿里云镜像缺 `async-executor 1.14.1`，`src-tauri/` 下的 `cargo test -p launcher-core` 连 `--offline` 都解析失败。Rust 验证改用 `.internal/core-check` 镜像 crate；CI 用 crates.io，不受影响。

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
