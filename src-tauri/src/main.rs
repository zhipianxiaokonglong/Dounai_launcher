//! 启动器可执行入口。
//!
//! 只负责装配：双根路径、v2 状态、生命周期 Supervisor 与命令注册。
//! 所有业务逻辑都在 `dounai_launcher` 库里，便于独立测试。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};

use dounai_launcher::state_v2::CloseBehavior;

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
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            let tauri::WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let Some(state) = window
                .app_handle()
                .try_state::<dounai_launcher::state_v2::AppStateV2>()
            else {
                return;
            };
            // 已经决定退出了（托盘菜单、或用户在关窗提示里选了停止并退出），
            // 这里不能再拦，否则自己的退出路径被自己的拦截挡掉。
            if state.is_quitting() {
                return;
            }
            match state.close_behavior() {
                CloseBehavior::Quit => {
                    state.begin_quit();
                }
                // 托盘没建成时，「收进托盘」等于把窗口藏进没有入口的地方。
                CloseBehavior::Tray if !state.tray_ready() => {
                    state.begin_quit();
                }
                CloseBehavior::Tray => {
                    api.prevent_close();
                    let _ = window.hide();
                }
                CloseBehavior::Ask => {
                    // 窗口先留着，等前端弹框回答。重复的关闭请求也照发，
                    // 界面按「是否已有弹框」自行去重——这样一来用户按 Esc
                    // 关掉弹框之后，再点 × 仍然能重新问一次。
                    api.prevent_close();
                    let _ = window.emit("close-requested", ());
                }
            }
        })
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
            // 老版本装出来的实例表没有 active，角色会一直跑备用端口，
            // 组装层连不上。启动时补一次，之后安装/卸载各自维护。
            let healed = dounai_launcher::commands_v2::heal_active_slots(&state);
            if !healed.is_empty() {
                eprintln!("已把 {} 补为所在角色的 active 实例", healed.join("、"));
            }
            app.manage(state);

            let supervisor =
                dounai_launcher::lifecycle_runtime::Supervisor::new(app.handle().clone())
                    .map_err(|e| format!("初始化生命周期失败：{e}"))?;
            app.manage(supervisor);

            // M7：2 秒一轮资源采样。线程只调 Supervisor 的方法，不另存句柄，
            // 没有实例运行时 `sample_once` 直接返回，这一轮不碰任何 Win32。
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(
                        dounai_launcher::usage::INTERVAL_MS,
                    ));
                    let Some(supervisor) =
                        handle.try_state::<dounai_launcher::lifecycle_runtime::Supervisor>()
                    else {
                        continue;
                    };
                    supervisor.sample_once();
                    // 外部实例没有事件可听，只能顺带探活。名单为空时这里直接返回。
                    if let Some(state) = handle.try_state::<dounai_launcher::state_v2::AppStateV2>()
                    {
                        dounai_launcher::commands_v2::refresh_externals(&state, &supervisor);
                    }
                });
            }

            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }
            // 托盘常驻：「收进托盘」这个默认行为靠它把窗口叫回来。
            if let Some(state) = app.try_state::<dounai_launcher::state_v2::AppStateV2>() {
                match setup_tray(app.handle()) {
                    Ok(()) => state.mark_tray_ready(),
                    Err(e) => eprintln!("[tray] 托盘创建失败，关窗将直接退出：{e}"),
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // v1 命令名与返回形状原样保留，内部改走 v2；前端 bridge.ts 不用改。
            dounai_launcher::commands_v2::set_module_enabled,
            dounai_launcher::commands_v2::uninstall_module,
            dounai_launcher::commands_v2::inspect_local_package,
            dounai_launcher::commands_v2::install_local_package,
            dounai_launcher::commands_v2::install_from_catalog,
            dounai_launcher::commands_v2::refresh_catalog,
            dounai_launcher::commands_v2::catalog_cache,
            dounai_launcher::commands_v2::trust_info,
            dounai_launcher::commands_v2::settings_snapshot,
            dounai_launcher::commands_v2::purge_local_data,
            dounai_launcher::commands_v2::read_module_manifests,
            dounai_launcher::commands_v2::open_admin_window,
            dounai_launcher::commands_v2::open_admin_embedded,
            dounai_launcher::commands_v2::admin_host_resize,
            dounai_launcher::commands_v2::admin_host_reload,
            dounai_launcher::commands_v2::admin_host_unmount,
            dounai_launcher::commands_v2::admin_host_visible,
            dounai_launcher::commands_v2::start_module,
            dounai_launcher::commands_v2::stop_module,
            dounai_launcher::commands_v2::start_all,
            dounai_launcher::commands_v2::stop_all,
            dounai_launcher::commands_v2::scan_local_modules,
            dounai_launcher::commands_v2::add_local_module,
            dounai_launcher::commands_v2::add_local_modules_bulk,
            dounai_launcher::commands_v2::preview_v1_manifest,
            dounai_launcher::commands_v2::convert_v1_manifest,
            dounai_launcher::commands_v2::remove_local_module,
            dounai_launcher::commands_v2::dev_mode_status,
            dounai_launcher::commands_v2::enable_dev_mode,
            dounai_launcher::env_probe::probe_environment,
            // v2 新增命令。
            dounai_launcher::commands_v2::list_instance_views,
            dounai_launcher::commands_v2::instance_usage,
            dounai_launcher::commands_v2::restart_instance,
            dounai_launcher::commands_v2::switch_active,
            dounai_launcher::commands_v2::confirm_local_source,
            dounai_launcher::commands_v2::read_instance_logs,
            dounai_launcher::commands_v2::list_instance_logs,
            dounai_launcher::commands_v2::open_instance_logs_dir,
            dounai_launcher::commands_v2::prepare_instance_env,
            dounai_launcher::commands_v2::set_keep_versions,
            dounai_launcher::commands_v2::set_child_output,
            // 关窗行为：收进托盘 / 停止并退出 / 每次询问。
            dounai_launcher::commands_v2::close_behavior,
            dounai_launcher::commands_v2::set_close_behavior,
            dounai_launcher::commands_v2::resolve_close,
            // 启动器自身 HTTP 客户端的代理：关 / 系统 / 手动。
            dounai_launcher::commands_v2::proxy_config,
            dounai_launcher::commands_v2::set_proxy_config,
            dounai_launcher::commands_v2::storage_facts,
            dounai_launcher::commands_v2::reclaim_storage,
            dounai_launcher::commands_v2::onboarding_state,
            dounai_launcher::commands_v2::complete_onboarding,
            dounai_launcher::commands_v2::reset_onboarding,
            dounai_launcher::commands_v2::confirm_trusted_key,
            dounai_launcher::commands_v2::revoke_trusted_key,
            // M8：下载中心与可配置源。
            dounai_launcher::commands_v2::list_sources,
            dounai_launcher::commands_v2::inspect_source,
            dounai_launcher::commands_v2::inspect_source_token,
            dounai_launcher::commands_v2::add_source,
            dounai_launcher::commands_v2::set_source_enabled,
            dounai_launcher::commands_v2::revoke_source,
            dounai_launcher::commands_v2::remove_source,
            // M6：链路。
            dounai_launcher::commands_v2::list_chains,
            dounai_launcher::commands_v2::save_chain,
            dounai_launcher::commands_v2::delete_chain,
            dounai_launcher::commands_v2::start_chain,
            dounai_launcher::commands_v2::stop_chain,
        ])
        .build(tauri::generate_context!())
        .expect("启动器构建失败");

    // 退出时先停全部子进程；强杀启动器则由每个实例的 Job Object 兜底。
    // 关窗默认不进这里——「收进托盘」在窗口事件里就被拦下了。
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

/// 建系统托盘。菜单只有两项：显示主窗口、退出。
///
/// 「退出」是唯一会真的停掉实例的入口：置 quitting 后走 `app.exit`，
/// 由 `RunEvent::ExitRequested` 里的 `stop_all` 收尾。
fn setup_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("Dounai 启动器")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "quit" => {
                if let Some(state) = app.try_state::<dounai_launcher::state_v2::AppStateV2>() {
                    state.begin_quit();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    // 图标直接内嵌：不依赖 `default_window_icon()` 有没有被解码出来，
    // 托盘图标缺失时 Windows 会留一个空白格，用户根本点不到。
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;
    builder = builder.icon(icon);
    builder.build(app)?;
    Ok(())
}

fn show_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
