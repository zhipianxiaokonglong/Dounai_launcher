//! 覆盖三个环节：发布工具打包、M2 运行时验签、信任判定。
//!
//! 默认用固定种子，CI 可复现。设置 `DOUNAI_K1_KEY` 指向私钥文件时改用真实
//! K1，并顺带断言内置指纹与这把私钥匹配。私钥不入库。

use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dounai_launcher::release::{
    check_manifest_basics, load_signing_key, scan_files, sign_manifest, write_zip,
};
use dounai_launcher::state_v2::BUILTIN_K1_FINGERPRINT;
use ed25519_dalek::SigningKey;
use launcher_core::m2::package::{verify_zip, ZipExpectation};
use launcher_core::m2::trust::{classify, ClassifyContext, TrustStore};
use launcher_core::m2::{fingerprint, Verdict};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../../contracts/examples/omninexus.v2.json");
const K5_PLACEHOLDER: &str =
    "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const UNRELATED_K1: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn signing_key() -> SigningKey {
    match std::env::var("DOUNAI_K1_KEY") {
        Ok(path) => load_signing_key(Path::new(&path)).expect("DOUNAI_K1_KEY 指向的私钥不可用"),
        Err(_) => SigningKey::from_bytes(&[7u8; 32]),
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

    let store = TrustStore::new(&fp, K5_PLACEHOLDER).unwrap();
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

    let store = TrustStore::new(UNRELATED_K1, K5_PLACEHOLDER).unwrap();
    let result = classify(&store, &verified.manifest, &ClassifyContext::default()).unwrap();
    assert_eq!(result.verdict, Verdict::D);
    assert!(result.signature_present);
}
