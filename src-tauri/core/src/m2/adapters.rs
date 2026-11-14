//! 可注入网络的远程 catalog 适配器与两个本地适配器。
//!
//! 网络只通过 `HttpFetcher` trait 进入，便于离线测试；重定向逐跳校验由
//! `validate_download_url` 的域名后缀集承担，最终以 entry.sha256 为准。
//! 每个远程适配器的公钥、指纹与白名单都来自 sources.json 的一条记录，
//! 没有全局 K5、也没有跨发布者的下载地址回退。
//! LocalDir 不复制源码，也不执行任何 git 命令。

use std::fs;
use std::path::Path;

use crate::m2::catalog::{CatalogDocument, CatalogEntry};
use crate::m2::local::{local_zip_receipt, observe_source, validate_local_dir};
use crate::m2::package::{verify_zip, ZipExpectation};
use crate::m2::source::{
    validate_download_url, PackageReceipt, SourceAdapter, SourceError, SourceKind,
};
use crate::m2::sources::SourceEntry;
use crate::security::sha256_hex;

pub trait HttpFetcher: Send + Sync {
    fn get(&self, url: &str) -> Result<Vec<u8>, SourceError>;
}

pub struct RemoteCatalogAdapter<'a> {
    pub id: String,
    /// 同一水位组内的源 id（官方主源 + 镜像），包下载地址只在组内挑。
    pub group_ids: Vec<String>,
    pub base_url: String,
    pub priority: u32,
    pub host_suffixes: Vec<String>,
    pub catalog_pubkey: String,
    pub signer_fp: String,
    pub http: &'a dyn HttpFetcher,
}

impl<'a> RemoteCatalogAdapter<'a> {
    pub fn from_source(
        source: &SourceEntry,
        group_ids: Vec<String>,
        http: &'a dyn HttpFetcher,
    ) -> Self {
        Self {
            id: source.id.clone(),
            group_ids,
            base_url: source.base_url.clone(),
            priority: source.priority.unwrap_or(1),
            host_suffixes: source.host_suffixes.clone(),
            catalog_pubkey: source.catalog_pubkey.clone(),
            signer_fp: source.catalog_fingerprint.clone(),
            http,
        }
    }

    fn suffixes(&self) -> Vec<&str> {
        self.host_suffixes.iter().map(String::as_str).collect()
    }

    /// 只认可本组源的下载地址。删掉 `sources.first()` 回退：跨源同名时
    /// 那条回退会让官方条目悄悄指向社区源的包，界面显示的和实际装的不是一回事。
    fn package_url<'b>(&self, entry: &'b CatalogEntry) -> Result<&'b str, SourceError> {
        let source = entry
            .sources
            .iter()
            .find(|s| self.group_ids.contains(&s.source_id))
            .ok_or_else(|| SourceError::PackageInvalid {
                reason: format!(
                    "{} 没有属于源 {} 的下载地址，不跨发布者回落",
                    entry.module_id, self.id
                ),
            })?;
        validate_download_url(&source.url, &self.suffixes())?;
        Ok(&source.url)
    }
}

impl SourceAdapter for RemoteCatalogAdapter<'_> {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> SourceKind {
        SourceKind::RemoteCatalog
    }
    fn priority(&self) -> u32 {
        self.priority
    }

    fn fetch_catalog(&self) -> Result<Option<CatalogDocument>, SourceError> {
        let url = format!("{}/catalog.json", self.base_url.trim_end_matches('/'));
        validate_download_url(&url, &self.suffixes()).map_err(|e| {
            SourceError::CatalogInvalid {
                reason: e.reason().to_string(),
            }
        })?;
        let bytes = self.http.get(&url)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| SourceError::CatalogInvalid {
            reason: format!("catalog 不是 UTF-8：{e}"),
        })?;
        let doc = CatalogDocument::parse(text, &self.catalog_pubkey, &self.signer_fp)?;
        Ok(Some(doc))
    }

    fn fetch_package(
        &self,
        entry: &CatalogEntry,
        dest: &Path,
    ) -> Result<PackageReceipt, SourceError> {
        let url = self.package_url(entry)?;
        let bytes = self.http.get(url)?;
        // 写盘前完成整包哈希、大小与解压总大小校验；不通过不落盘。
        let expectation = ZipExpectation {
            sha256: Some(&entry.sha256),
            size: Some(entry.size),
            ..Default::default()
        };
        verify_zip(&bytes, &expectation).map_err(|e| SourceError::PackageInvalid {
            reason: e.to_string(),
        })?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| SourceError::PackageInvalid {
                reason: format!("创建下载目录失败：{e}"),
            })?;
        }
        fs::write(dest, &bytes).map_err(|e| SourceError::PackageInvalid {
            reason: format!("写下载文件失败：{e}"),
        })?;
        Ok(PackageReceipt {
            source_kind: SourceKind::RemoteCatalog,
            source_path: None,
            sha256: format!("sha256:{}", sha256_hex(&bytes)),
            size: bytes.len() as u64,
        })
    }
}

pub struct LocalZipAdapter {
    pub path: std::path::PathBuf,
}

impl SourceAdapter for LocalZipAdapter {
    fn id(&self) -> &str {
        "local-zip"
    }
    fn kind(&self) -> SourceKind {
        SourceKind::LocalZip
    }
    fn priority(&self) -> u32 {
        0
    }
    fn fetch_catalog(&self) -> Result<Option<CatalogDocument>, SourceError> {
        // 无新鲜度锚，不自动更新；Ok(None) 不表示来源不可用。
        Ok(None)
    }
    fn fetch_package(
        &self,
        _entry: &CatalogEntry,
        _dest: &Path,
    ) -> Result<PackageReceipt, SourceError> {
        local_zip_receipt(&self.path).map_err(|e| SourceError::PackageInvalid {
            reason: e.to_string(),
        })
    }
}

