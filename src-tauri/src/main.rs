//! 启动器可执行入口。
//!
//! 只负责装配：双根路径、v2 状态、生命周期 Supervisor 与命令注册。
//! 所有业务逻辑都在 `dounai_launcher` 库里，便于独立测试。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use tauri::Manager;

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }))
        // 只提供服务端文件选择器：拿到路径后由 install_local_package 自己读文件。
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 双根：安装根是只读的程序目录，数据根是用户本地应用数据目录。
            // 两者必须不同，Paths::new 会校验，避免把用户数据写进 Program Files。
            let install_root = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
                .ok_or_else(|| "无法确定安装目录".to_string())?;
            let data_root: PathBuf = app
                .path()
                .app_local_data_dir()
                .map_err(|e| format!("无法确定数据目录：{e}"))?
                .join("launcher");

            let state = dounai_launcher::state_v2::AppStateV2::new(install_root, data_root)
                .map_err(|e| format!("初始化 v2 状态失败：{e}"))?;
            app.manage(state);

            let supervisor =
                dounai_launcher::lifecycle_runtime::Supervisor::new(app.handle().clone())
                    .map_err(|e| format!("初始化生命周期失败：{e}"))?;
            app.manage(supervisor);

            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // v1 命令名与返回形状原样保留，内部改走 v2；前端 bridge.ts 不用改。
            dounai_launcher::commands_v2::list_module_states,
            dounai_launcher::commands_v2::set_module_enabled,
            dounai_launcher::commands_v2::uninstall_module,
            dounai_launcher::commands_v2::install_module,
            dounai_launcher::commands_v2::inspect_local_package,
            dounai_launcher::commands_v2::install_local_package,
            dounai_launcher::commands_v2::download_package,
            dounai_launcher::commands_v2::refresh_catalog,
            dounai_launcher::commands_v2::trust_info,
            dounai_launcher::commands_v2::settings_snapshot,
            dounai_launcher::commands_v2::hash_file,
            dounai_launcher::commands_v2::purge_local_data,
            dounai_launcher::commands_v2::read_module_manifests,
            dounai_launcher::commands_v2::open_admin_window,
            dounai_launcher::commands_v2::start_module,
            dounai_launcher::commands_v2::stop_module,
            dounai_launcher::commands_v2::start_all,
            dounai_launcher::commands_v2::stop_all,
            dounai_launcher::commands_v2::scan_local_modules,
            dounai_launcher::commands_v2::add_local_module,
            dounai_launcher::commands_v2::add_local_modules_bulk,
            dounai_launcher::commands_v2::remove_local_module,
            dounai_launcher::commands_v2::dev_mode_status,
            dounai_launcher::commands_v2::enable_dev_mode,
            dounai_launcher::env_probe::probe_environment,
            // v2 新增命令。
            dounai_launcher::commands_v2::list_instances,
            dounai_launcher::commands_v2::list_instance_views,
            dounai_launcher::commands_v2::restart_instance,
            dounai_launcher::commands_v2::switch_active,
            dounai_launcher::commands_v2::confirm_local_source,
            dounai_launcher::commands_v2::get_lifecycle_snapshot,
            dounai_launcher::commands_v2::read_instance_logs,
            dounai_launcher::commands_v2::prepare_instance_env,
            dounai_launcher::commands_v2::set_keep_versions,
            dounai_launcher::commands_v2::set_k5_pubkey,
            dounai_launcher::commands_v2::confirm_trusted_key,
            dounai_launcher::commands_v2::revoke_trusted_key,
        ])
        .build(tauri::generate_context!())
        .expect("启动器构建失败");

    // 退出时先停全部子进程；强杀启动器则由每个实例的 Job Object 兜底。
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::ExitRequested { .. }) {
            if let Some(supervisor) =
                handle.try_state::<dounai_launcher::lifecycle_runtime::Supervisor>()
            {
                supervisor.stop_all();
            }
        }
    });
}
