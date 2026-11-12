//! 可注入网络的双源适配器实现。
//!
//! 网络只通过 `HttpFetcher` trait 进入，便于离线测试；重定向逐跳校验由
//! `validate_download_url` 的域名后缀集承担，最终以 entry.sha256 为准。
//! LocalDir 不复制源码，也不执行任何 git 命令。

use std::fs;
use std::path::Path;

use crate::m2::catalog::{CatalogDocument, CatalogEntry};
use crate::m2::local::{local_zip_receipt, observe_source, validate_local_dir};
use crate::m2::package::{verify_zip, ZipExpectation};
use crate::m2::source::{
    validate_download_url, PackageReceipt, SourceAdapter, SourceError, SourceKind,
    GITHUB_HOST_SUFFIXES, SOURCEFORGE_HOST_SUFFIXES,
};
use crate::security::sha256_hex;

pub trait HttpFetcher: Send + Sync {
    fn get(&self, url: &str) -> Result<Vec<u8>, SourceError>;
}

pub struct RemoteCatalogAdapter<'a> {
    pub id: String,
    pub base_url: String,
    pub priority: u32,
    pub host_suffixes: &'static [&'static str],
    pub k5_pubkey_b64: String,
    pub http: &'a dyn HttpFetcher,
}

impl<'a> RemoteCatalogAdapter<'a> {
    pub fn github(
        id: &str,
        base_url: &str,
        priority: u32,
        k5_pubkey_b64: &str,
        http: &'a dyn HttpFetcher,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            priority,
            host_suffixes: GITHUB_HOST_SUFFIXES,
            k5_pubkey_b64: k5_pubkey_b64.into(),
            http,
        }
    }

    pub fn sourceforge(
        id: &str,
        base_url: &str,
        priority: u32,
        k5_pubkey_b64: &str,
        http: &'a dyn HttpFetcher,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            priority,
            host_suffixes: SOURCEFORGE_HOST_SUFFIXES,
            k5_pubkey_b64: k5_pubkey_b64.into(),
            http,
        }
    }

    fn package_url<'b>(&self, entry: &'b CatalogEntry) -> Result<&'b str, SourceError> {
        let source = entry
            .sources
            .iter()
            .find(|s| s.source_id == self.id)
            .or_else(|| entry.sources.first())
            .ok_or_else(|| SourceError::PackageInvalid {
                reason: format!("{} 没有下载地址", entry.module_id),
            })?;
        validate_download_url(&source.url, self.host_suffixes)?;
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
        validate_download_url(&url, self.host_suffixes).map_err(|e| {
            SourceError::CatalogInvalid {
                reason: e.reason().to_string(),
            }
        })?;
        let bytes = self.http.get(&url)?;
        let text = std::str::from_utf8(&bytes).map_err(|e| SourceError::CatalogInvalid {
            reason: format!("catalog 不是 UTF-8：{e}"),
        })?;
        let doc = CatalogDocument::parse(text, &self.k5_pubkey_b64)?;
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

    #[test]
    fn unavailable_source_yields_unavailable_not_a_fake_catalog() {
        let http = FakeHttp {
            responses: BTreeMap::new(),
        };
        let adapter = RemoteCatalogAdapter::github(
            "github",
            "https://github.com/dounai/releases",
            1,
            "AAAA",
            &http,
        );
        assert!(matches!(
            adapter.fetch_catalog(),
            Err(SourceError::Unavailable { .. })
        ));
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
