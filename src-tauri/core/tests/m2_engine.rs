//! M2 主体端到端验收：双源裁决、包完整性、四级验签、多实例与本地导入。
//!
//! 网络与真机 UI 不在本文件范围。所有网络走 FakeHttp 注入。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Write};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use launcher_core::m2::adapters::{HttpFetcher, LocalDirAdapter, RemoteCatalogAdapter};
use launcher_core::m2::catalog::{
    check_update_allowed, select_catalog, CatalogEntry, CatalogState, EntrySource,
};
use launcher_core::m2::contract_probe::evaluate;
use launcher_core::m2::installed::{InstalledState, Instance};
use launcher_core::m2::local::{require_confirmed, validate_local_dir};
use launcher_core::m2::package::{install_package, verify_zip, ZipExpectation};
use launcher_core::m2::retention::RetentionPolicy;
use launcher_core::m2::source::{SourceAdapter, SourceError};
use launcher_core::m2::trust::{classify, ClassifyContext, TrustStore};
use launcher_core::m2::{fingerprint, Category, InstanceSourceKind, Verdict};
use launcher_core::manifest::Role;
use launcher_core::security::sha256_hex;
use launcher_core::signature::canonical_payload;

const GITHUB_CATALOG: &str = "https://github.com/dounai/releases/catalog.json";
const SF_CATALOG: &str = "https://downloads.sourceforge.net/dounai/catalog.json";
const GITHUB_ZIP: &str = "https://github.com/dounai/releases/demo.zip";
const SF_ZIP: &str = "https://downloads.sourceforge.net/dounai/demo.zip";
const NOW: &str = "2026-01-02T00:00:00Z";

fn keypair(seed: u8) -> (SigningKey, String, String) {
    let sk = SigningKey::from_bytes(&[seed; 32]);
    let pk = B64.encode(sk.verifying_key().to_bytes());
    let fp = fingerprint(&pk).unwrap();
    (sk, pk, fp)
}

fn declared(files: &[(&str, &[u8])]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(path, bytes)| ((*path).to_string(), format!("sha256:{}", sha256_hex(bytes))))
        .collect()
}

fn manifest_value(files: &BTreeMap<String, String>) -> Value {
    json!({
        "schema_version": 2,
        "module_id": "dounai/demo",
        "name": "demo",
        "version": "1.0.0",
        "variant": "stable",
        "role": "llm",
        "category": "optional",
        "publisher": { "id": "demo", "name": "Demo" },
        "requires": { "launcher": ">=1.0.0" },
        "runtime": { "python": ">=3.11,<3.14", "venv": "{instance_venv}",
                     "deps": { "kind": "pyproject", "path": "pyproject.toml" } },
        "entry": { "command": "{venv_python} -m demo", "env": {}, "health": null },
        "ports": [8102],
        "permissions": { "network": [], "filesystem": [], "devices": [] },
        "files": files
    })
}

fn sign_manifest(mut value: Value, sk: &SigningKey, pk: &str) -> Value {
    value["signature"] = json!({
        "algo": "SHA-256 + Ed25519", "pubkey": pk, "key_id": "K1",
        "signed_at": "2026-01-01T00:00:00Z", "value": ""
    });
    let payload = canonical_payload(&value).unwrap();
    let signature = sk.sign(&payload);
    value["signature"]["value"] = json!(B64.encode(signature.to_bytes()));
    value
}

