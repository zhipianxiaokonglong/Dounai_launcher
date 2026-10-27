//! 模块启停端到端测试（对应 docs/RUNNING.md §9.1 的 R1–R10）。
//!
//! 用**真实子进程**验证，而不是 mock：启停的关键风险全在进程管理上
//! （拉不起来、停不干净、死进程显示在跑），mock 掉就测不到了。
//!
//! 为了让测试跨平台且不依赖真实上游，这里安装一个"假模块"：
//! 它的 manifest 声明 `cmd /c timeout /t N`（Windows）或 `sleep N`（Unix），
//! 因此无需任何上游服务即可验证启停链路。

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::json;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::{
    install_module_inner, start_all_inner, start_module_inner, AppState, InstallRequest,
};
use dounai_launcher::state::Category;
use dounai_launcher::trust::TrustPolicy;

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dounai-run-{}-{}-{}",
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

fn state(root: &Path) -> AppState {
    AppState::with_trust(
        root.to_path_buf(),
        Allowlist::new(Vec::<String>::new()),
        TrustPolicy::permissive(),
    )
    .unwrap()
}

/// 造一个可启动的假模块包：manifest 声明一个会存活数秒的命令。
fn zip_with_manifest(module_id: &str, command: &str, health_url: Option<&str>) -> Vec<u8> {
    let manifest = json!({
        "module_id": module_id,
        "name": module_id,
        "version": "1.0.0",
        "category": "core",
        "entry": {
            "type": "command",
            "command": command,
            "working_dir": "",
            "env": {},
            "health": { "url": health_url, "timeout_ms": 500 }
        },
        "dependencies": [],
        "model_packs": [],
        "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    });

    let mut buf = Vec::new();
    {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        w.start_file("manifest.json", opts).unwrap();
        w.write_all(manifest.to_string().as_bytes()).unwrap();
        w.finish().unwrap();
    }
    buf
}

fn install(state: &AppState, module_id: &str, command: &str, health: Option<&str>) {
    install_as(state, module_id, command, health, "core");
}

/// 按指定分类安装假模块。
///
/// 必须显式传分类：`mark_installed` 会按分类决定初始 `enabled`
/// （reserved 一律禁用）。用错分类会让 reserved 变成"可启动"，
/// 从而绕过分类约束——这正是 `r6_start_all_skips_reserved` 要防的。
fn install_as(
    state: &AppState,
    module_id: &str,
    command: &str,
    health: Option<&str>,
    category: &str,
) {
    let zip = zip_with_manifest(module_id, command, health);
    let hash = dounai_launcher::sha256_hex(&zip);
    install_module_inner(
        state,
        InstallRequest {
            module_id: module_id.to_string(),
            category: category.to_string(),
            version: "1.0.0".to_string(),
            zip_base64: B64.encode(&zip),
            expected_sha256: hash,
            manifest: json!({ "module_id": module_id }),
        },
    )
    .expect("安装假模块失败");
}

fn long_running_cmd(seconds: u32) -> String {
    if cfg!(windows) {
        format!("cmd /c timeout /t {seconds}")
    } else {
        format!("sleep {seconds}")
    }
}

fn quick_exit_cmd(code: i32) -> String {
    if cfg!(windows) {
        format!("cmd /c exit {code}")
    } else {
        format!("exit {code}")
    }
}

/* ---------------- R1 / R3 / R7 / R8：拉起、停止、幂等 ---------------- */

#[test]
fn r1_r3_start_and_stop_real_process() {
    let root = tmp_root("start-stop");
    let st = state(&root);
    install(&st, "fakemod", &long_running_cmd(30), None);

    // R1：拉起后应为 running 且有 PID
    let v = start_module_inner(&st, "fakemod").expect("启动应成功");
    assert_eq!(v.run, "running", "无健康端点时进程存活即视为 running");
    assert!(v.pid.is_some(), "运行中应有 PID");

    // R3：停止后 PID 消失
    let s = st.processes().stop("fakemod");
    assert_eq!(s.run, "stopped");
    assert_eq!(s.pid, None, "停止后不应再有 PID");
    assert!(!st.processes().is_active("fakemod"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r7_start_is_idempotent() {
    let root = tmp_root("idem-start");
    let st = state(&root);
    install(&st, "fakemod", &long_running_cmd(30), None);

    let a = start_module_inner(&st, "fakemod").unwrap();
    let b = start_module_inner(&st, "fakemod").unwrap();
    assert_eq!(a.pid, b.pid, "重复启动不应产生第二个进程");

    st.processes().stop("fakemod");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r8_stop_is_idempotent() {
    let root = tmp_root("idem-stop");
    let st = state(&root);
    // 从未启动过
    let v = st.processes().stop("never");
    assert_eq!(v.run, "stopped");
    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- R4：进程退出后不显示"在跑" ---------------- */

#[test]
fn r4_exited_process_reports_failed() {
    let root = tmp_root("exited");
    let st = state(&root);
    install(&st, "quickexit", &quick_exit_cmd(7), None);

    // 无健康端点：启动瞬间记为 running
    let v = start_module_inner(&st, "quickexit").unwrap();
    assert_eq!(v.run, "running");

    // 等它退出后，快照必须回落，不能继续显示 running
    std::thread::sleep(std::time::Duration::from_millis(800));
    let after = st.processes().view("quickexit");
    assert_ne!(after.run, "running", "已退出进程不得显示为运行中");
    assert_eq!(after.run, "failed");
    assert!(after.pid.is_none(), "退出后 PID 应回收");

    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- R2：健康检查失败 → failed ---------------- */

#[test]
fn r2_health_timeout_marks_failed() {
    let root = tmp_root("healthfail");
    let st = state(&root);
    // 指向必然无服务的端口
    install(
        &st,
        "badhealth",
        &long_running_cmd(20),
        Some("http://127.0.0.1:1/healthz"),
    );

    let err = start_module_inner(&st, "badhealth").unwrap_err();
    assert!(
        err.contains("健康检查") || err.contains("退出"),
        "应给出健康检查失败原因，实际：{err}"
    );

    let v = st.processes().view("badhealth");
    assert_eq!(v.run, "failed", "健康检查失败后应为 failed");
    assert!(v.last_error.is_some(), "应带有可展示的失败原因");

    st.processes().stop("badhealth");
    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- R5：前置校验拒绝且原因可读 ---------------- */

#[test]
fn r5_rejects_not_installed() {
    let root = tmp_root("notinstalled");
    let st = state(&root);
    let err = start_module_inner(&st, "ghost").unwrap_err();
    assert!(err.contains("未安装"), "实际：{err}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r5_rejects_disabled_module() {
    let root = tmp_root("disabled");
    let st = state(&root);
    install_as(&st, "opt", &long_running_cmd(20), None, "optional");

    // 停用。必须走 AppState 的接口：状态文件在构造时已读入内存，
    // 之后再改磁盘文件是看不到的（会出现"文件改了但行为没变"的假象）。
    st.set_state_for_test("opt", "1.0.0", Category::Optional, Some(false))
        .unwrap();

    let err = start_module_inner(&st, "opt").unwrap_err();
    assert!(err.contains("已停用"), "实际：{err}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r5_rejects_non_command_entry() {
    let root = tmp_root("notcmd");
    let st = state(&root);

    // 手工造一个 entry.type = plugin 的模块
    let manifest = json!({
        "module_id": "pluginmod",
        "version": "1.0.0",
        "entry": { "type": "plugin", "command": "python -m x" },
        "dependencies": [], "model_packs": [],
        "signature": { "algo": "SHA-256 + Ed25519", "value": "" }
    });
    let mut buf = Vec::new();
    {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        w.start_file("manifest.json", opts).unwrap();
        w.write_all(manifest.to_string().as_bytes()).unwrap();
        w.finish().unwrap();
    }
    let hash = dounai_launcher::sha256_hex(&buf);
    install_module_inner(
        &st,
        InstallRequest {
            module_id: "pluginmod".into(),
            category: "core".into(),
            version: "1.0.0".into(),
            zip_base64: B64.encode(&buf),
            expected_sha256: hash,
            manifest: json!({ "module_id": "pluginmod" }),
        },
    )
    .unwrap();

    let err = start_module_inner(&st, "pluginmod").unwrap_err();
    assert!(err.contains("不支持作为进程启动"), "实际：{err}");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r5_rejects_missing_manifest() {
    let root = tmp_root("nomanifest");
    let st = state(&root);

    // 安装一个不含 manifest.json 的包
    let mut buf = Vec::new();
    {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        w.start_file("readme.txt", opts).unwrap();
        w.write_all(b"hi").unwrap();
        w.finish().unwrap();
    }
    let hash = dounai_launcher::sha256_hex(&buf);
    install_module_inner(
        &st,
        InstallRequest {
            module_id: "bare".into(),
            category: "core".into(),
            version: "1.0.0".into(),
            zip_base64: B64.encode(&buf),
            expected_sha256: hash,
            manifest: json!({ "module_id": "bare" }),
        },
    )
    .unwrap();

    let err = start_module_inner(&st, "bare").unwrap_err();
    assert!(err.contains("manifest"), "实际：{err}");
    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- R6：一键启动顺序与失败不阻塞 ---------------- */

#[test]
fn r6_start_all_follows_order_and_continues_on_failure() {
    let root = tmp_root("startall");
    let st = state(&root);

    // 正常模块
    install(&st, "dounai-input", &long_running_cmd(30), None);
    install(&st, "omninexus", &long_running_cmd(30), None);
    // 必定失败的模块（健康端点不存在），验证不阻塞其余
    install(
        &st,
        "moodvoice",
        &long_running_cmd(20),
        Some("http://127.0.0.1:1/healthz"),
    );

    let results = start_all_inner(&st);
    let ids: Vec<&str> = results.iter().map(|v| v.module_id.as_str()).collect();

    // 顺序符合固定清单（输入层 → 模型层 → 语音层）
    assert_eq!(ids, vec!["dounai-input", "omninexus", "moodvoice"]);

    // 前两个成功，第三个失败但不影响前两个
    let by_id: std::collections::HashMap<&str, &str> = results
        .iter()
        .map(|v| (v.module_id.as_str(), v.run.as_str()))
        .collect();
    assert_eq!(by_id["dounai-input"], "running");
    assert_eq!(by_id["omninexus"], "running");
    assert_eq!(by_id["moodvoice"], "failed", "失败模块应单独标记 failed");

    st.processes().stop_all();
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn r6_start_all_skips_reserved_and_disabled() {
    let root = tmp_root("skip");
    let st = state(&root);
    install(&st, "normal", &long_running_cmd(20), None);

    // reserved：必须按 reserved 分类安装，装完自动禁用，不应参与启动。
    // 若按 core 安装，`mark_installed` 会把 enabled 置 true，
    // 等于绕过分类约束——这正是本用例要防的。
    install_as(
        &st,
        "dounai-memory-reserved",
        &long_running_cmd(20),
        None,
        "reserved",
    );

    let ids: Vec<String> = dounai_launcher::commands::start_order(&st);
    assert_eq!(ids, vec!["normal"], "reserved 与禁用的模块不应进入启动顺序");

    std::fs::remove_dir_all(&root).ok();
}

/* ---------------- R9：工作目录越界拒绝 ---------------- */

#[test]
fn r9_rejects_working_dir_escape() {
    use dounai_launcher::runtime::{check_working_dir, StartError};
    let root = Path::new("C:\\app\\modules\\m");
    assert!(matches!(
        check_working_dir(root, "..\\..\\Windows\\System32"),
        Err(StartError::UnsafeWorkingDir(_))
    ));
}

#[test]
fn r9_rejects_shell_meta_in_command() {
    use dounai_launcher::runtime::{split_command, StartError};
    // manifest 若被篡改成带管道/重定向，必须在解析阶段就拒绝
    for bad in ["cmd /c a | b", "python x && evil", "a > out.txt"] {
        assert!(
            matches!(split_command(bad), Err(StartError::ShellMeta(_))),
            "应拒绝 {bad}"
        );
    }
}
