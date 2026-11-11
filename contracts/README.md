# M0 契约冻结文件

启动器 v2 的角色、包格式与本地状态以本目录为单一真源。Rust 侧类型定义、跨仓一致性 CI 与发布工具都以这里的文件为准。修改任一文件后，先运行校验，再同步依赖这些文件的代码与样例。

## 文件

| 文件 | 用途 | 写入方 |
|---|---|---|
| `roles-v1.json` | 五值封闭枚举、规范端口、health 路径、契约探测路径 | 启动器仓，编译期内嵌 |
| `module-v2.schema.json` | zip 包与 dir 目录实例共用的 manifest | 发布者，`pack` 工具生成 |
| `catalog-v2.schema.json` | K5 签名的索引、版本单调性与新鲜度 | 官方发布流水线 |
| `source-v1.schema.json` | `state/installed.json` 的实例记录、来源与 active 槽 | 启动器 |
| `trust-v1.schema.json` | `state/trust.json` 的公钥信任列表 | 启动器 |
| `retention-v1.schema.json` | `state/retention.json` 的旧版保留策略 | 启动器 |
| `install-v1.schema.json` | `state/install.json` 的双根、安装模式与 uv 定位 | M5 安装器写，M1 夹具可写 |
| `deps-v1.schema.json` | `state/deps/<instance_key>/<sha256>.json` 的 generation 元数据 | 环境引擎 |
| `source-adapter-v1.md` | `SourceAdapter` trait 的接口与不变式 | 启动器仓 |
| `validate.mjs` | 零依赖契约校验器 | 启动器仓 |
| `check-upstreams.mjs` | 核对角色声明与五个仓源码是否一致 | 启动器仓 |
| `examples/*.v2.json` | 五个角色的 schema 校验样例 | 启动器仓 |

## 五个角色

| role | 规范端口 | health | active 上限 | 第三方 | 可降级 |
|---|---|---|---|---|---|
| `input` | 8101 | `/healthz` | 1 | 可 | 否 |
| `llm` | 8102 | `/v1/healthz` | 1 | 可 | 否 |
| `output` | 8103 | `/api/health` | 1 | 可 | 是，`text_only` 与 `text_fallback` |
| `shell` | 无 | 无 | 1 | 可，仅限显式激活 | 否 |
| `auxiliary` | 动态分配 | 可选 | 无 | 可 | 是 |

三条规则：

1. manifest 出现未列出的 `role` 时硬拒，并提示需要更高版本启动器。不得降级成 `auxiliary`。
2. 只有 active 实例占规范端口，其余并存实例使用 `18101` 起的备用端口段。组装层只认 8101、8102、8103，因此三上游零改动。
3. 新增与 `input`、`llm`、`output` 同级的数据面角色，需要同时改组装层的端口、事件转发与解析代码，本版不做。

## 第三方界面

`shell` 允许第三方模块填槽。呈现层只做本地 HTTP 客户端，不占规范端口。启动器不向界面注入密钥，也没有从界面调用管理命令的通道。`input` 与 `llm` 同样允许第三方，且接触用户输入与对话内容。

第三方界面受四条约束：

1. 不能自动成为 active。首次激活前单独确认一次，确认框默认焦点在“取消”。
2. 卡片与窗口常驻显示“第三方界面 · 发布者名称”，不显示 official 徽标。`category` 只能是 `optional`。
3. `required_contract` 为 `null`。启动器不做 HTTP 契约探测。`consumes` 只用于核对 manifest 的 `depends_on`。
4. `health` 为 `null`。切换后不设健康门禁，保留“切回上一个界面”入口供手动回退。

警告：激活第三方界面前，确认文案须写明“启动器无法验证界面内容”。不披露时，仿冒的配置页可能诱导用户输入密钥。

## 契约探测路径按上游实测填写

三个数据面角色的契约端点路径与返回结构并不相同，激活前按 `expect.shape` 分别断言。

| role | 探测路径 | 返回形状 | 最小断言 |
|---|---|---|---|
| `input` | `GET /v1/contract` | JSON Schema 文档 | 含 `x-contract-version`，不低于 1.1 |
| `llm` | `GET /v1/contract` | `contract_manifest` | 含 `service`、`service_version`、`contracts`；情绪契约版本 2；12 类标签齐全 |
| `output` | `GET /api/v1/contract` | `contract_document` | 含 `contract_version`、`contract_hash`、`tags`、`output`、`errors`；12 类标签齐全 |

## 校验

在项目根目录运行：

```bash
node contracts/validate.mjs
node contracts/check-upstreams.mjs
```

`validate.mjs` 零依赖，按 `module-v2.schema.json` 校验五份样例，并核对角色枚举与样例的端口、health、依赖。当前 84 项断言全部通过。