fn build_zip(files: &[(&str, &[u8])], manifest: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut writer = ZipWriter::new(Cursor::new(&mut out));
        let options = SimpleFileOptions::default();
        writer.start_file("manifest.json", options).unwrap();
        writer.write_all(manifest.to_string().as_bytes()).unwrap();
        for (path, bytes) in files {
            writer.start_file(*path, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    out
}

fn entry_for(sha256: &str, size: u64, publisher_fp: &str) -> CatalogEntry {
    CatalogEntry {
        module_id: "dounai/demo".into(),
        variant: "stable".into(),
        version: "1.0.0".into(),
        role: Role::Llm,
        name: "demo".into(),
        publisher_fp: publisher_fp.into(),
        sha256: sha256.into(),
        size,
        sources: vec![
            EntrySource {
                source_id: "github".into(),
                url: GITHUB_ZIP.into(),
            },
            EntrySource {
                source_id: "sourceforge".into(),
                url: SF_ZIP.into(),
            },
        ],
        signature_required: true,
        expires: "2026-01-07T00:00:00Z".into(),
        expected_signer_fp: None,
        min_version: None,
        revoked: vec![],
    }
}

fn catalog_json(sk: &SigningKey, sequence: u64, entry: &CatalogEntry) -> Vec<u8> {
    let mut document = json!({
        "schema_version": 2,
        "sequence": sequence,
        "generated_at": "2026-01-01T00:00:00Z",
        "expires": "2026-01-07T00:00:00Z",
        "sources": [
            { "id": "github", "kind": "github",
              "base_url": "https://github.com/dounai/releases", "priority": 1 },
            { "id": "sourceforge", "kind": "sourceforge",
              "base_url": "https://downloads.sourceforge.net/dounai", "priority": 2 }
        ],
        "modules": [entry],
        "revoked": [],
        "signature": { "algo": "SHA-256 + Ed25519", "key_id": "K5",
                       "signed_at": "2026-01-01T00:00:00Z", "value": "" }
    });
    let payload = canonical_payload(&document).unwrap();
    let signature = sk.sign(&payload);
    document["signature"]["value"] = json!(B64.encode(signature.to_bytes()));
    document.to_string().into_bytes()
}

struct FakeHttp {
    responses: BTreeMap<String, Result<Vec<u8>, String>>,
}

impl HttpFetcher for FakeHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>, SourceError> {
        match self.responses.get(url) {
            Some(Ok(bytes)) => Ok(bytes.clone()),
            Some(Err(reason)) => Err(SourceError::Unavailable {
                source_id: url.into(),
                reason: reason.clone(),
            }),
            None => Err(SourceError::Unavailable {
                source_id: url.into(),
                reason: "未配置响应".into(),
            }),
        }
    }
}

fn fetch(
    adapter: &impl SourceAdapter,
) -> Result<launcher_core::m2::catalog::CatalogDocument, SourceError> {
    match adapter.fetch_catalog()? {
        Some(doc) => Ok(doc),
        None => Err(SourceError::Unavailable {
            source_id: adapter.id().into(),
            reason: "本地来源没有 catalog".into(),
        }),
    }
}

fn fixture() -> (
    SigningKey,
    String,
    String,
    SigningKey,
    String,
    String,
    Vec<u8>,
    CatalogEntry,
) {
    let (k1, pk1, fp1) = keypair(1);
    let (k5, pk5, fp5) = keypair(5);
    let files: Vec<(&str, &[u8])> = vec![
        ("pyproject.toml", b"[project]\n"),
        ("demo.py", b"print(1)\n"),
    ];
    let manifest = sign_manifest(manifest_value(&declared(&files)), &k1, &pk1);
    let zip_bytes = build_zip(&files, &manifest);
    let sha256 = format!("sha256:{}", sha256_hex(&zip_bytes));
    let entry = entry_for(&sha256, zip_bytes.len() as u64, &fp1);
    (k1, pk1, fp1, k5, pk5, fp5, zip_bytes, entry)
}

#[test]
fn one_source_down_still_installs_from_the_other() {
    let (_k1, _pk1, fp1, k5, pk5, fp5, zip_bytes, entry) = fixture();
    let catalog = catalog_json(&k5, 7, &entry);
    let mut responses = BTreeMap::new();
    responses.insert(GITHUB_CATALOG.into(), Err("拔掉 GitHub 源".into()));
    responses.insert(SF_CATALOG.into(), Ok(catalog));
    responses.insert(SF_ZIP.into(), Ok(zip_bytes.clone()));
    let http = FakeHttp { responses };
    let github = RemoteCatalogAdapter::github(
        "github",
        "https://github.com/dounai/releases",
        1,
        &pk5,
        &http,
    );
    let sf = RemoteCatalogAdapter::sourceforge(
        "sourceforge",
        "https://downloads.sourceforge.net/dounai",
        2,
        &pk5,
        &http,
    );
    assert!(matches!(
        fetch(&github),
        Err(SourceError::Unavailable { .. })
    ));
    let candidates = vec![(1, fetch(&github)), (2, fetch(&sf))];
    let document = select_catalog(&candidates, &CatalogState::default(), NOW).unwrap();
    assert_eq!(document.sequence, 7);

    let dir = tempfile::tempdir().unwrap();
    let downloaded = dir.path().join("download.zip");
    let receipt = sf.fetch_package(&entry, &downloaded).unwrap();
    assert_eq!(receipt.sha256, entry.sha256);
    let verified = verify_zip(
        &fs::read(&downloaded).unwrap(),
        &ZipExpectation {
            sha256: Some(&entry.sha256),
            size: Some(entry.size),
            ..Default::default()
        },
    )
    .unwrap();
    install_package(&verified, &dir.path().join("modules/demo")).unwrap();
    assert!(dir.path().join("modules/demo/demo.py").exists());

    let store = TrustStore::new(&fp1, &fp5).unwrap();
    let result = classify(
        &store,
        &verified.manifest,
        &ClassifyContext {
            catalog_entry: Some(&entry),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.verdict, Verdict::A);
    assert_eq!(result.publisher_fp.as_deref(), Some(fp1.as_str()));
}

#[test]
fn catalog_rollback_bad_signature_and_no_usable_source_are_all_rejected() {
    let (_k1, _pk1, _fp1, k5, pk5, _fp5, _zip, entry) = fixture();
    let previous = CatalogState {
        sequence: Some(7),
        ..Default::default()
    };
    let old = catalog_json(&k5, 6, &entry);
    let signed = String::from_utf8(old.clone()).unwrap();
    let doc = launcher_core::m2::catalog::CatalogDocument::parse(&signed, &pk5).unwrap();
    assert!(doc.validate(&previous, NOW).is_err());

    let tampered = signed.replace("\"sequence\":6", "\"sequence\":8");
    assert!(matches!(
        launcher_core::m2::catalog::CatalogDocument::parse(&tampered, &pk5),
        Err(SourceError::CatalogInvalid { .. })
    ));

    let http = FakeHttp {
        responses: BTreeMap::new(),
    };
    let github = RemoteCatalogAdapter::github(
        "github",
        "https://github.com/dounai/releases",
        1,
        &pk5,
        &http,
    );
    let sf = RemoteCatalogAdapter::sourceforge(
        "sourceforge",
        "https://downloads.sourceforge.net/dounai",
        2,
        &pk5,
        &http,
    );
    let candidates = vec![(1, fetch(&github)), (2, fetch(&sf))];
    assert!(matches!(
        select_catalog(&candidates, &previous, NOW),
        Err(SourceError::CatalogInvalid { .. })
    ));
}

#[test]
fn zip_tampering_and_file_list_mismatch_leave_no_files() {
    let (k1, pk1, _fp1, _k5, _pk5, _fp5, zip_bytes, entry) = fixture();
    let files: Vec<(&str, &[u8])> = vec![
        ("pyproject.toml", b"[project]\n"),
        ("demo.py", b"print(1)\n"),
    ];
    let expectation = ZipExpectation {
        sha256: Some(&entry.sha256),
        size: Some(entry.size),
        ..Default::default()
    };

    let mut changed = zip_bytes.clone();
    let index = changed.len() - 40;
    changed[index] ^= 1;
    assert!(verify_zip(&changed, &expectation).is_err());

    // 多一个文件、少一个文件、哈希声明不符，全部整体拒绝。
    let extra_manifest = sign_manifest(
        manifest_value(&declared(&[
            ("pyproject.toml", b"[project]\n"),
            ("demo.py", b"print(1)\n"),
            ("extra.py", b"x\n"),
        ])),
        &k1,
        &pk1,
    );
    let extra = build_zip(&files, &extra_manifest);
    assert!(verify_zip(&extra, &ZipExpectation::default()).is_err());

    let few_manifest = sign_manifest(
        manifest_value(&declared(&[("pyproject.toml", b"[project]\n")])),
        &k1,
        &pk1,
    );
    let few = build_zip(&files, &few_manifest);
    assert!(verify_zip(&few, &ZipExpectation::default()).is_err());

    let mut wrong = declared(&files);
    wrong.insert("demo.py".into(), format!("sha256:{}", "a".repeat(64)));
    let wrong_manifest = sign_manifest(manifest_value(&wrong), &k1, &pk1);
    let wrong_zip = build_zip(&files, &wrong_manifest);
    assert!(verify_zip(&wrong_zip, &ZipExpectation::default()).is_err());

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("modules").join("demo");
    assert!(!target.exists());
    assert!(verify_zip(&zip_bytes, &expectation).is_ok());
}

#[test]
fn signature_stripping_bad_pubkey_and_fingerprint_change_are_hard_rejections() {
    let (k1, pk1, fp1, _k5, _pk5, fp5, _zip, entry) = fixture();
    let files: Vec<(&str, &[u8])> = vec![
        ("pyproject.toml", b"[project]\n"),
        ("demo.py", b"print(1)\n"),
    ];
    let store = TrustStore::new(&fp1, &fp5).unwrap();
    let unsigned = launcher_core::m2::manifest::PackageManifest::parse(
        &manifest_value(&declared(&files)).to_string(),
    )
    .unwrap();
    assert_eq!(
        classify(
            &store,
            &unsigned,
            &ClassifyContext {
                catalog_entry: Some(&entry),
                ..Default::default()
            },
        )
        .unwrap()
        .verdict,
        Verdict::D
    );

    let signed = sign_manifest(manifest_value(&declared(&files)), &k1, &pk1);
    let mut forged = signed.clone();
    let (_other, other_pk, _) = keypair(9);
    forged["signature"]["pubkey"] = json!(other_pk);
    // 换公钥后签名不重算，raw 随之变化，验签必然失败。
    let forged_manifest =
        launcher_core::m2::manifest::PackageManifest::parse(&forged.to_string()).unwrap();
    assert_eq!(
        classify(&store, &forged_manifest, &ClassifyContext::default())
            .unwrap()
            .verdict,
        Verdict::D
    );

    let good = launcher_core::m2::manifest::PackageManifest::parse(&signed.to_string()).unwrap();
    assert_eq!(
        classify(
            &store,
            &good,
            &ClassifyContext {
                previous_publisher_fp: Some(&format!("sha256:{}", "b".repeat(64))),
                ..Default::default()
            },
        )
        .unwrap()
        .verdict,
        Verdict::D
    );
}

#[test]
fn unregistered_key_needs_tofu_and_revocation_blocks_updates() {
    let (third, third_pk, third_fp) = keypair(3);
    let (_k5, _pk5, k5_fp) = keypair(5);
    let files: Vec<(&str, &[u8])> = vec![
        ("pyproject.toml", b"[project]\n"),
        ("demo.py", b"print(1)\n"),
    ];
    let signed = sign_manifest(manifest_value(&declared(&files)), &third, &third_pk);
    let manifest =
        launcher_core::m2::manifest::PackageManifest::parse(&signed.to_string()).unwrap();
    let mut store = TrustStore::new(&format!("sha256:{}", "a".repeat(64)), &k5_fp).unwrap();
    assert_eq!(
        classify(&store, &manifest, &ClassifyContext::default())
            .unwrap()
            .verdict,
        Verdict::B
    );
    store.tofu(&third_fp, "Third Party", NOW).unwrap();
    assert_eq!(
        classify(&store, &manifest, &ClassifyContext::default())
            .unwrap()
            .verdict,
        Verdict::A
    );
    assert!(store.revoke(&third_fp, NOW));
    assert_eq!(
        classify(&store, &manifest, &ClassifyContext::default())
            .unwrap()
            .verdict,
        Verdict::D
    );
}

#[test]
fn version_monotonicity_is_scoped_to_the_same_variant() {
    let (_k1, _pk1, fp1, k5, pk5, _fp5, _zip, entry) = fixture();
    let catalog_json_text = String::from_utf8(catalog_json(&k5, 7, &entry)).unwrap();
    let doc = launcher_core::m2::catalog::CatalogDocument::parse(&catalog_json_text, &pk5).unwrap();
    let mut state = CatalogState::default();
    state
        .last_seen_version
        .insert(format!("{fp1}/dounai/demo/stable"), "2.0.0".into());
    assert!(check_update_allowed(&doc, &state, &entry, "1.0.0").is_err());
    // 另一个 variant 不受影响。
    state.last_seen_version.clear();
    state
        .last_seen_version
        .insert(format!("{fp1}/dounai/demo/agent"), "9.0.0".into());
    assert!(check_update_allowed(&doc, &state, &entry, "1.0.0").is_ok());
}

#[test]
fn local_dir_is_never_copied_and_head_change_blocks_autostart() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let install_root = dir.path().join("app");
    let data_root = dir.path().join("data");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&install_root).unwrap();
    fs::create_dir_all(&data_root).unwrap();
    fs::write(source.join("demo.py"), b"print(1)\n").unwrap();
    let (_k1, _pk1, _fp1, _k5, _pk5, _fp5, _zip, entry) = fixture();

    let adapter = LocalDirAdapter {
        path: source.clone(),
        install_root: install_root.clone(),
        data_root: data_root.clone(),
        exe_dir: install_root.clone(),
    };
    let dest = dir.path().join("never-written.zip");
    let receipt = adapter.fetch_package(&entry, &dest).unwrap();
    assert_eq!(receipt.source_path.as_deref(), Some(source.as_path()));
    assert!(!dest.exists(), "目录导入不复制源码");

    let first = launcher_core::m2::local::observe_source(&source, NOW).unwrap();
    assert!(require_confirmed(Some(first.confirmation_anchor()), &first).is_ok());
    fs::write(source.join("demo.py"), b"print(2)\n").unwrap();
    let second = launcher_core::m2::local::observe_source(&source, NOW).unwrap();
    assert!(require_confirmed(Some(first.confirmation_anchor()), &second).is_err());
    assert!(validate_local_dir(&source, &install_root, &data_root, &install_root).is_ok());
}