pub struct LocalDirAdapter {
    pub path: std::path::PathBuf,
    pub install_root: std::path::PathBuf,
    pub data_root: std::path::PathBuf,
    pub exe_dir: std::path::PathBuf,
}

impl SourceAdapter for LocalDirAdapter {
    fn id(&self) -> &str {
        "local-dir"
    }
    fn kind(&self) -> SourceKind {
        SourceKind::LocalDir
    }
    fn priority(&self) -> u32 {
        0
    }
    fn fetch_catalog(&self) -> Result<Option<CatalogDocument>, SourceError> {
        Ok(None)
    }
    fn fetch_package(
        &self,
        _entry: &CatalogEntry,
        _dest: &Path,
    ) -> Result<PackageReceipt, SourceError> {
        validate_local_dir(
            &self.path,
            &self.install_root,
            &self.data_root,
            &self.exe_dir,
        )
        .map_err(|e| SourceError::LocalPathRejected {
            reason: e.to_string(),
        })?;
        // 不复制源码：receipt 只登记路径与只读快照。
        let observed = observe_source(&self.path, &crate::time::utc_now()).map_err(|e| {
            SourceError::LocalPathRejected {
                reason: e.to_string(),
            }
        })?;
        Ok(PackageReceipt {
            source_kind: SourceKind::LocalDir,
            source_path: Some(self.path.clone()),
            sha256: observed.fingerprint,
            size: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m2::sources::SourceRegistry;
    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use std::collections::BTreeMap;

    struct FakeHttp {
        responses: BTreeMap<String, Result<Vec<u8>, String>>,
    }
    impl HttpFetcher for FakeHttp {
        fn get(&self, url: &str) -> Result<Vec<u8>, SourceError> {
            match self.responses.get(url) {
                Some(Ok(bytes)) => Ok(bytes.clone()),
                Some(Err(reason)) => Err(SourceError::Unavailable {
                    source_id: "fake".into(),
                    reason: reason.clone(),
                }),
                None => Err(SourceError::Unavailable {
                    source_id: "fake".into(),
                    reason: format!("未配置响应：{url}"),
                }),
            }
        }
    }

    fn official(index: usize) -> SourceEntry {
        let sk = SigningKey::from_bytes(&[5; 32]);
        let pk = base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let fp = crate::m2::fingerprint(&pk).unwrap();
        SourceRegistry::builtin(&pk, &fp).unwrap().sources[index].clone()
    }

    fn entry() -> CatalogEntry {
        CatalogEntry {
            module_id: "dounai/demo".into(),
            variant: "stable".into(),
            version: "1.0.0".into(),
            role: crate::manifest::Role::Llm,
            name: "Demo".into(),
            publisher_fp: format!("sha256:{}", "a".repeat(64)),
            sha256: format!("sha256:{}", "a".repeat(64)),
            size: 1,
            sources: vec![crate::m2::catalog::EntrySource {
                source_id: "community-other".into(),
                url: "https://example.org/a.zip".into(),
            }],
            signature_required: true,
            expires: "2026-01-08T00:00:00Z".into(),
            expected_signer_fp: None,
            min_version: None,
            min_launcher: None,
            revoked: vec![],
        }
    }

    #[test]
    fn unavailable_source_yields_unavailable_not_a_fake_catalog() {
        let http = FakeHttp {
            responses: BTreeMap::new(),
        };
        let source = official(0);
        let adapter = RemoteCatalogAdapter::from_source(&source, vec![source.id.clone()], &http);
        assert!(matches!(
            adapter.fetch_catalog(),
            Err(SourceError::Unavailable { .. })
        ));
    }

    /// 跨源同名时不得拿第一条下载地址充数：那会把 A 源的条目装成 B 源的包。
    #[test]
    fn package_url_never_falls_back_to_another_publisher() {
        let http = FakeHttp {
            responses: BTreeMap::new(),
        };
        let source = official(0);
        let adapter = RemoteCatalogAdapter::from_source(&source, vec![source.id.clone()], &http);
        let error = adapter.package_url(&entry()).unwrap_err();
        assert!(error.reason().contains("不跨发布者回落"));
    }

    #[test]
    fn official_mirror_shares_the_download_group() {
        let http = FakeHttp {
            responses: BTreeMap::new(),
        };
        let source = official(1);
        let adapter = RemoteCatalogAdapter::from_source(
            &source,
            vec!["official-sf".into(), "official-gh".into()],
            &http,
        );
        let mut entry = entry();
        entry.sources[0].source_id = "official-gh".into();
        entry.sources[0].url = "https://raw.githubusercontent.com/x/a.zip".into();
        // githubusercontent.com 在官方 GitHub 源的白名单内，URL 本身通过校验。
        assert!(adapter.package_url(&entry).is_ok());
    }

    #[test]
    fn local_adapters_have_no_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let zip_adapter = LocalZipAdapter {
            path: dir.path().join("x.zip"),
        };
        assert!(zip_adapter.fetch_catalog().unwrap().is_none());
        let dir_adapter = LocalDirAdapter {
            path: dir.path().to_path_buf(),
            install_root: dir.path().join("app"),
            data_root: dir.path().join("data"),
            exe_dir: dir.path().join("app"),
        };
        assert!(dir_adapter.fetch_catalog().unwrap().is_none());
    }
}
