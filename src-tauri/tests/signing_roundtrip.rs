//! 覆盖四个环节：发布工具打包与签索引、M2 运行时验签、信任判定。
//!
//! 默认用固定种子，CI 可复现。设置 `DOUNAI_K1_KEY` 或 `DOUNAI_K5_KEY` 指向
//! 私钥文件时改用真实密钥，并顺带断言内置公钥与指纹和这把私钥匹配。
//! 私钥不入库。

use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::release::{
    check_manifest_basics, load_signing_key, scan_files, sign_catalog_doc, sign_manifest, write_zip,
};
use dounai_launcher::state_v2::{
    BUILTIN_K1_FINGERPRINT, BUILTIN_K5_FINGERPRINT, BUILTIN_K5_PUBKEY_B64,
};
use ed25519_dalek::SigningKey;
use launcher_core::m2::catalog::CatalogDocument;
use launcher_core::m2::package::{verify_zip, ZipExpectation};
use launcher_core::m2::trust::{classify, ClassifyContext, TrustStore};
use launcher_core::m2::{fingerprint, Verdict};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../../contracts/examples/omninexus.v2.json");
const UNRELATED_K1: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn signing_key() -> SigningKey {
    match std::env::var("DOUNAI_K1_KEY") {
        Ok(path) => load_signing_key(Path::new(&path)).expect("DOUNAI_K1_KEY 指向的私钥不可用"),
        Err(_) => SigningKey::from_bytes(&[7u8; 32]),
    }
}

fn k5_signing_key() -> SigningKey {
    match std::env::var("DOUNAI_K5_KEY") {
        Ok(path) => load_signing_key(Path::new(&path)).expect("DOUNAI_K5_KEY 指向的私钥不可用"),
        Err(_) => SigningKey::from_bytes(&[11u8; 32]),
    }
}

fn build_signed_zip(sk: &SigningKey) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("manifest.json"), EXAMPLE).unwrap();
    std::fs::write(
        src.join("pyproject.toml"),
        b"[project]\nname = \"omninexus\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut manifest: Value = serde_json::from_str(EXAMPLE).unwrap();
    check_manifest_basics(&manifest).unwrap();
    let files = scan_files(&src).unwrap();
    manifest["files"] = json!(files);
    sign_manifest(&mut manifest, sk, "K1").unwrap();

    let out = dir.path().join("module.zip");
    write_zip(
        &out,
        &src,
        &serde_json::to_string_pretty(&manifest).unwrap(),
        &files,
    )
    .unwrap();
    std::fs::read(&out).unwrap()
}

#[test]
fn tool_signed_package_is_trusted_by_the_runtime() {
    let sk = signing_key();
    let pk = B64.encode(sk.verifying_key().to_bytes());
    let fp = fingerprint(&pk).unwrap();
    let bytes = build_signed_zip(&sk);

    if std::env::var_os("DOUNAI_K1_KEY").is_some() {
        assert_eq!(fp, BUILTIN_K1_FINGERPRINT, "真实 K1 指纹未登记进 state_v2");
    }

    let verified = verify_zip(&bytes, &ZipExpectation::default()).unwrap();
    assert_eq!(verified.manifest.module_id, "dounai/omninexus");
    assert_eq!(verified.files.len(), 1);
    assert!(verified.manifest.is_signed());

    let store = TrustStore::new(&fp, BUILTIN_K5_FINGERPRINT).unwrap();
    let result = classify(&store, &verified.manifest, &ClassifyContext::default()).unwrap();
    assert_eq!(result.verdict, Verdict::A);
    assert_eq!(result.publisher_fp.as_deref(), Some(fp.as_str()));
}

#[test]
fn runtime_rejects_a_tampered_entry_command() {
    let sk = signing_key();
    let bytes = build_signed_zip(&sk);
    let mut verified = verify_zip(&bytes, &ZipExpectation::default()).unwrap();
    verified.manifest.raw["entry"]["command"] = json!("evil.exe");

    let store = TrustStore::new(UNRELATED_K1, BUILTIN_K5_FINGERPRINT).unwrap();
    let result = classify(&store, &verified.manifest, &ClassifyContext::default()).unwrap();
    assert_eq!(result.verdict, Verdict::D);
    assert!(result.signature_present);
}

fn unsigned_catalog() -> Value {
    json!({
        "schema_version": 2,
        "sequence": 1,
        "generated_at": "2026-11-13T07:10:00Z",
        "expires": "2026-11-15T07:10:00Z",
        "sources": [{
            "id": "sourceforge",
            "kind": "sourceforge",
            "base_url": "https://sourceforge.net/projects/dounai-releases/files/",
            "priority": 1,
            "enabled": true
        }],
        "modules": [{
            "module_id": "dounai/omninexus",
            "variant": "stable",
            "version": "0.3.3",
            "role": "llm",
            "name": "OmniNexus",
            "publisher_fp": BUILTIN_K1_FINGERPRINT,
            "sha256": format!("sha256:{}", "a".repeat(64)),
            "size": 1024,
            "sources": [{
                "source_id": "sourceforge",
                "url": "https://sourceforge.net/projects/dounai-releases/files/omninexus-0.3.3.zip"
            }],
            "signature_required": true,
            "expires": "2026-11-15T07:10:00Z"
        }],
        "revoked": [],
        "signature": {
            "algo": "Ed25519",
            "key_id": "K5",
            "signed_at": "2026-11-13T07:10:00Z",
            "value": ""
        }
    })
}

/// 发布工具用 K5 签出的 catalog，必须能被启动器内置的 K5 公钥验通。
#[test]
fn tool_signed_catalog_is_accepted_by_the_runtime() {
    let sk = k5_signing_key();
    let pk = B64.encode(sk.verifying_key().to_bytes());
    if std::env::var_os("DOUNAI_K5_KEY").is_some() {
        assert_eq!(pk, BUILTIN_K5_PUBKEY_B64, "真实 K5 公钥未登记进 state_v2");
        assert_eq!(
            fingerprint(&pk).unwrap(),
            BUILTIN_K5_FINGERPRINT,
            "真实 K5 指纹未登记进 state_v2"
        );
    }

    let mut doc = unsigned_catalog();
    sign_catalog_doc(&mut doc, &sk, "K5").unwrap();
    let text = serde_json::to_string(&doc).unwrap();
    let fp = fingerprint(&pk).unwrap();
    let parsed = CatalogDocument::parse(&text, &pk, &fp).unwrap();
    assert_eq!(parsed.sequence, 1);
    assert_eq!(parsed.modules.len(), 1);

    // 改动条目内容后签名不再匹配，parse 必须在内容校验之前硬拒。
    let mut tampered = doc.clone();
    tampered["modules"][0]["version"] = json!("9.9.9");
    let err = CatalogDocument::parse(&serde_json::to_string(&tampered).unwrap(), &pk, &fp)
        .unwrap_err();
    assert!(err.to_string().contains("验签失败"), "实际错误：{err}");

    // 换一把钥匙签的同内容 catalog 不能靠自述 pubkey 蒙混过关。
    let other = SigningKey::from_bytes(&[42u8; 32]);
    let mut forged = unsigned_catalog();
    sign_catalog_doc(&mut forged, &other, "community-other").unwrap();
    forged["signature"]["pubkey"] =
        json!(B64.encode(other.verifying_key().to_bytes()));
    let err = CatalogDocument::parse(&serde_json::to_string(&forged).unwrap(), &pk, &fp)
        .unwrap_err();
    assert!(err.to_string().contains("不得自述 pubkey"), "实际错误：{err}");
}
