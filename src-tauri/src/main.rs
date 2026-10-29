//! 启动器可执行入口。
//!
//! 只负责装配：初始化数据根、注册命令、创建窗口。
//! 所有业务逻辑都在 `dounai_launcher` 库里，便于独立测试。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use dounai_launcher::catalog::Allowlist;
use dounai_launcher::commands::AppState;
use dounai_launcher::trust::TrustPolicy;
use tauri::Manager;

/// 构造 manifest 信任策略。
///
/// 调试构建：允许未签名（仓库里的样例 manifest 尚未签名，便于开发）。
/// 发布构建：**拒绝未签名**。发布时可在此填入可信公钥，
/// 公钥来自 `docs/HANDOVER.md` 里约定的签名密钥移交流程。
fn build_trust_policy() -> TrustPolicy {
    #[cfg(debug_assertions)]
    {
        TrustPolicy::permissive()
    }
    #[cfg(not(debug_assertions))]
    {
        // 尚未配置公钥时，严格策略会拒绝一切安装（fail-closed）。
        // 这是刻意的：宁可装不上，也不能装来路不明的模块。
        TrustPolicy::strict(Vec::new())
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }))
        .setup(|app| {
            // 数据根：用户本地应用数据目录，不写入安装目录（避免权限问题）
            let data_root: PathBuf = app
                .path()
                .app_local_data_dir()
                .map_err(|e| format!("无法确定数据目录：{e}"))?
                .join("launcher");

            // 出网白名单：一期为空集，即完全不允许联网（最保守）。
            // 配置真实下载源时在此加入域名，并同步更新 docs/ALLOWLIST.md。
            let allowlist = Allowlist::new(Vec::<String>::new());

            let state = AppState::with_trust(data_root, allowlist, build_trust_policy())
                .map_err(|e| format!("初始化状态失败：{e}"))?;
            app.manage(state);

            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dounai_launcher::commands::list_module_states,
            dounai_launcher::commands::set_module_enabled,
            dounai_launcher::commands::uninstall_module,
            dounai_launcher::commands::install_module,
            dounai_launcher::commands::download_package,
            dounai_launcher::commands::refresh_catalog,
            dounai_launcher::commands::trust_info,
            dounai_launcher::commands::hash_file,
            dounai_launcher::commands::purge_local_data,
            dounai_launcher::commands::start_module,
            dounai_launcher::commands::stop_module,
            dounai_launcher::commands::start_all,
            dounai_launcher::commands::stop_all,
            dounai_launcher::commands::scan_local_modules,
            dounai_launcher::commands::add_local_module,
            dounai_launcher::commands::add_local_modules_bulk,
            dounai_launcher::commands::remove_local_module,
            dounai_launcher::commands::dev_mode_status,
            dounai_launcher::commands::enable_dev_mode,
            dounai_launcher::env_probe::probe_environment,
        ])
        .run(tauri::generate_context!())
        .expect("启动器启动失败");
}
