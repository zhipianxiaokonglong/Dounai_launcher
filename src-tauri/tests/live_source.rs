//! 真实分发源冒烟测试（默认跳过，需出网）。
//!
//! 为什么要有：catalog 与包都托管在 SourceForge，而 SF 的下载域会 302 到
//! 镜像主机。这条链路**只能靠真网络验证**——单测里的 FakeHttp 不经过
//! TLS、302 与镜像域名，跑通也不代表真源可用。
//!
//! 跑法：
//! ```text
//! cargo test --test live_source -- --ignored --nocapture
//! ```
//! 不放进 CI：CI 不该依赖第三方站点可用性，也不该把网络波动算成回归。

use std::path::Path;

use dounai_launcher::commands_v2::BUILTIN_CATALOG_BASE;
use dounai_launcher::download::{download, DownloadOptions};
use dounai_launcher::http_client::ReqwestClient;
use dounai_launcher::m2_http::ReqwestCatalogFetcher;
use dounai_launcher::state_v2::{BUILTIN_K5_FINGERPRINT, BUILTIN_K5_PUBKEY_B64};
use launcher_core::m2::adapters::RemoteCatalogAdapter;
use launcher_core::m2::catalog::CatalogDocument;
use launcher_core::m2::source::SourceAdapter;

/// 内置 K5 公钥必须能把线上 catalog 验通——验不过是发版事故，不是测试问题。
#[test]
#[ignore = "需要出网：访问 SourceForge"]
fn builtin_catalog_verifies_with_builtin_k5() {
    let fetcher = ReqwestCatalogFetcher::new().unwrap();
    let adapter = RemoteCatalogAdapter::sourceforge(
        "sourceforge",
        BUILTIN_CATALOG_BASE,
        1,
        BUILTIN_K5_PUBKEY_B64,
        &fetcher,
    );
    let doc = adapter
        .fetch_catalog()
        .expect("线上 catalog 应可取到并验签通过")
        .expect("sourceforge 应返回目录");
    assert!(!doc.modules.is_empty(), "目录不该是空的");
    assert_eq!(doc.signature.key_id, "K5");
    // 指纹是界面展示与实际判定共用的同一个值。
    assert_eq!(
        launcher_core::m2::fingerprint(BUILTIN_K5_PUBKEY_B64).unwrap(),
        BUILTIN_K5_FINGERPRINT
    );
    for entry in &doc.modules {
        assert!(
            entry.sha256.starts_with("sha256:"),
            "{} 的哈希没有前缀：{}",
            entry.module_id,
            entry.sha256
        );
        assert!(entry.size > 0, "{} 没写包大小", entry.module_id);
    }
    println!(
        "目录可用：{} 个模块，expires={}",
        doc.modules.len(),
        doc.expires
    );
    for entry in &doc.modules {
        println!(
            "  {}@{} v{} sha256={} size={}",
            entry.module_id, entry.variant, entry.version, entry.sha256, entry.size
        );
    }
}

/// 目录里第一个包真的能下下来，且哈希与目录声明一致。
#[test]
#[ignore = "需要出网：从 SourceForge 下载模块包"]
fn first_catalog_package_downloads_and_matches_hash() {
    let fetcher = ReqwestCatalogFetcher::new().unwrap();
    let adapter = RemoteCatalogAdapter::sourceforge(
        "sourceforge",
        BUILTIN_CATALOG_BASE,
        1,
        BUILTIN_K5_PUBKEY_B64,
        &fetcher,
    );
    let doc: CatalogDocument = adapter.fetch_catalog().unwrap().unwrap();
    let entry = doc.modules.first().expect("目录为空");

    let dir = std::env::temp_dir().join(format!("dounai-live-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("pkg.zip");
    let sources: Vec<String> = entry.sources.iter().map(|s| s.url.clone()).collect();
    let options = DownloadOptions {
        retries: 1,
        backoff_ms: 200,
        expected_sha256: Some(
            entry
                .sha256
                .trim_start_matches("sha256:")
                .to_string(),
        ),
    };
    let report = download(&ReqwestClient::default_client().unwrap(), &sources, &dest, &options)
        .expect("包应能下载并通过哈希校验");
    assert_eq!(report.bytes, entry.size, "下载大小与目录声明不一致");
    assert!(Path::new(&report.path).is_file());

    // 下下来的确实是一个可验的包，不是错误页。
    let bytes = std::fs::read(&report.path).unwrap();
    let verified = launcher_core::m2::package::verify_zip(
        &bytes,
        &launcher_core::m2::package::ZipExpectation::default(),
    )
    .expect("下载内容应是合法模块包");
    assert_eq!(verified.manifest.module_id, entry.module_id);
    println!(
        "下载成功：{} -> {} 字节，来源 {}，尝试 {} 次",
        entry.module_id, report.bytes, report.source, report.attempts
    );

    std::fs::remove_dir_all(&dir).ok();
}