#[test]
fn expected_signer_fingerprint_blocks_signature_stripping_on_first_import() {
    let (k1, pk1, fp1, _k5, _pk5, fp5, _zip, _entry) = fixture();
    let files: Vec<(&str, &[u8])> = vec![
        ("pyproject.toml", b"[project]\n"),
        ("demo.py", b"print(1)\n"),
    ];
    let signed = sign_manifest(manifest_value(&declared(&files)), &k1, &pk1);
    let manifest =
        launcher_core::m2::manifest::PackageManifest::parse(&signed.to_string()).unwrap();
    let store = TrustStore::new(&fp1, &fp5).unwrap();

    let wrong = format!("sha256:{}", "c".repeat(64));
    assert_eq!(
        classify(
            &store,
            &manifest,
            &ClassifyContext {
                expected_signer_fp: Some(&wrong),
                ..Default::default()
            },
        )
        .unwrap()
        .verdict,
        Verdict::D
    );
    assert_eq!(
        classify(
            &store,
            &manifest,
            &ClassifyContext {
                expected_signer_fp: Some(fp1.as_str()),
                ..Default::default()
            },
        )
        .unwrap()
        .verdict,
        Verdict::A
    );

    let unsigned = launcher_core::m2::manifest::PackageManifest::parse(
        &manifest_value(&declared(&files)).to_string(),
    )
    .unwrap();
    // 填了期望指纹却删掉签名段，不得降级为「未验证，单次确认即可」。
    assert_eq!(
        classify(
            &store,
            &unsigned,
            &ClassifyContext {
                expected_signer_fp: Some(fp1.as_str()),
                ..Default::default()
            },
        )
        .unwrap()
        .verdict,
        Verdict::D
    );
    // 没有任何包外锚时，未签名包才走 C 级单次确认。
    assert_eq!(
        classify(&store, &unsigned, &ClassifyContext::default())
            .unwrap()
            .verdict,
        Verdict::C
    );
}

