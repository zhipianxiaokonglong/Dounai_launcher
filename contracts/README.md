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
| `source-adapter-v1.md` | `SourceAdapter` trait 的接口与不变式 | 启动器仓 |
| `validate.mjs` | 零依赖校验器 | 启动器仓 |
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
```

校验器零依赖，按 `module-v2.schema.json` 校验五份样例。校验器还核对角色枚举与样例的端口、health、依赖。负例覆盖未知 role、路径穿越、缺 `pubkey` 与未验证实例自动更新。当前 66 项断言全部通过。

CI 应再用完整 JSON Schema 校验器复核：

```bash
npx --yes ajv-cli@5 validate --spec=draft2020 --strict=false \
  -s contracts/module-v2.schema.json -d "contracts/examples/*.v2.json"
```

注意：`validate.mjs` 不校验 `format: date-time`。CI 使用 ajv 时需装 `ajv-formats` 并开启格式校验，否则时间字段只按字符串检查。

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

警告：把 `unknown_role_policy` 改成降级为 `auxiliary`，第三方模块会绕过契约自检。不要改这个值。

说明：本版不读取 v1 manifest，不设兼容层；`permissions` 只作声明，启动器不做能力沙箱；目录实例固定按未验证处理，不承诺入口文件哈希，也不自动更新。

## M0 尚未完成的工作

本目录交付的是契约文件，M0 还有以下实施项：

- `launcher-core` 抽取与 `admin-host` 抽象；
- 启用 Tauri `unstable` 并固定小版本，capability 按最小授权记录升级检查清单；
- 跨仓一致性 CI job，禁止条件不满足时静默跳过；
- `pack` 与 `sign-catalog` 两个命令行工具；
- K1、K5 两把密钥的生成与编译期内置，属于 M5 执行项。

以上工作全部限定在 Windows。Windows 专属进程代码放进 `process/win`，不写跨平台 trait 的空壳。
