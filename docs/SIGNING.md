# 模块签名方法

签名放进 zip 包内 `manifest.json` 的 `signature` 段。启动器先用包内公钥验签，再拿公钥指纹查信任表。字段定义见 [module-v2.schema.json](../contracts/module-v2.schema.json)，行为约束见 [DESIGN.md](DESIGN.md) 7.2 与 7.3。

## 1. 签名对象

签名覆盖剔除 `signature` 后的整份 manifest。

`launcher_core::signature::canonical_payload` 删掉 `signature` 段，再按 key 升序递归重排并序列化。`files` 清单在覆盖内，清单里每个 SHA-256 又覆盖文件的实际字节。改启动命令、改权限、改依赖路径、增删文件都会让验签失败。

字段书写顺序与空白不影响签名。`pubkey`、`key_id`、`signed_at`、`value` 都不在覆盖内。

发布工具与运行时共用 `launcher_core::signature`，两边不会产生不同的签名原文。`src-tauri/tests/signing_roundtrip.rs` 把工具打出的包送进 M2 验签与信任判定，锁住这条约定。

`algo` 的契约值已改为 `Ed25519`。签名输入是 `canonical_payload` 的字节，Ed25519 内部使用 SHA-512。SHA-256 只用于逐文件哈希与公钥指纹，不属于签名算法。旧字面量 `SHA-256 + Ed25519` 已被 `validate.mjs` 的负例断言拒收。

首次对外发布前没有已分发的包或 catalog，这次纠错没有迁移负担。发布后再改算法标识必须升 schema 版本，不能在原文件里叠加兼容分支。

## 2. 设计取舍

四条取舍决定现在的形态。

1. 签名与内容同处一份 manifest。单独的 `.sig` 文件会和包内容脱钩，分发时容易漏带。
2. 包内自带 `pubkey`。验签只返回布尔值，带公钥才能分清“包完整但公钥未登记”与“包被篡改”。
3. 判定用指纹，不用 `key_id`。指纹由公钥字节算出，包无法自述。`key_id` 只作展示。
4. K1 与 K5 分离。共用一把钥匙时，K1 泄露方可以签一份不含 `revoked` 的 catalog。

私钥格式选了 32 字节 seed 的 base64，没有 PKCS#8 封装，也没有口令加密。格式简单，代价是文件本身必须靠目录位置与 ACL 保护。

`signature.pubkey` 不靠签名保护。替换公钥后签名必然验不过，因此不需要把公钥放进覆盖内容。

## 3. 密钥体系

K1 签模块包，K5 签 catalog。两把钥匙不得混用。

| 项 | K1 | K5 |
|---|---|---|
| 签名对象 | 包内 `manifest.json` | 两源分发的 `catalog.json` |
| 公钥来源 | 包内 `signature.pubkey` | 启动器内置 `BUILTIN_K5_PUBKEY_B64` |
| 内置值 | 公钥与指纹，登记在 `state_v2.rs` | 公钥与指纹，登记在 `state_v2.rs` |
| 新鲜度 | 靠 catalog 的版本单调与 `expires` | `expires` 上限 7 天 |
| 泄露处置 | K5 签 catalog，把 K1 指纹写进 `revoked` | 带外公告加强制升级 exe |

指纹是公钥原始 32 字节的 SHA-256，小写十六进制，带 `sha256:` 前缀。私钥是 32 字节 Ed25519 seed 的 base64。

包内禁止出现 `publisher_fp`、`official`、`instance_id` 等启动器侧状态字段。schema 的 `not` 分支会整体拒绝这类 manifest。

## 4. 生成与保管

K1 与 K5 都在 2026-11-13 生成，公钥登记文件是 [keys/K1.json](../keys/K1.json) 与 [keys/K5.json](../keys/K5.json)。两把钥匙放在不同目录，互不派生。生成规程如下。

1. 在仓库外创建密钥目录，K1 与 K5 分开。
2. 运行 `cargo run --bin pack -- keygen --out-private <私钥> --out-public <公钥> --key-id <K1 或 K5> --meta keys/<K1 或 K5>.json`。
3. 用 `icacls` 移除私钥文件的继承权限，只授予当前用户完全控制。
4. 把 `--meta` 产出的登记文件放进仓库 `keys/`。
5. 把公钥与指纹写进 `state_v2.rs` 中对应钥匙的两个常量。
6. 设置 `DOUNAI_K1_KEY` 与 `DOUNAI_K5_KEY` 指向私钥，运行 `cargo test --test signing_roundtrip`。

`keygen` 用系统随机源生成 seed。Windows 走 `BCryptGenRandom`，其他平台读 `/dev/urandom`。`--meta` 只写公钥、指纹、用途与生成时间，不含私钥。

注意：`keygen` 遇到已存在的目标文件会直接报错。覆盖旧私钥会让此前签出的包失去可追溯的签发者。

警告：私钥入库或写进 CI 变量后，任何能读到仓库的人都能签出 A 类包。私钥保存在仓库外的 `Dounai_launcher-keys/K1-2026-11-13/` 与 `Dounai_launcher-keys/K5-2026-11-13/` 两个目录，仓库的 `.gitignore` 另有一层 `*.key` 与 `priv*` 兜底。

警告：持有这把私钥的人就是官方发布者。项目移交或持有人变化时，重新生成密钥并走第 7 节轮换，不要转交原私钥。

## 5. 打包、签名与分发

`pack` 从源目录生成 `files` 清单并签名，不改写源目录。