负例覆盖未知 role、路径穿越、缺 `pubkey` 与未验证实例自动更新。M1 新增的负例覆盖相对路径、哈希格式与未知 `kind`。

`check-upstreams.mjs` 从组装层与三个上游的源码解析 GET 路由和端口，逐项比对 `roles-v1.json`。默认读取工作区根目录下的五个仓；用 `--root` 指向别的检出布局，用 `--roles` 指向另一份角色契约做负例测试。源码缺失、路径不一致或正则零匹配都判失败，不把检查不适用当成通过。

[contracts.yml](../.github/workflows/contracts.yml) 在每次 push 与 PR 检出四个上游仓。job 依次运行两份校验器。ajv 接着编译六份状态 schema，并校验五份样例。workflow 不设跳过条件，也不使用 `continue-on-error`。

`validate.mjs` 不校验 `format` 关键字。draft 2020-12 把 `format` 当注释，所以 `written_at` 与 `generated_at` 另用 `pattern` 断言 UTC 时间格式，校验器会执行这条 `pattern`。CI 的 ajv 只编译状态 schema，不做状态文件的数据级校验；合规性由写端与断言共同保证。本地复核需装 `ajv-cli@5` 与 `ajv-formats@3`，命令见 workflow。

## 发布工具

`src-tauri/src/bin/pack.rs` 与 `sign_catalog.rs` 按本目录契约产出可安装的包与索引。在 `src-tauri/` 目录运行：

```bash
cargo run --bin pack -- keygen --out-private k1.priv --out-public k1.pub
cargo run --bin pack -- pack --dir <源目录> --key k1.priv --out module.zip
cargo run --bin pack -- verify --pubkey k1.pub --zip module.zip
cargo run --bin sign_catalog -- sign --key k5.priv --catalog catalog.json --out signed.json
cargo run --bin sign_catalog -- verify --pubkey k5.pub --catalog signed.json
```

`pack` 扫描源目录生成 `files`，把带 `pubkey` 的签名段写进 zip 内的 `manifest.json`，不改写源目录。打包后立即自检，输出 catalog 条目要用的 `sha256`、`size` 与 `publisher_fp`。`sign_catalog` 固定用 K5 签名，拒绝有效期超过 7 天的索引。

`canonical_payload` 剔除整个 `signature` 段，`key_id`、`signed_at` 与 `pubkey` 不在签名覆盖内。`pubkey` 不靠签名保护：能验通即证明签名者持有对应私钥，替换公钥后验签必失败。完整 schema 校验由 [contracts.yml](../.github/workflows/contracts.yml) 的 ajv 承担，工具只做结构性检查。

## 定稿时收紧的口径

以下口径按代码事实收紧或补全：

1. `version` 只接受 SemVer 三段，不含预发布与构建元数据。`0.3.3-agent` 是合法 SemVer 预发布串，光写“严格 SemVer”挡不住把变体塞进版本串。变体由 `variant` 单独声明。
2. `runtime` 对 `shell` 不必填，`entry.command` 用 `{install_root}`。`shell` 只启动本地 exe，其余角色必须含 `{venv_python}`。
3. `model_packs` 设为可选字段。字段沿用 v1，待后续细化。
4. v1 的 `reserved` 段未纳入 v2。默认禁用等语义改由 `category: reserved` 与验签结果裁定。
5. `active` 对象不含 `auxiliary` 键。该角色没有 active 上限，用槽位表达会产生假的单选约束。
6. `last_seen_version` 放在 `installed.json` 的 `catalog_state`，不新增状态文件。
7. 三个契约端点的返回结构不同，`required_contract.expect.shape` 分别为 `json_schema`、`contract_manifest`、`contract_document`。本版不做统一 schema 化。
8. 官方 `module_id` 采用 `dounai/` 加 v1 标识。namespace 只是命名，不表示信任；“官方”标签只由内置 K1 指纹验通授予。
9. `roles.json` 定名为 `roles-v1.json`，文件名带版本，便于后续不兼容升级。
10. 五份样例的 `signature` 为 `null`、`files` 为 `{}`。样例只用于 schema 校验，不可安装；`category` 表示目标发布形态，正式包须由 K1 签名后才生效。

## 变更规则

- `roles-v1.json` 增加或删除 `role` 时，同步改 `module-v2.schema.json` 的 `role` 枚举、`source-v1.schema.json` 的 `active` 键集合、`catalog-v2.schema.json` 的条目枚举与五份样例。
- 字段语义不兼容时升文件名版本号，不在原文件里叠加兼容分支。
- `validate.mjs` 的失败项不得通过放宽 schema 消除。允许例外时，在断言里写明理由。
- M1 新增 `install-v1.schema.json` 与 `deps-v1.schema.json`。`module-v2.schema.json` 只改 `runtime.deps` 与 `runtime.snapshot_sha256` 的 `description`，字段类型、必填与取值未变，不升文件名版本号。
- M2 把 `deps-v1` 的落盘路径改成内容寻址 generation，只改 `description`，字段与取值不变，不升文件名版本号。

