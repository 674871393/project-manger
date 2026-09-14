mod commands;
mod process_manager;
mod release;
mod settings;

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    // 单实例：再次启动 exe 不开新进程，唤起已有实例的窗口
    // （可能藏在托盘里，show + unminimize + focus 一步到位）。
    // 注意：该插件必须最先注册，否则互斥锁建立前 UI 已起，拦截不住。
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(
        |app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = (w.show(), w.unminimize(), w.set_focus());
            }
        },
    ));

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(process_manager::ProcessManager::default())
        .invoke_handler(tauri::generate_handler![
            commands::load_projects,
            commands::save_projects,
            commands::inspect_project,
            commands::read_git_branch,
            commands::scan_projects,
            commands::check_deps,
            commands::read_log_tail,
            release::read_release_context,
            release::start_release,
            process_manager::start_project,
            process_manager::stop_project,
            process_manager::install_project,
            process_manager::stop_install,
            process_manager::probe_port,
            process_manager::get_lan_ip,
            process_manager::get_running,
            process_manager::open_in_vscode,
            settings::load_settings,
            settings::save_settings,
        ])
        .setup(|app| {
            // 上次崩溃残留的孤儿进程清理（PRD 7）
            process_manager::cleanup_orphans(app.handle());

            let window = app.get_webview_window("main").unwrap();

            // 系统托盘：设置「最小化到托盘」后唯一的恢复入口。
            // 左键单击 = 显示窗口；右键菜单 = 显示主界面 / 退出。
            let show = tauri::menu::MenuItem::with_id(
                app,
                "show",
                "显示主界面",
                true,
                None::<&str>,
            )?;
            let quit = tauri::menu::MenuItem::with_id(
                app,
                "quit",
                "退出",
                true,
                None::<&str>,
            )?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;

            let tray = tauri::tray::TrayIconBuilder::with_id("main-tray")
                .tooltip("前端项目管理器")
                .icon(app.default_window_icon().unwrap().clone())
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let _ = tray
                            .app_handle()
                            .get_webview_window("main")
                            .map(|w| (w.show(), w.unminimize(), w.set_focus()));
                    }
                })
                .menu(&menu)                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        let _ = app
                            .get_webview_window("main")
                            .map(|w| (w.show(), w.unminimize(), w.set_focus()));
                    }
                    "quit" => {
                        // 走 app.exit：触发 RunEvent::ExitRequested → stop_all 清理进程树
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            tray.set_visible(true)?;

            // 窗口关闭拦截：按设置决定「真退出」还是「隐藏到托盘」。
            // false / Err（读不到设置按默认）：不拦截，走正常退出。
            let handle = app.handle().clone();
            let window_for_close = window.clone();
            window.on_window_event(move |e| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                    let minimize_on_close =
                        settings::load_settings_inner(&handle).map(|s| s.minimize_on_close);
                    if minimize_on_close == Ok(true) {
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            // 应用退出：杀掉所有运行中的项目进程树，不留孤儿（PRD 6.3 / 验收 7）
            if let tauri::RunEvent::ExitRequested { .. } = event {
                app.state::<process_manager::ProcessManager>()
                    .stop_all(app);
            }
        });
}