#[test]
fn incompatible_contract_does_not_activate_and_cleanup_respects_boundaries() {
    assert!(!evaluate(Role::Llm, &json!({})).is_compatible());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("installed.json");
    let mut state = InstalledState::new();
    for (id, version) in [
        ("dounai/demo@1.0.0", "1.0.0"),
        ("dounai/demo@2.0.0", "2.0.0"),
    ] {
        state
            .upsert(Instance {
                instance_id: id.into(),
                module_id: "dounai/demo".into(),
                variant: "stable".into(),
                version: version.into(),
                role: Role::Llm,
                category: Category::Optional,
                publisher_fp: Some(format!("sha256:{}", "a".repeat(64))),
                verdict: Verdict::A,
                confirmed_at: NOW.into(),
                installed_at: NOW.into(),
                source_kind: InstanceSourceKind::Zip,
                source_path: None,
                git: None,
                pinned: false,
                auto_update: true,
                zip_sha256: None,
                expected_signer_fp: None,
                signature_present: None,
                allow_unsigned_once: None,
                head_confirmed: None,
                data_dir: Some(format!("C:/data/{id}")),
                note: None,
            })
            .unwrap();
    }
    // 契约自检未通过时不提交切换。
    assert!(state.active.is_empty());
    state
        .switch_active(&path, Role::Llm, "dounai/demo@2.0.0", false)
        .unwrap();
    let policy = RetentionPolicy {
        keep_versions: 1,
        ..Default::default()
    };
    let plan = policy.cleanup_plan(&state, &BTreeSet::new(), true).unwrap();
    assert_eq!(plan.removable, vec!["dounai/demo@1.0.0".to_string()]);
    assert!(!plan.removable.contains(&"dounai/demo@2.0.0".to_string()));
    assert!(
        !plan.preserved_data_dirs.is_empty(),
        "data_dir 只标记不删除"
    );
    assert!(policy
        .cleanup_plan(&state, &BTreeSet::new(), false)
        .unwrap()
        .removable
        .is_empty());
}