警告：把 `unknown_role_policy` 改成降级为 `auxiliary`，第三方模块会绕过契约自检。不要改这个值。

说明：本版不读取 v1 manifest，不设兼容层；`permissions` 只作声明，启动器不做能力沙箱；目录实例固定按未验证处理，不承诺入口文件哈希，也不自动更新。

## M0 实施状态

M0 实施清单的四项工作已交付到本分支。

- 跨仓一致性 CI 与两个发布工具见 [contracts.yml](../.github/workflows/contracts.yml) 和“发布工具”一节。
- `launcher-core` 位于 `src-tauri/core`，是不依赖 Tauri 的独立 crate。签名与路径安全模块已迁入，`paths` 与 `AdminHost` 接口已冻结。CI 的 `core` job 单独跑无窗口单测。
- Tauri 固定为 `=2.12.5` 并启用 `unstable`。capability 维持最小集，升级检查清单见 [TAURI_UPGRADE.md](../docs/TAURI_UPGRADE.md)。

剩余一项属于 M5：K1、K5 两把密钥的生成与编译期内置。生成真实私钥并内置到发布构建前，按 M5 的发布流水线执行，不放在 M0。

以上工作全部限定在 Windows。Windows 专属进程代码放进 `process/win`，不写跨平台 trait 的空壳。

## M1 契约增量

M1 新增两份状态 schema，不改 M0 已冻结的约束。

| 状态文件 | 契约 | 写入方 |
|---|---|---|
| `state/install.json` | [install-v1.schema.json](install-v1.schema.json) | 安装器写一次，之后只读 |
| `state/deps/<instance_key>/<sha256>.json` | [deps-v1.schema.json](deps-v1.schema.json) | 环境引擎 |
| `state/deps/<instance_key>/<sha256>.lock` | 无 schema，内容是 uv 的 pinned snapshot | 环境引擎 |
| `state/runtime.json` | 无 schema，引擎内部状态 | 环境引擎 |

说明：`state/runtime.json` 记 `install_mode`、`python_install_dir`、`python_build_id`、`uv_version` 与每个实例的 `snapshot_sha256`，不进跨仓契约。锁文件的哈希放在这里，是为了让篡改锁文件时有一个独立比对点。这只防意外改动，不防本地恶意改写。

注意：`source-v1.schema.json` 的 `instance_id` 允许含 `/`，例如 `dounai/omninexus@stable@main`。引擎拼状态路径前先把标识编码成不含斜杠的 key。`install-v1` 的两个根不得相同，这条由 `Paths::new` 校验，schema 表达不了跨字段比较。

## M2 状态格式变更

锁与元数据从每个实例一份可变文件，改成按内容哈希命名的不可变 generation。改动只在 `state/` 内，`deps-v1` 的字段与取值不变。

| 项 | M1 | M2 |
|---|---|---|
| 锁路径 | `state/deps/<instance_key>.lock` | `state/deps/<instance_key>/<sha256>.lock` |
| 元数据路径 | `state/deps/<instance_key>.json` | `state/deps/<instance_key>/<sha256>.json` |
| venv 路径 | `instances/<instance_key>/venv`，原地 sync | `instances/<instance_key>/venvs/<generation>`，新建走 `.staging` 后原子 rename |
| 提交点 | 锁、元数据、venv、`runtime.json` 多次写 | 只有 `runtime.json` 一次原子写 |
| 崩溃后果 | 可能留下锁新、哈希旧，或环境新、状态旧 | 旧 generation 的锁与 venv 都还在，旧 `runtime.json` 照常可读 |

`runtime.json` 引用的 generation 必须先落盘，写入后永不改写。同一 generation 重复提交是幂等的，元数据保留首次生成时间。残留 generation 的清理由 M2 旧版保留策略统一处理，不在环境引擎里删。

`prepare_instance` 按实例的 `source_kind` 决定快照语义。zip 包必须比对签名 manifest 的 `runtime.snapshot_sha256`，缺字段或不符都硬拒；dir 允许重解析，哈希变化时把 `previous_sha256`、`current_sha256` 与观察时间记进 `runtime.json`，交 M4 提示重新确认。

`module-v2.schema.json` 只改了两处说明文字。`runtime.deps.kind` 由发布者声明，启动器不嗅探、不按优先级猜测；`runtime.snapshot_sha256` 是发布者预置的哈希，包签名后不可变，运行时生成的哈希另记在 `state/runtime.json`。
