#![windows_subsystem = "windows"]

mod bridge;
mod server;
mod storage;
mod telemetry;

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::windows::WindowExtWindows;
use tao::window::{Icon as WindowIcon, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon as TrayIconImage, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenEventW, OpenProcessToken, SetEvent,
    WaitForSingleObject, EVENT_MODIFY_STATE, SYNCHRONIZATION_ACCESS_RIGHTS,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, MSGFLT_ALLOW, SW_SHOWNORMAL, WM_DROPFILES,
};
use wry::{NewWindowResponse, PermissionKind, PermissionResponse, WebContext, WebViewBuilder};

const ICON_BYTES: &[u8] = include_bytes!(r"G:\Strata-3060\icons\am2strata.ico");

#[derive(Debug)]
enum UserEvent {
    WakeRequested,
    ServerReady,
    SplashUpdate {
        logs: Vec<String>,
        message: Option<String>,
        failed: bool,
    },
    HealthRefreshed(Option<server::HealthResponse>),
    ModelSwitched(Result<String, String>),
    UpdatesReport(Result<String, String>),
    DownloadFinished {
        filename: String,
        path: Option<String>,
        success: bool,
    },
    TrayAction(MenuId),
    TrayClicked,
    HotKeyTriggered,
    TelemetryUpdated(String),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum IpcMessage {
    #[serde(rename = "sync_storage")]
    SyncStorage { key: String, value: Option<String> },
    #[serde(rename = "check_updates")]
    CheckUpdates,
    #[serde(rename = "open_path")]
    OpenPath { path: String },
    #[serde(rename = "retry_start")]
    RetryStart,
    #[serde(rename = "switch_model")]
    SwitchModel { model: String },
    #[serde(rename = "request_telemetry")]
    RequestTelemetry,
}

fn parse_model_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--model" {
            return args.next();
        }
        if let Some(rest) = arg.strip_prefix("--model=") {
            return Some(rest.to_string());
        }
    }
    None
}

fn load_rgba_icon() -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(ICON_BYTES)
        .expect("embedded am2strata.ico must be valid")
        .into_rgba8();
    let (w, h) = img.dimensions();
    (img.into_raw(), w, h)
}

fn enable_dark_title_bar(hwnd: isize) {
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

fn is_process_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret_len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

fn allow_uipi_drag_drop(hwnd: isize) {
    const WM_COPYGLOBALDATA: u32 = 0x0049;
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, None);
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYGLOBALDATA, MSGFLT_ALLOW, None);
    }
}

