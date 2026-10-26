//! 启动器专项测试场景（必过项）。
//!
//! 必须通过的场景：
//! 1. 最小安装纯净机测试
//! 2. 推荐下载（含中断续传、失败回滚）
//! 3. 模块装卸循环 10 次无残留
//! 4. 离线模式降级
//! 5. 坏包/伪造包拒装
//! 6. 磁盘满/断电恢复
//! 7. 多版本共存切换
//!
//! 本文件用真实文件系统与真实 zip 字节覆盖这些场景中可自动化的部分；
//! 依赖真实机器状态的（如真正磁盘写满）以注入式故障模拟替代。

use dounai_launcher::catalog::{
    fetch_catalog, parse_catalog, Allowlist, CatalogFetcher, CatalogResult,
};
use dounai_launcher::download::{download, DownloadOptions, HttpFetcher};
use dounai_launcher::install::{extract_zip_into, InstallError, InstallReport, InstallTransaction};
use dounai_launcher::state::{Category, InstallState, StateStore};
use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-spec-{}-{}-{}",
        name,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    buf
}

fn install_from_zip(
    dest: &std::path::Path,
    zip: &[u8],
    hash: Option<&str>,
) -> Result<InstallReport, InstallError> {
    let tx = InstallTransaction::begin(dest)?;
    extract_zip_into(&tx, zip, hash)?;
    tx.commit()
}

/* ------------------------------------------------------------------ */
/* 场景 1：最小安装纯净机                                            */
/* ------------------------------------------------------------------ */

