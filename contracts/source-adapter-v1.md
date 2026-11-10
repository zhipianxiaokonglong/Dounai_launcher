# SourceAdapter trait 契约（M0 冻结）

`SourceAdapter` 把远端目录源、本地 zip 与本地目录统一成一条只读取数通路。启动器只通过这组方法取得 catalog 与包字节，安装、验签、信任判定与激活都不在适配器内完成。

## 接口

以下签名在 M0 冻结，M1 与 M2 按此编写 Rust 代码。

```rust
pub enum SourceKind {
    RemoteCatalog,  // GitHub、SourceForge 等带签名 catalog 的源
    LocalZip,       // 用户选中的单个 zip 文件
    LocalDir,       // 本地文件夹或 Git 仓库，不复制源码
}

pub struct PackageReceipt {
    pub source_kind: SourceKind,
    pub source_path: Option<PathBuf>,
    pub sha256: String,             // 启动器自算；本地来源只防传输损坏
    pub size: u64,
}

pub enum SourceError {
    Unavailable { source_id: String, reason: String },
    CatalogInvalid { reason: String },   // 含签名无效、序号倒退、已过期
    PackageInvalid { reason: String },   // 含哈希不符、超过解压上限
    LocalPathRejected { reason: String },
}

pub trait SourceAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> SourceKind;
    fn priority(&self) -> u32;
    fn fetch_catalog(&self) -> Result<Option<CatalogDocument>, SourceError>;
    fn fetch_package(&self, entry: &CatalogEntry, dest: &Path) -> Result<PackageReceipt, SourceError>;
}
```

`fetch_catalog` 对 `LocalZip` 与 `LocalDir` 返回 `Ok(None)`。启动器据此判定“无新鲜度锚”，把该实例标为不自动更新。

## 各来源的职责边界

| 来源 | catalog | 包字节 | 启动器侧后续动作 |
|---|---|---|---|
| `RemoteCatalog` | 拉取并用内置 K5 公钥验签 | 下载到 staging，校验 `sha256` 与 `size` | 包内 `pubkey` 验签、A/B/C/D 分级 |
| `LocalZip` | 无 | 读取用户选中的文件 | 同一套包内验签；可选期望签名者指纹 |
| `LocalDir` | 无 | 不复制，`{module_dir}` 指向源码目录 | 只读观察 `.git`，固定 C 类 |

## 不变式

1. `LocalDir` 适配器不得执行 `git pull`、`fetch`、`checkout` 或 `submodule update`。自动拉取会让远端代码未经确认就运行。
2. `LocalDir` 拒绝 `data_root`、`install_root` 本身及其祖先，拒绝启动器 exe 所在目录，拒绝指向启动器数据根的符号链接。
3. `fetch_package` 写盘前按 zip 元数据核对解压后总大小。超过上限返回 `PackageInvalid`，不落盘。
4. `LocalZip` 的 `sha256` 由启动器自己计算，只防传输损坏，不构成完整性保证。UI 必须写明这一点。
5. `RemoteCatalog` 的 catalog 未通过 K5 验签、序号倒退或已过期时返回 `CatalogInvalid`，不得回退到未验签的旧缓存继续自动更新。
6. 两源 catalog 按“签名有效、序号不倒退、时间不倒退”裁决，选出的结果写进 `installed.json` 的 `catalog_state`。
7. `origin` 可能含凭据，`fetch_package` 与适配器日志都不得写出该值。

警告：把 `LocalDir` 接上自动拉取，等于让远端代码绕过确认流程。这条不变式不得按便利性放宽。

注意：`fetch_catalog` 返回 `Ok(None)` 表示没有新鲜度锚，不表示来源不可用。调用方不得据此把实例标记为已验证。

## 错误暴露

`SourceError` 的每个变体都要给出具体原因。安装界面不得把这些错误统一显示成“安装失败”。用户需要知道是源不可达、catalog 过期、包哈希不符，还是本地路径被拒。

## 平台与版本

本契约只面向 Windows。适配器不提供跨平台 trait 的空壳，路径处理统一使用 `launcher-core::paths`。

`SourceKind` 增加新变体属于不兼容变更，需要另发 `source-adapter-v2.md`。第三方可以编写自己的 `SourceAdapter`，不能扩展 `SourceKind` 枚举，也不能新增 `role`。