fn relaunch_as_admin(hwnd: isize) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let args: String = std::env::args()
        .skip(1)
        .map(|a| {
            if a.contains(' ') {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut exe_w: Vec<u16> = exe.to_string_lossy().encode_utf16().collect();
    exe_w.push(0);
    let mut args_w: Vec<u16> = args.encode_utf16().collect();
    args_w.push(0);
    let hinst = unsafe {
        ShellExecuteW(
            Some(HWND(hwnd as *mut _)),
            w!("runas"),
            PCWSTR(exe_w.as_ptr()),
            if args.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(args_w.as_ptr())
            },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    (hinst.0 as isize) > 32
}

fn downloads_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        let dl = PathBuf::from(profile).join("Downloads");
        if dl.exists() {
            return dl;
        }
    }
    PathBuf::from(server::BASE_DIR)
}

fn unique_download_path(base_dir: &PathBuf, suggested: &PathBuf) -> PathBuf {
    let file_name = suggested
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("am2strata-download");
    let candidate = base_dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = PathBuf::from(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("download")
        .to_string();
    let ext = PathBuf::from(file_name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for i in 1..1000 {
        let next = base_dir.join(format!("{stem} ({i}){ext}"));
        if !next.exists() {
            return next;
        }
    }
    candidate
}

fn main() {
    let requested_model = parse_model_arg();
    let elevated = is_process_elevated();

    // Single-instance check: if already running, pass any --model request and wake existing window
    let mut mutex_handle: Option<isize> = unsafe {
        let mutex = CreateMutexW(None, false, w!("Local\\am2strata-desktop-instance"));
        if let Ok(handle) = mutex {
            if GetLastError() == ERROR_ALREADY_EXISTS {
                storage::write_pending_model(requested_model.as_deref());
                if let Ok(evt) = OpenEventW(
                    SYNCHRONIZATION_ACCESS_RIGHTS(EVENT_MODIFY_STATE.0),
                    false,
                    w!("Local\\am2strata-desktop-wake"),
                ) {
                    let _ = SetEvent(evt);
                    let _ = CloseHandle(evt);
                }
                let _ = CloseHandle(handle);
                return;
            }
            Some(handle.0 as isize)
        } else {
            None
        }
    };

    let wake_event = unsafe { CreateEventW(None, false, false, w!("Local\\am2strata-desktop-wake")).ok() };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Forward external wake signals into the tao event loop
    if let Some(evt) = wake_event {
        let raw_evt = evt.0 as isize;
        let wake_proxy = proxy.clone();
        std::thread::spawn(move || loop {
            let handle = HWND(raw_evt as *mut _);
            let res = unsafe { WaitForSingleObject(windows::Win32::Foundation::HANDLE(handle.0), 500) };
            if res == WAIT_OBJECT_0 {
                let _ = wake_proxy.send_event(UserEvent::WakeRequested);
            }
        });
    }

    // Forward tray & menu & hotkey events into tao event loop
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            let _ = p.send_event(UserEvent::TrayAction(ev.id));
        }));
    }
    {
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } = ev
            {
                let _ = p.send_event(UserEvent::TrayClicked);
            }
        }));
    }
    {
        let p = proxy.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
            if ev.state == HotKeyState::Pressed {
                let _ = p.send_event(UserEvent::HotKeyTriggered);
            }
        }));
    }

    let (icon_rgba, icon_w, icon_h) = load_rgba_icon();
    let win_icon = WindowIcon::from_rgba(icon_rgba.clone(), icon_w, icon_h).ok();
    let tray_icon_img = TrayIconImage::from_rgba(icon_rgba, icon_w, icon_h)
        .expect("tray icon image must be valid");

    let saved_win = storage::load_window_state();
    let win_title = if elevated {
        "am2Strata (Administrator)"
    } else {
        "am2Strata"
    };
    let window = WindowBuilder::new()
        .with_title(win_title)
        .with_window_icon(win_icon)
        .with_inner_size(LogicalSize::new(saved_win.width, saved_win.height))
        .with_position(LogicalPosition::new(saved_win.x, saved_win.y))
        .with_min_inner_size(LogicalSize::new(900, 620))
        .with_maximized(saved_win.maximized)
        .with_visible(true)
        .build(&event_loop)
        .expect("failed to build main window");

    enable_dark_title_bar(window.hwnd());
    if elevated {
        allow_uipi_drag_drop(window.hwnd());
    }

    // Register global hotkey Alt+Space to toggle am2Strata from anywhere
    let hotkey_manager = GlobalHotKeyManager::new().ok();
    let toggle_hotkey = HotKey::new(Some(Modifiers::ALT), Code::Space);
    if let Some(ref mgr) = hotkey_manager {
        let _ = mgr.register(toggle_hotkey);
    }

    // Build System Tray menu
    let configs = server::discover_configs();
    let tray_menu = Menu::new();
    let item_show = MenuItem::new("Open am2Strata (Alt+Space)", true, None);
    let item_status = MenuItem::new("Status: Checking server…", false, None);
    let models_submenu = Submenu::new("Switch Model", true);
    let mut model_menu_map: HashMap<MenuId, String> = HashMap::new();
    let mut model_check_items: Vec<(String, CheckMenuItem)> = Vec::new();

    for cfg in &configs {
        let chk = CheckMenuItem::new(&cfg.display_name, true, false, None);
        let _ = models_submenu.append(&chk);
        model_menu_map.insert(chk.id().clone(), cfg.model_name.clone());
        model_check_items.push((cfg.model_name.clone(), chk));
    }

    let item_unload = MenuItem::new("Unload Model (Free VRAM)", true, None);
    let item_updates = MenuItem::new("Check GitHub Updates (Ctrl+Shift+U)", true, None);
    let item_open_media = MenuItem::new("Open Media & Data Folder", true, None);
    let item_open_browser = MenuItem::new("Open Web UI in Browser", true, None);
    let item_elevate = if elevated {
        MenuItem::new("Running as Administrator ✓", false, None)
    } else {
        MenuItem::new("Restart as Administrator (UAC)", true, None)
    };
    let item_restart_srv = MenuItem::new("Restart AI Server", true, None);
    let item_stop_srv = MenuItem::new("Stop Server & Free VRAM", true, None);
    let item_quit_keep = MenuItem::new("Quit Desktop App (Keep Server)", true, None);
    let item_quit_stop = MenuItem::new("Quit & Stop Server (Free VRAM)", true, None);

    let _ = tray_menu.append_items(&[
        &item_show,
        &item_status,
        &PredefinedMenuItem::separator(),
        &models_submenu,
        &item_unload,
        &PredefinedMenuItem::separator(),
        &item_updates,
        &item_open_media,
        &item_open_browser,
        &item_elevate,
        &PredefinedMenuItem::separator(),
        &item_restart_srv,
        &item_stop_srv,
        &PredefinedMenuItem::separator(),
        &item_quit_keep,
        &item_quit_stop,
    ]);

    let initial_tip = if elevated {
        "am2Strata Local AI (127.0.0.1:8088) [Admin]"
    } else {
        "am2Strata Local AI (127.0.0.1:8088)"
    };
    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip(initial_tip)
        .with_icon(tray_icon_img)
        .build()
        .expect("failed to build tray icon");

    // Persistent WebView2 profile on G:\
    let userdata_dir = PathBuf::from(server::AM2_DIR).join("desktop-userdata");
    let _ = std::fs::create_dir_all(&userdata_dir);
    let mut web_context = WebContext::new(Some(userdata_dir));

    let tel_proxy = proxy.clone();
    let telemetry_state = telemetry::spawn_sampler(move |json| {
        let _ = tel_proxy.send_event(UserEvent::TelemetryUpdated(json));
    });
    let initial_telemetry = telemetry_state.lock().unwrap().clone();

    let initial_storage = storage::load_synced_storage();
    let init_js = bridge::build_init_script(&initial_storage, &initial_telemetry);

    let server_already_up = server::get_health().is_some();
    let initial_model_label = requested_model
        .clone()
        .or_else(|| configs.first().map(|c| c.model_name.clone()))
        .unwrap_or_else(|| server::DEFAULT_MODEL.to_string());

    let ipc_proxy = proxy.clone();
    let ipc_telemetry = Arc::clone(&telemetry_state);
    let dl_proxy = proxy.clone();
    let dl_root = downloads_dir();

    // Note: intentionally NO .with_drag_drop_handler(...) so WebView2 keeps AllowExternalDrop=true
    // and native HTML5 drag-and-drop (pictures, PDFs, videos, audio, code) works identically to the browser.
    let mut builder = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_background_color((15, 19, 24, 255))
        .with_autoplay(true)
        .with_clipboard(true)
        .with_devtools(true)
        .with_initialization_script(&init_js)
        .with_permission_handler(|kind| match kind {
            PermissionKind::Microphone
            | PermissionKind::ClipboardRead
            | PermissionKind::Notifications
            | PermissionKind::AutomaticDownloads
            | PermissionKind::Autoplay
            | PermissionKind::LocalFonts => PermissionResponse::Allow,
            _ => PermissionResponse::Default,
        })
        .with_new_window_req_handler(|url, _features| {
            if url.starts_with("http://127.0.0.1:8088") || url.starts_with("blob:") {
                NewWindowResponse::Allow
            } else {
                let _ = open::that(&url);
                NewWindowResponse::Deny
            }
        })
        .with_download_started_handler(move |_url, dest| {
            *dest = unique_download_path(&dl_root, dest);
            true
        })
        .with_download_completed_handler(move |_url, path, success| {
            let path_str = path.as_ref().map(|p| p.to_string_lossy().to_string());
            let filename = path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string();
            let _ = dl_proxy.send_event(UserEvent::DownloadFinished {
                filename,
                path: path_str,
                success,
            });
        })
        .with_ipc_handler(move |req| {
            let body = req.body();
            let Ok(msg) = serde_json::from_str::<IpcMessage>(body) else {
                return;
            };
            match msg {
                IpcMessage::SyncStorage { key, value } => {
                    storage::update_synced_storage_key(&key, value.as_deref());
                }
                IpcMessage::CheckUpdates => {
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let res = server::run_github_update_check();
                        let _ = p.send_event(UserEvent::UpdatesReport(res));
                    });
                }
                IpcMessage::OpenPath { path } => {
                    let p = PathBuf::from(&path);
                    let target = p.parent().unwrap_or(&p);
                    let _ = open::that(target);
                }
                IpcMessage::RetryStart => {
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let _ = server::start_server_headless(None);
                        wait_until_healthy_and_notify(p);
                    });
                }
                IpcMessage::SwitchModel { model } => {
                    let p = ipc_proxy.clone();
                    std::thread::spawn(move || {
                        let res = server::switch_model(&model);
                        let _ = p.send_event(UserEvent::ModelSwitched(res));
                    });
                }
                IpcMessage::RequestTelemetry => {
                    if let Ok(snap) = ipc_telemetry.lock() {
                        if let Ok(json) = serde_json::to_string(&*snap) {
                            let _ = ipc_proxy.send_event(UserEvent::TelemetryUpdated(json));
                        }
                    }
                }
            }
        });

    builder = if server_already_up {
        builder.with_url(server::SERVER_URL)
    } else {
        builder.with_html(bridge::splash_html(&initial_model_label))
    };

    let webview = builder
        .build(&window)
        .expect("failed to build WebView2 instance");

    // If server wasn't running yet, start it headlessly in the background and wait for /health
    if !server_already_up {
        let p = proxy.clone();
        let m = requested_model.clone();
        std::thread::spawn(move || {
            if let Err(err) = server::start_server_headless(m.as_deref()) {
                let _ = p.send_event(UserEvent::SplashUpdate {
                    logs: vec![err.clone()],
                    message: Some(err),
                    failed: true,
                });
                return;
            }
            wait_until_healthy_and_notify(p);
        });
    } else if let Some(ref req_model) = requested_model {
        // Server was already running, check if user launched a specific model shortcut
        let p = proxy.clone();
        let m = req_model.clone();
        std::thread::spawn(move || {
            let res = server::switch_model(&m);
            let _ = p.send_event(UserEvent::ModelSwitched(res));
        });
    }

    // Background health poller for live Tray status & active model checkmark
    {
        let p = proxy.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(4));
            let h = server::get_health();
            let _ = p.send_event(UserEvent::HealthRefreshed(h));
        });
    }

    let current_state = Arc::new(Mutex::new(saved_win));
    let mut loaded_main_ui = server_already_up;
    let mut last_win_save = Instant::now();

    let show_and_focus = |win: &tao::window::Window| {
        win.set_visible(true);
        win.set_minimized(false);
        win.set_focus();
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                // Initial health sync for tray
                let p = proxy.clone();
                std::thread::spawn(move || {
                    let h = server::get_health();
                    let _ = p.send_event(UserEvent::HealthRefreshed(h));
                });
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    // Minimize to tray to preserve active VRAM state and Linger/Claude Code gateway
                    window.set_visible(false);
                }
                WindowEvent::Resized(PhysicalSize { width, height }) => {
                    if let Ok(mut st) = current_state.lock() {
                        let scale = window.scale_factor();
                        let maximized = window.is_maximized();
                        st.maximized = maximized;
                        if !maximized && width > 400 && height > 300 {
                            st.width = (width as f64 / scale).round() as u32;
                            st.height = (height as f64 / scale).round() as u32;
                        }
                        if last_win_save.elapsed() > Duration::from_millis(500) {
                            storage::save_window_state(&st);
                            last_win_save = Instant::now();
                        }
                    }
                }
                WindowEvent::Moved(PhysicalPosition { x, y }) => {
                    if let Ok(mut st) = current_state.lock() {
                        if !window.is_maximized() && x > -3000 && y > -2000 {
                            let scale = window.scale_factor();
                            st.x = (x as f64 / scale).round() as i32;
                            st.y = (y as f64 / scale).round() as i32;
                        }
                    }
                }
                _ => {}
            },
            Event::UserEvent(ev) => match ev {
                UserEvent::WakeRequested => {
                    show_and_focus(&window);
                    if let Some(model) = storage::take_pending_model() {
                        let p = proxy.clone();
                        let _ = webview.evaluate_script(
                            "window.__desktopToast && window.__desktopToast('info','Switching model','Loading requested model into VRAM…')",
                        );
                        std::thread::spawn(move || {
                            let res = server::switch_model(&model);
                            let _ = p.send_event(UserEvent::ModelSwitched(res));
                        });
                    }
                }
                UserEvent::TrayClicked => {
                    if window.is_visible() && !window.is_minimized() {
                        window.set_visible(false);
                    } else {
                        show_and_focus(&window);
                    }
                }
                UserEvent::TelemetryUpdated(json) => {
                    if loaded_main_ui {
                        let js = format!(
                            "window.__setDesktopTelemetry && window.__setDesktopTelemetry({json});"
                        );
                        let _ = webview.evaluate_script(&js);
                    }
                }
                UserEvent::HotKeyTriggered => {
                    if window.is_visible() && !window.is_minimized() && window.is_focused() {
                        window.set_visible(false);
                    } else {
                        show_and_focus(&window);
                    }
                }
                UserEvent::SplashUpdate {
                    logs,
                    message,
                    failed,
                } => {
                    if !loaded_main_ui {
                        let logs_json = serde_json::to_string(&logs).unwrap_or_else(|_| "[]".into());
                        let msg_json = serde_json::to_string(&message).unwrap_or_else(|_| "null".into());
                        let js = format!("window.__setSplashLogs && window.__setSplashLogs({logs_json}, {msg_json}, {failed});");
                        let _ = webview.evaluate_script(&js);
                    }
                }
                UserEvent::ServerReady => {
                    loaded_main_ui = true;
                    let _ = webview.load_url(server::SERVER_URL);
                }
                UserEvent::HealthRefreshed(health) => {
                    match health {
                        Some(h) => {
                            let model = h.model.unwrap_or_else(|| "Ready".to_string());
                            let loaded = h.loaded.unwrap_or(false);
                            let caps = match (h.images.unwrap_or(false), h.video.unwrap_or(false), h.image_generation.unwrap_or(false)) {
                                (true, true, true) => " · Vision + Video + Media",
                                (true, _, true) => " · Vision + Media",
                                (true, _, _) => " · Vision",
                                _ => "",
                            };
                            let alert_note = match h.alerts.unwrap_or(0) {
                                0 => String::new(),
                                n => format!(" ({n} alert(s))"),
                            };
                            let srv = h.server.as_deref().unwrap_or("am2Strata");
                            let status_ok = h.status.as_deref().unwrap_or("ok");
                            let admin_tag = if elevated { " [Admin]" } else { "" };
                            let label = if loaded {
                                format!("Active: {model}{admin_tag}")
                            } else {
                                format!("Standby ({status_ok}): {model}{admin_tag}")
                            };
                            item_status.set_text(&label);
                            let tip = format!("{srv} ({}){admin_tag}{alert_note}\n{label}{caps}", server::SERVER_URL);
                            let _ = tray_icon.set_tooltip(Some(tip));
                            for (cfg_id, chk) in &model_check_items {
                                let is_match = model.to_lowercase().contains(&cfg_id.to_lowercase())
                                    || cfg_id
                                        .split('-')
                                        .take(2)
                                        .all(|tok| model.to_lowercase().contains(tok));
                                chk.set_checked(loaded && is_match);
                            }
                            if !loaded_main_ui {
                                loaded_main_ui = true;
                                let _ = webview.load_url(server::SERVER_URL);
                            }
                        }
                        None => {
                            item_status.set_text("Status: Server stopped (VRAM free)");
                            let _ = tray_icon.set_tooltip(Some("am2Strata — Server stopped"));
                            for (_, chk) in &model_check_items {
                                chk.set_checked(false);
                            }
                        }
                    }
                }
                UserEvent::ModelSwitched(res) => {
                    match res {
                        Ok(msg) => {
                            let msg_js = serde_json::to_string(&msg).unwrap_or_default();
                            let _ = webview.evaluate_script(&format!(
                                "window.__desktopToast && window.__desktopToast('success', 'Model ready', {msg_js}); if (typeof loadHealth==='function') loadHealth();"
                            ));
                        }
                        Err(err) => {
                            let err_js = serde_json::to_string(&err).unwrap_or_default();
                            let _ = webview.evaluate_script(&format!(
                                "window.__desktopToast && window.__desktopToast('error', 'Model switch failed', {err_js});"
                            ));
                        }
                    }
                    let p = proxy.clone();
                    std::thread::spawn(move || {
                        let _ = p.send_event(UserEvent::HealthRefreshed(server::get_health()));
                    });
                }
                UserEvent::UpdatesReport(res) => {
                    show_and_focus(&window);
                    let text = match res {
                        Ok(r) => r,
                        Err(e) => format!("Update check error:\n{e}"),
                    };
                    let json_text = serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into());
                    let _ = webview.evaluate_script(&format!(
                        "window.__showUpdatesModal && window.__showUpdatesModal({json_text});"
                    ));
                }
                UserEvent::DownloadFinished {
                    filename,
                    path,
                    success,
                } => {
                    let fname_js = serde_json::to_string(&filename).unwrap_or_else(|_| "\"file\"".into());
                    let path_js = serde_json::to_string(&path).unwrap_or_else(|_| "null".into());
                    if success {
                        let _ = webview.evaluate_script(&format!(
                            "window.__desktopToast && window.__desktopToast('success', 'Saved to Downloads', {fname_js}, {path_js});"
                        ));
                    } else {
                        let _ = webview.evaluate_script(&format!(
                            "window.__desktopToast && window.__desktopToast('error', 'Download failed', {fname_js});"
                        ));
                    }
                }
                UserEvent::TrayAction(id) => {
                    if id == *item_show.id() {
                        show_and_focus(&window);
                    } else if let Some(model_id) = model_menu_map.get(&id).cloned() {
                        show_and_focus(&window);
                        let _ = webview.evaluate_script(
                            "window.__desktopToast && window.__desktopToast('info', 'Switching model', 'Loading selected model into VRAM…');",
                        );
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            let res = server::switch_model(&model_id);
                            let _ = p.send_event(UserEvent::ModelSwitched(res));
                        });
                    } else if id == *item_unload.id() {
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            let res = server::unload_model();
                            let _ = p.send_event(UserEvent::ModelSwitched(res));
                        });
                    } else if id == *item_updates.id() {
                        show_and_focus(&window);
                        let _ = webview.evaluate_script(
                            "window.__showUpdatesModal && window.__showUpdatesModal('Checking GitHub for updates…');",
                        );
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            let res = server::run_github_update_check();
                            let _ = p.send_event(UserEvent::UpdatesReport(res));
                        });
                    } else if id == *item_open_media.id() {
                        let _ = open::that(server::BASE_DIR);
                    } else if id == *item_open_browser.id() {
                        let _ = open::that(server::SERVER_URL);
                    } else if id == *item_elevate.id() && !elevated {
                        if let Ok(st) = current_state.lock() {
                            storage::save_window_state(&st);
                        }
                        if let Some(raw) = mutex_handle.take() {
                            unsafe {
                                let _ = CloseHandle(HANDLE(raw as *mut _));
                            }
                        }
                        server::stop_server_processes();
                        if relaunch_as_admin(window.hwnd()) {
                            *control_flow = ControlFlow::Exit;
                        } else {
                            let _ = webview.evaluate_script(
                                "window.__desktopToast && window.__desktopToast('warn', 'Elevation cancelled', 'Restarting standard AI server…');",
                            );
                            let p = proxy.clone();
                            std::thread::spawn(move || {
                                let _ = server::start_server_headless(None);
                                wait_until_healthy_and_notify(p);
                            });
                        }
                    } else if id == *item_restart_srv.id() {
                        loaded_main_ui = false;
                        show_and_focus(&window);
                        let _ = webview.load_html(&bridge::splash_html("Restarting am2strata…"));
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            server::stop_server_processes();
                            std::thread::sleep(Duration::from_millis(600));
                            if let Err(err) = server::start_server_headless(None) {
                                let _ = p.send_event(UserEvent::SplashUpdate {
                                    logs: vec![err.clone()],
                                    message: Some(err),
                                    failed: true,
                                });
                                return;
                            }
                            wait_until_healthy_and_notify(p);
                        });
                    } else if id == *item_stop_srv.id() {
                        std::thread::spawn(|| server::stop_server_processes());
                        item_status.set_text("Status: Server stopped (VRAM free)");
                        let _ = webview.evaluate_script(
                            "window.__desktopToast && window.__desktopToast('warn', 'Server stopped', 'All engine processes terminated and VRAM freed.');",
                        );
                    } else if id == *item_quit_keep.id() {
                        if let Ok(st) = current_state.lock() {
                            storage::save_window_state(&st);
                        }
                        *control_flow = ControlFlow::Exit;
                    } else if id == *item_quit_stop.id() {
                        if let Ok(st) = current_state.lock() {
                            storage::save_window_state(&st);
                        }
                        server::stop_server_processes();
                        *control_flow = ControlFlow::Exit;
                    }
                }
            },
            _ => {}
        }
    });
}

fn wait_until_healthy_and_notify(proxy: tao::event_loop::EventLoopProxy<UserEvent>) {
    let start = Instant::now();
    let timeout = Duration::from_secs(180);
    while start.elapsed() < timeout {
        if server::get_health().is_some() {
            let _ = proxy.send_event(UserEvent::ServerReady);
            return;
        }
        let logs = server::latest_log_tail(14);
        let _ = proxy.send_event(UserEvent::SplashUpdate {
            logs,
            message: None,
            failed: false,
        });
        std::thread::sleep(Duration::from_millis(750));
    }
    let _ = proxy.send_event(UserEvent::SplashUpdate {
        logs: server::latest_log_tail(20),
        message: Some("Timed out waiting for am2strata.exe on port 8088.".to_string()),
        failed: true,
    });
}