#[test]
fn scene1_clean_machine_minimal_install() {
    // 模拟"纯净机"：空目录，无任何既存状态
    let root = tmp_root("scene1-clean");
    let state_path = root.join("modules-state.json");

    // 初始状态必须为空，不报错
    let store = StateStore::open(&state_path).unwrap();
    assert!(store.all().is_empty(), "纯净机初始状态应为空");

    // 装一个核心模块
    let dest = root.join("modules").join("dounai-input");
    let zip = make_zip(&[("dounai_input/__init__.py", b"# pkg")]);
    let hash = dounai_launcher::sha256_hex(&zip);
    let report = install_from_zip(&dest, &zip, Some(&hash)).unwrap();
    assert_eq!(report.files, 1);
    assert!(!report.replaced);

    // 文件真实落盘
    assert!(dest.join("dounai_input/__init__.py").exists());

    // 状态记录正确
    let mut store = StateStore::open(&state_path).unwrap();
    store
        .mark_installed(
            "dounai-input",
            "0.1.0",
            &dest.to_string_lossy(),
            Category::Core,
        )
        .unwrap();
    let st = store.get("dounai-input").unwrap();
    assert_eq!(st.install, InstallState::Installed);
    assert!(st.enabled, "core 模块应默认启用");

    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 2：推荐下载（中断续传 + 失败回滚）                            */
/* ------------------------------------------------------------------ */

struct ScriptedFetcher {
    /// 第 n 次调用返回什么：(状态码, 数据) 或错误
    script: RefCell<Vec<(Option<u16>, Vec<u8>)>>,
    calls: RefCell<usize>,
}

impl HttpFetcher for ScriptedFetcher {
    fn fetch(
        &self,
        _url: &str,
        _range: Option<u64>,
    ) -> Result<dounai_launcher::download::FetchResponse, String> {
        let mut n = self.calls.borrow_mut();
        let idx = *n;
        *n += 1;
        match self.script.borrow().get(idx) {
            Some((Some(status), body)) => Ok(dounai_launcher::download::FetchResponse {
                status: *status,
                body: body.clone(),
            }),
            Some((None, _)) => Err("模拟中断".into()),
            None => Err("脚本耗尽".into()),
        }
    }
}

#[test]
fn scene2_interrupted_download_resumes() {
    let root = tmp_root("scene2-resume");
    let dest = root.join("model.bin");
    let part = root.join("model.bin.part");

    // 第一次：下载了前 4 字节后中断
    std::fs::write(&part, b"HEAD").unwrap();

    let fetcher = ScriptedFetcher {
        script: RefCell::new(vec![(Some(206), b"TAIL".to_vec())]),
        calls: RefCell::new(0),
    };
    let r = download(
        &fetcher,
        &["https://dl.example.com/m".into()],
        &dest,
        &DownloadOptions::default(),
    )
    .unwrap();
    // 关键：续传而非重下
    assert_eq!(r.resumed_from, 4, "应从断点续传");
    assert_eq!(std::fs::read(&dest).unwrap(), b"HEADTAIL");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene2_download_failure_rolls_back() {
    let root = tmp_root("scene2-rollback");
    let dest = root.join("model.bin");

    let fetcher = ScriptedFetcher {
        script: RefCell::new(vec![(None, vec![]), (None, vec![]), (None, vec![])]),
        calls: RefCell::new(0),
    };
    let opts = DownloadOptions {
        retries: 2,
        backoff_ms: 1,
        expected_sha256: None,
    };
    let err = download(&fetcher, &["https://dl.example.com/m".into()], &dest, &opts).unwrap_err();
    assert!(format!("{err}").contains("均失败"));
    // 不留目标文件
    assert!(!dest.exists(), "下载失败不应留下目标文件");
    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 3：装卸循环 10 次无残留                                        */
/* ------------------------------------------------------------------ */

#[test]
fn scene3_install_uninstall_cycle_10_times_no_residue() {
    let root = tmp_root("scene3-cycle");
    let state_path = root.join("modules-state.json");
    let zip = make_zip(&[("a.txt", b"data"), ("sub/b.txt", b"more")]);
    let hash = dounai_launcher::sha256_hex(&zip);

    for i in 1..=10 {
        let dest = root.join("modules").join("mod-cycle");

        // 安装
        let report = install_from_zip(&dest, &zip, Some(&hash)).unwrap();
        assert_eq!(report.files, 2, "第 {i} 轮安装文件数不对");
        assert!(dest.join("a.txt").exists());

        // 记录安装
        {
            let mut store = StateStore::open(&state_path).unwrap();
            store
                .mark_installed(
                    "mod-cycle",
                    "1.0.0",
                    &dest.to_string_lossy(),
                    Category::Optional,
                )
                .unwrap();
        }

        // 卸载
        {
            let mut store = StateStore::open(&state_path).unwrap();
            let removed = store.uninstall("mod-cycle", Category::Optional).unwrap();
            assert_eq!(removed.version, "1.0.0");
        }
        std::fs::remove_dir_all(&dest).ok();

        // 检查残留：不得留下 staging 目录或 .part
        let leftovers: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("staging") || n.ends_with(".part") || n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "第 {i} 轮有残留：{leftovers:?}");

        // 状态文件中不应再有该模块
        let store = StateStore::open(&state_path).unwrap();
        assert!(store.get("mod-cycle").is_none(), "第 {i} 轮状态未清除");
    }
    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 4：离线模式降级                                                */
/* ------------------------------------------------------------------ */

struct OfflineFetcher;
impl CatalogFetcher for OfflineFetcher {
    fn get(&self, _url: &str) -> Result<Vec<u8>, String> {
        Err("离线".into())
    }
}

#[test]
fn scene4_offline_falls_back_to_cache() {
    let root = tmp_root("scene4-offline");
    let cache = root.join("catalog.json");
    let catalog =
        br#"{"modules":[{"module_id":"a","download_url":"https://dl.example.com/a.zip"}]}"#;
    std::fs::write(&cache, catalog).unwrap();

    let al = Allowlist::new(["dl.example.com"]);
    let r = fetch_catalog(
        &OfflineFetcher,
        &al,
        "https://dl.example.com/catalog.json",
        &cache,
    );
    match r {
        CatalogResult::Cached(entries) => assert_eq!(entries.len(), 1),
        other => panic!("离线时应回退缓存，实际 {other:?}"),
    }
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene4_offline_without_cache_reports_unavailable() {
    let root = tmp_root("scene4-nocache");
    let cache = root.join("catalog.json");
    let al = Allowlist::new(["dl.example.com"]);
    let r = fetch_catalog(
        &OfflineFetcher,
        &al,
        "https://dl.example.com/catalog.json",
        &cache,
    );
    // 不得伪造空清单
    assert!(matches!(r, CatalogResult::Unavailable(_)));
    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 5：坏包 / 伪造包拒装                                           */
/* ------------------------------------------------------------------ */

#[test]
fn scene5_corrupt_zip_rejected() {
    let root = tmp_root("scene5-corrupt");
    let dest = root.join("mod");
    let garbage = b"this is not a zip file at all".to_vec();
    let err = install_from_zip(&dest, &garbage, None).unwrap_err();
    assert!(matches!(err, InstallError::Zip(_)), "实际 {err:?}");
    assert!(!dest.exists(), "坏包不应产生安装目录");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene5_forged_hash_rejected() {
    let root = tmp_root("scene5-forged");
    let dest = root.join("mod");
    let zip = make_zip(&[("a.txt", b"legit")]);
    let fake_hash = dounai_launcher::sha256_hex(b"something-else");

    let err = install_from_zip(&dest, &zip, Some(&fake_hash)).unwrap_err();
    assert!(matches!(err, InstallError::HashMismatch { .. }));
    assert!(!dest.exists(), "伪造包不应产生安装目录");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene5_malicious_zip_rejected_entirely() {
    let root = tmp_root("scene5-malicious");
    let dest = root.join("mod");
    // 混合包：一个正常条目 + 一个穿越条目
    let zip = make_zip(&[("good.txt", b"ok"), ("../../../etc/passwd", b"evil")]);
    let err = install_from_zip(&dest, &zip, None).unwrap_err();
    assert!(matches!(err, InstallError::UnsafeEntry(_)));
    assert!(!dest.exists(), "含恶意条目时不得产生任何安装结果");
    // 且绝不能越界写出
    assert!(!root.parent().unwrap().join("etc/passwd").exists());
    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 6：磁盘满 / 断电恢复                                           */
/* ------------------------------------------------------------------ */

#[test]
fn scene6_power_loss_recovers_to_broken() {
    let root = tmp_root("scene6-powerloss");
    let state_path = root.join("modules-state.json");

    // 模拟：安装进行中进程被杀（状态停留在 Installing）
    {
        let mut store = StateStore::open(&state_path).unwrap();
        store.mark_installing("mod-x").unwrap();
    }

    // 重启后：必须识别为 Broken，而不是假装已安装
    let store = StateStore::open(&state_path).unwrap();
    let st = store.get("mod-x").unwrap();
    assert_eq!(st.install, InstallState::Broken, "中断的安装应为 Broken");
    assert!(st.last_error.is_some(), "应带可展示的错误说明");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene6_staging_cleaned_after_failure() {
    let root = tmp_root("scene6-staging");
    let dest = root.join("mod");
    {
        // begin 后不 commit，模拟中途失败
        let tx = InstallTransaction::begin(&dest).unwrap();
        tx.place_file("partial.txt", b"x").unwrap();
    }
    let staging: Vec<String> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("staging"))
        .collect();
    assert!(
        staging.is_empty(),
        "失败后临时目录应被清理，实际 {staging:?}"
    );
    assert!(!dest.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene6_disk_full_simulated_write_failure_is_rolled_back() {
    // 用不可写路径模拟磁盘满：把目标指到一个已存在的文件下
    let root = tmp_root("scene6-diskfull");
    let blocker = root.join("blocker");
    std::fs::write(&blocker, b"i am a file").unwrap();

    // 目标目录的父路径是个文件 → 创建必然失败
    let dest = blocker.join("mod");
    let zip = make_zip(&[("a.txt", b"a")]);
    let err = install_from_zip(&dest, &zip, None).unwrap_err();
    // 应返回 IO 错误而非 panic
    assert!(matches!(err, InstallError::Io(_)), "实际 {err:?}");
    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 场景 7：多版本共存切换                                              */
/* ------------------------------------------------------------------ */

#[test]
fn scene7_version_upgrade_replaces_cleanly() {
    let root = tmp_root("scene7-upgrade");
    let dest = root.join("mod");

    // v1
    let zip1 = make_zip(&[("version.txt", b"1.0.0"), ("old-only.txt", b"legacy")]);
    let r1 = install_from_zip(&dest, &zip1, None).unwrap();
    assert!(!r1.replaced);
    assert_eq!(std::fs::read(dest.join("version.txt")).unwrap(), b"1.0.0");
    assert!(dest.join("old-only.txt").exists());

    // v2 覆盖：旧文件不应残留
    let zip2 = make_zip(&[("version.txt", b"2.0.0"), ("new-only.txt", b"fresh")]);
    let r2 = install_from_zip(&dest, &zip2, None).unwrap();
    assert!(r2.replaced, "升级应报告 replaced");
    assert_eq!(std::fs::read(dest.join("version.txt")).unwrap(), b"2.0.0");
    assert!(dest.join("new-only.txt").exists());
    assert!(!dest.join("old-only.txt").exists(), "旧版本文件不应残留");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn scene7_version_recorded_in_state() {
    let root = tmp_root("scene7-state");
    let state_path = root.join("modules-state.json");
    let mut store = StateStore::open(&state_path).unwrap();

    store
        .mark_installed("m", "1.0.0", "path1", Category::Optional)
        .unwrap();
    assert_eq!(store.get("m").unwrap().version, "1.0.0");

    // 升级
    store
        .mark_installed("m", "2.0.0", "path2", Category::Optional)
        .unwrap();
    let st = store.get("m").unwrap();
    assert_eq!(st.version, "2.0.0");
    assert_eq!(st.path, "path2");

    std::fs::remove_dir_all(&root).ok();
}

/* ------------------------------------------------------------------ */
/* 补充：清单解析与白名单在本场景中的组合行为                          */
/* ------------------------------------------------------------------ */

#[test]
fn scene_extra_catalog_rejects_non_https_download() {
    let raw = br#"{"modules":[{"module_id":"x","download_url":"http://dl.example.com/x.zip"}]}"#;
    let err = parse_catalog(raw).unwrap_err();
    assert!(err.contains("https"), "应提示必须是 https：{err}");
}

#[test]
fn scene_extra_allowlist_blocks_wildcard_abuse() {
    let al = Allowlist::new(["*.example.com"]);
    // 不能用 *. 匹配任意域名
    assert!(al.check("https://evil.com/x").is_err());
    assert!(al.check("https://example.com.evil.com/x").is_err());
}