```bash
cargo run --bin pack -- pack --dir <源目录> --key <K1 私钥> --out <zip> --key-id K1
cargo run --bin pack -- verify --pubkey <K1 公钥文件> --zip <zip>
```

`pack` 结束时自检一次，输出 catalog 条目要用的 `sha256`、`size` 与 `publisher_fp`。自检会验签并逐文件比对哈希，清单与包内文件不一致时报错。

### 5.1 发布到 SourceForge

模块包由发布方生成，上传到 SourceForge 的 `dounai-releases` 项目：

`https://sourceforge.net/projects/dounai-releases/files/`

1. 用 K1 打包并自检，记下 `sha256`、`size` 与 `publisher_fp`。
2. 把 zip 上传到项目 Files。目录按 `modules/<namespace>/<name>/<version>/` 分层，文件名不带路径分隔符。
3. 生成 catalog，条目 `sources` 指向 SourceForge 下载地址，`sha256` 与 `size` 用第 1 步的值。
4. 用 K5 签 catalog，`expires` 不超过 7 天，再用 K5 公钥自检一次。
5. 上传 `catalog.json`，回读一次核对 `sha256`。

上传通道是 SFTP 与 `rsync`/`scp`。2026-11-13 已把本机 SSH 公钥登记到 SourceForge，实测 `tonyjh07@frs.sourceforge.net` 认证通过，`/home/frs/project/dounai-releases/` 可上传、列出与删除。上传仍未脚本化，发布时逐次执行。K1 私钥、K5 私钥与上传凭据分开保管，任一泄露都按密钥泄露流程处理。

启动器侧的顺序固定，见 [DESIGN.md](DESIGN.md) 7.3。

1. 核对整包 `sha256` 与字节数。
2. 读取包内 `manifest.json`，不落盘。
3. 用 `signature.pubkey` 验签。验不过直接判 D 类。
4. 算指纹并查信任表，得到 A、B、C 或 D 类结果。
5. 解压到 staging，逐文件核 `files` 哈希。
6. 提交事务，启动前再核一次入口文件哈希。

A 类表示公钥已登记且启用。B 类需要用户确认指纹。C 类是未签名包的单次确认。D 类硬拒，不提供确认入口。

## 6. 轮换与撤销

两条轮换路径都必须配带外公告与新版启动器。catalog 的 `revoked` 不能单独撤销密钥。

K1 泄露时按以下顺序处理。

1. 用 K5 签一份新 catalog，把旧 K1 指纹写进 `revoked`。
2. 生成新 K1，替换 `state_v2.rs` 的两个常量。
3. 用新 K1 重签全部在分发包。
4. 发布新版本启动器，并在仓库外公告旧指纹。

K5 泄露时按以下顺序处理。

1. 生成新 K5，替换 `state_v2.rs` 的指纹与公钥常量。
2. 用新 K5 重签 catalog，`expires` 不超过 7 天。
3. 发布新版本启动器并强制升级。
4. 在仓库外公告泄露窗口。

警告：只换内置公钥而不发新版 exe 时，旧版启动器仍会信任被撤销的钥匙。旧版没有读取远端禁用列表的通道。

## 7. 当前状态

K1 与 K5 都已生成并内置。模块包托管在 SourceForge 的 `dounai-releases` 项目，官方 catalog 尚未签发。

| 项 | 状态 |
|---|---|
| K1 私钥 | 已生成，仓库外，ACL 仅限当前用户 |
| K1 公钥与指纹 | 已登记在 `keys/K1.json` 与 `state_v2.rs` |
| K1 签出的包 | 离线验签通过，M2 判定为 A 类 |
| K5 私钥 | 已生成，仓库外，ACL 仅限当前用户 |
| K5 公钥与指纹 | 已登记在 `keys/K5.json` 与 `state_v2.rs` |
| K5 签出的 catalog | 离线验签通过，篡改条目被硬拒 |
| SourceForge 项目 | 已创建，包尚未上传 |
| SourceForge 上传通道 | 已实测，SSH 认证与目录写入通过 |
| 远程 catalog 分发 | 未启用，尚无签名 catalog |
| 真机安装 | 未实测 |

没有已签发的 catalog 时，`refresh_catalog` 返回 `unavailable`。这是 fail-closed，不是故障。K5 验签路径本身已经可用，缺的是 SourceForge 上的索引文件。

启用远程分发前要完成四件事。

1. 打包并上传首个模块包，记录 `sha256`、`size` 与 `publisher_fp`。
2. 生成 catalog，`sources` 指向 SourceForge，用 `sign_catalog` 以 K5 签名。
3. 上传 `catalog.json`，回读核对。
4. 真机验证拉取、验签、下包与安装。

说明：[ROADMAP.md](ROADMAP.md) 规定离线断言不计真机验收通过。上表的离线结果不改写 M3 与 M5 的验收状态。

## 8. 已知限制

`signed_at` 由发布机填写，包内签名不证明时间可信。新鲜度只由 catalog 的 `sequence`、`expires` 与 `revoked` 承担。

私钥没有口令或硬件密钥保护。文件一旦被复制，持有者即可签发官方包。

上传还没有脚本化的校验回读。zip 与 `catalog.json` 的哈希靠人工比对，误传只能靠安装时的哈希校验拦住，流程本身不阻断。

首次本地导入时，catalog 与已装实例这两道防线都还没有锚点。删掉签名段的官方包可能被当成 C 类确认安装。披露见 [SECURITY.md](SECURITY.md)。

目录实例的签名段只作展示。源码目录随时可变，完整性按源码工作区处理，不参与信任判定。
