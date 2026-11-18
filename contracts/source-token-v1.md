# 源邀请码契约（source-token-v1）

源邀请码把“catalog 地址 + catalog 签名公钥”压成一行可复制的字符串。发布方只需公布这一行，用户粘贴后由启动器解析、算指纹、派生白名单，再按既有确认流程显式信任。邀请码不是凭证，也不降低信任门槛：邀请码只是把用户原本要填的两个字段打包，避免复制粘贴出错。

## 格式

```text
dnsrc1:<payload>.<check>
```

- `dnsrc1`：版本前缀。换格式时改前缀，不叠加兼容分支。
- `payload`：UTF-8 JSON 标准 base64url（无 `=` padding，字母表 `A-Za-z0-9-_`）。
- `check`：`payload` 字符串本身 UTF-8 字节的 SHA-256 取前 8 字节，base64url（定长 11 字符）。

`payload` 解码后的 JSON 字段封闭为四个：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `v` | 整数 | 是 | 载荷版本，本版固定为 `1` |
| `u` | 字符串 | 是 | catalog 基址，即 `catalog.json` 所在目录，不带尾斜杠 |
| `k` | 字符串 | 是 | Ed25519 公钥原始 32 字节的标准 base64，与 `sources.json` 的 `catalog_pubkey` 同格式 |
| `n` | 字符串 | 否 | 名称提示，空白视为缺省 |

**有意不设的字段**：指纹（`fingerprint`/`signer_fp` 等）与自定义白名单（`h`/`host_suffixes` 等）。启动器解析时丢弃这些未知键，调用方拿不到指纹与白名单。启动器从 `k` 现算指纹，从 `u` 派生白名单，与手动填表走同一条路径。

## 解析规则

1. 去掉首尾空白，总长不得超过 8192 字符。
2. 必须以 `dnsrc1:` 开头；按最后一个 `.` 分成 `payload` 与 `check`。
3. 重算 `checksum(payload)` 与 `check` 逐字符比对，不等即拒。
4. base64url 解码 `payload`，按 UTF-8 解析 JSON，字段不符即拒。
5. `v` 不等于 `1` 即拒；`u` 去空白与尾斜杠后不得为空；`k` 必须是 32 字节合法 base64。
6. 返回仅有 `base_url`、`catalog_pubkey`、`name` 三个字段的结果。

校验位只防粘贴截断与手抖，**不是信任根**。攻击者同样能造出匹配的 `check`，因此界面必须原样展示从 `k` 现算的完整指纹与从 `u` 派生的白名单，让用户带外核对后显式确认；确认框默认焦点在取消。确认前不写 `sources.json`，任何时候都不写 `trust.json`。

## 与源注册表的衔接

`inspect_source_token` 解析后调用与 `inspect_source` 相同的 `build_source_preview`：

- 指纹从公钥现算，与 `sources-v1.schema.json` 的 `catalog_fingerprint` 同义。
- 白名单从地址派生；回环地址（`127.0.0.0/8`、`localhost`、`::1`）允许 http，其余仍强制 https，见 `catalog-v2.schema.json` 与 `sources-v1.schema.json` 的 URL pattern。
- 邀请码里的名称只作展示提示，不参与 id 派生；id 仍按主机取，回环地址固定为 `localtest`。

确认后写入的 `SourceEntry` 与手动添加完全相同：`kind=community`、`auto_update=false`、`deletable=true`。邀请码不引入新的状态文件，也不改 `sources-v1` 的字段。

## 真源与工具

`src-tauri/core/src/m2/source_token.rs` 的 `issue` 与 `parse` 是编解码的唯一真源。命令行工具 `src-tauri/src/bin/source_token.rs` 直接调用同一份代码：

```bash
cargo run --bin source_token -- issue --url http://127.0.0.1:8765/releases --key-file keys/K5.json --name "本地测试源"
cargo run --bin source_token -- parse --token 'dnsrc1:...'
```

工具与运行时共用编解码代码，避免工具生成的邀请码在运行时解析不了。工具只接触公钥、不接触私钥、不写状态文件。

## 变更规则

- 载荷字段增减或语义不兼容时改前缀（如 `dnsrc2`），不复用 `dnsrc1`。
- `check` 算法变更等同格式不兼容，必须改前缀。
- 本契约在正式分发前不冻结；改完必须同步 `source_token.rs`、`source_token` 工具、本文件与 `validate.mjs` 的断言计数。
