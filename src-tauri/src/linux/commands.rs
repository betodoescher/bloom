use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Window};

use crate::settings_file::{self, get_setting_str};
use crate::state::{self, SETTINGS_FILE_LOCK};
use crate::types::{AppInfo, AudioSessionInfo, IntRect, WifiStatus};

use super::host::{self, AudioDevices, BatteryDetails, BtDevice, DockExtra, TrayApp, WifiNetworkList};
use super::{self as shell};

#[derive(Deserialize)]
struct CustomFolder {
    name: String,
    path: String,
}

fn folder_icon(id: &str) -> &'static str {
    match id {
        "documents" => "folder-documents",
        "downloads" => "folder-download",
        "pictures" => "folder-pictures",
        "music" => "folder-music",
        "videos" => "folder-videos",
        "desktop" => "user-desktop",
        _ => "folder",
    }
}

fn hidden_drive_keys(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter_map(|token| {
            if token.len() == 1 && token.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic()) {
                Some(token.to_ascii_uppercase())
            } else if token.starts_with('/') {
                Some(token.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn config_file(app: &AppHandle, name: &str) -> Result<std::path::PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name))
}

#[tauri::command]
pub fn set_menu_open(open: bool, rect: Option<IntRect>) {
    state::MENU_IS_OPEN.store(open, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut slot) = state::MENU_RECT.lock() {
        *slot = rect;
    }
}

#[tauri::command]
pub async fn set_dock_hovered(hovered: bool) {
    state::DOCK_IS_HOVERED.store(hovered, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub async fn set_notch_hovered(_app: AppHandle, hovered: bool) {
    state::NOTCH_IS_HOVERED.store(hovered, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub async fn set_notch_visible(visible: bool) {
    state::NOTCH_IS_VISIBLE.store(visible, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub fn setup_shelf_drop(_app: AppHandle) {}

#[tauri::command]
pub async fn shelf_thumbnail(path: String) -> crate::shelf_preview::ShelfPreview {
    tauri::async_runtime::spawn_blocking(move || crate::shelf_preview::preview(&path))
        .await
        .unwrap_or(crate::shelf_preview::ShelfPreview {
            thumb: None,
            text: None,
            folder: false,
        })
}

#[tauri::command]
pub async fn shelf_open(path: String) -> Result<(), String> {
    host::run_ok("xdg-open", &[&path])
}

#[tauri::command]
pub async fn shelf_prune(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().filter(|path| std::path::Path::new(path).exists()).collect()
}

#[tauri::command]
pub fn shelf_drag_icon() -> Option<String> {
    None
}

#[tauri::command]
pub async fn update_dock_rect(rect: IntRect) {
    if let Ok(mut slot) = state::DOCK_RECT.lock() {
        *slot = Some(rect);
    }
}

#[tauri::command]
pub async fn update_notch_rect(rect: IntRect) {
    if let Ok(mut slot) = state::NOTCH_RECT.lock() {
        *slot = Some(rect);
    }
}

#[tauri::command]
pub fn set_window_height(window: Window, height: f64) {
    if let Ok(scale) = window.scale_factor() {
        if let Ok(size) = window.inner_size() {
            let width = size.width as f64 / scale;
            let _ = window.set_size(tauri::LogicalSize::new(width, height));
        }
    }
}

#[tauri::command]
pub fn resize_settings_window(app: AppHandle, width: f64, height: f64) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.set_size(tauri::LogicalSize::new(width, height));
    }
}

#[tauri::command]
pub fn set_ignore_cursor_events(window: Window, ignore: bool) {
    shell::set_click_through(&window, ignore);
}

#[tauri::command]
pub fn set_window_region(window: Window, rects: Vec<IntRect>) {
    shell::set_regions(&window, &rects);
}

#[tauri::command]
pub async fn init_dock(app: AppHandle, mode: String) {
    if !settings_file::dock_enabled() {
        if let Some(win) = app.get_webview_window("dock") {
            let _ = win.hide();
        }
        return;
    }
    shell::place_dock(&app, &mode);
    let _ = app.emit("dock-overlap", false);
    let _ = app.emit(
        "dock-maximized",
        state::CURRENT_FOREGROUND_MAXIMIZED.load(std::sync::atomic::Ordering::Relaxed),
    );
}

#[tauri::command]
pub async fn toggle_dock(app: AppHandle, enable: bool) {
    if enable {
        let mode = get_setting_str(&app, "bloom-dock-mode").unwrap_or_else(|| "smart".into());
        init_dock(app, mode).await;
    } else if let Some(win) = app.get_webview_window("dock") {
        let _ = win.hide();
    }
}

#[tauri::command]
pub async fn sync_appbar(app: AppHandle) {
    shell::place_notch(&app);
    shell::sync_overlay(&app);
}

#[tauri::command]
pub async fn change_dock_mode(app: AppHandle, mode: String) {
    init_dock(app, mode).await;
}

#[tauri::command]
pub async fn change_notch_mode(app: AppHandle, _mode: String) {
    shell::place_notch(&app);
}

#[tauri::command]
pub async fn open_app(_app: AppHandle, app_name: String) {
    let _ = host::launch(&app_name);
}

#[tauri::command]
pub async fn launch_new_instance(app_path: String, app_name: Option<String>) {
    let _ = host::launch_new(&app_path);
    let _ = app_name;
}

#[tauri::command]
pub async fn get_active_windows() -> Vec<AppInfo> {
    host::apps_from_windows(&host::live_windows())
}

#[tauri::command]
pub async fn focus_window(_app: AppHandle, hwnd: isize) -> bool {
    host::activate_window(hwnd)
}

#[tauri::command]
pub async fn focus_app_windows(hwnds: Vec<isize>) {
    host::cycle_app_windows(&hwnds);
}

#[tauri::command]
pub async fn get_app_icon(
    app: AppHandle,
    path: String,
    name: Option<String>,
    _hwnd: Option<isize>,
) -> Result<Option<String>, String> {
    if let Some(custom) = custom_icon(&app, &path)? {
        return Ok(Some(custom));
    }
    let icon_name = if path.ends_with(".desktop") {
        desktop_icon(&path)
    } else {
        None
    };
    Ok(icon_name
        .or(name)
        .and_then(|name| host::icon_for(&name))
        .or_else(|| host::icon_for(&path)))
}

fn desktop_icon(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| line.trim().strip_prefix("Icon=").map(|s| s.to_string()))
}

fn custom_map(app: &AppHandle) -> Result<HashMap<String, String>, String> {
    let path = config_file(app, "custom-icons.json")?;
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn custom_icon(app: &AppHandle, key: &str) -> Result<Option<String>, String> {
    Ok(custom_map(app)?.get(key).cloned())
}

#[tauri::command]
pub fn save_pinned_apps(app: AppHandle, apps: Vec<AppInfo>) -> Result<(), String> {
    let path = config_file(&app, "pinned.json")?;
    let content = serde_json::to_string(&apps).map_err(|e| e.to_string())?;
    settings_file::atomic_write_file(&path, content.as_bytes())
}

#[tauri::command]
pub async fn load_pinned_apps(app: AppHandle) -> Vec<AppInfo> {
    let Ok(path) = config_file(&app, "pinned.json") else {
        return Vec::new();
    };
    if let Ok(text) = std::fs::read_to_string(&path) {
        return serde_json::from_str(&text).unwrap_or_default();
    }
    let apps = host::favorite_apps();
    if !apps.is_empty() {
        if let Ok(content) = serde_json::to_string(&apps) {
            let _ = settings_file::atomic_write_file(&path, content.as_bytes());
        }
    }
    apps
}

#[tauri::command]
pub async fn clear_icon_cache(_app: AppHandle) -> Result<(), String> {
    host::clear_icons();
    Ok(())
}

#[tauri::command]
pub async fn set_custom_icon(app: AppHandle, cache_key: String, icon_data: String) -> Result<String, String> {
    let path = config_file(&app, "custom-icons.json")?;
    let mut map = custom_map(&app)?;
    map.insert(cache_key, icon_data.clone());
    let content = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
    settings_file::atomic_write_file(&path, &content)?;
    Ok(icon_data)
}

#[tauri::command]
pub async fn remove_custom_icon(app: AppHandle, cache_key: String) -> Result<(), String> {
    let path = config_file(&app, "custom-icons.json")?;
    let mut map = custom_map(&app)?;
    map.remove(&cache_key);
    let content = serde_json::to_vec(&map).map_err(|e| e.to_string())?;
    settings_file::atomic_write_file(&path, &content)
}

#[tauri::command]
pub async fn get_custom_icons(app: AppHandle) -> Result<HashMap<String, String>, String> {
    custom_map(&app)
}

#[tauri::command]
pub async fn get_installed_apps() -> Vec<AppInfo> {
    host::installed_apps(&host::live_windows())
}

#[tauri::command]
pub fn hide_native_osd() {}

#[tauri::command]
pub fn open_settings_window(app: AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

#[tauri::command]
pub fn hide_overlay(app: AppHandle) {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.hide();
    }
}

#[tauri::command]
pub fn set_splash_fullscreen(app: AppHandle, fullscreen: bool) {
    state::OVERLAY_IN_SPLASH.store(fullscreen, std::sync::atomic::Ordering::Relaxed);
    if let Some(win) = app.get_webview_window("overlay") {
        if fullscreen {
            shell::cover_monitor(&win);
            let _ = win.show();
        } else {
            let _ = win.hide();
        }
    }
}

#[tauri::command]
pub fn sync_overlay_position(app: AppHandle) {
    shell::sync_overlay(&app);
}

#[tauri::command]
pub fn open_wifi_settings() {
    host::control_center("wifi");
}

#[tauri::command]
pub fn open_sound_settings() {
    host::control_center("sound");
}

#[tauri::command]
pub fn open_notification_center() {
    host::toggle_notifications();
}

#[tauri::command]
pub fn open_system_tray(app: AppHandle) {
    let _ = app.emit("show-tray-panel", ());
}

#[tauri::command]
pub fn media_play_pause() {
    host::media_simple("PlayPause");
}

#[tauri::command]
pub fn media_next() {
    host::media_simple("Next");
}

#[tauri::command]
pub fn media_previous() {
    host::media_simple("Previous");
}

#[tauri::command]
pub fn media_seek(position_ms: f64) {
    host::media_seek(position_ms);
}

#[tauri::command]
pub fn open_media_source_app() {
    host::open_media_app();
}

#[tauri::command]
pub fn set_volume(volume: f32) {
    host::set_volume(volume);
    state::CURRENT_VOLUME.store((volume.clamp(0.0, 1.0) * 100.0) as u32, std::sync::atomic::Ordering::Relaxed);
}

#[tauri::command]
pub async fn get_audio_sessions() -> Result<Vec<AudioSessionInfo>, String> {
    Ok(host::audio_sessions())
}

#[tauri::command]
pub async fn get_audio_devices() -> AudioDevices {
    host::audio_devices()
}

#[tauri::command]
pub async fn set_audio_device(id: String) -> Result<(), String> {
    host::set_audio_device(&id)
}

#[tauri::command]
pub async fn set_app_volume(pid: u32, volume: f32) -> Result<(), String> {
    host::set_node_volume(pid, volume);
    Ok(())
}

#[tauri::command]
pub async fn set_app_mute(pid: u32, muted: bool) -> Result<(), String> {
    host::set_node_mute(pid, muted);
    Ok(())
}

#[tauri::command]
pub fn set_volume_mixer_rect(x: f64, y: f64, width: f64, height: f64) {
    if let Ok(mut slot) = state::VOLUME_MIXER_RECT.lock() {
        *slot = Some((x, y, width, height));
    }
}

#[tauri::command]
pub fn clear_volume_mixer_rect() {
    if let Ok(mut slot) = state::VOLUME_MIXER_RECT.lock() {
        *slot = None;
    }
}

#[tauri::command]
pub async fn quit_bloom(handle: AppHandle) {
    state::SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.exit(0);
}

#[tauri::command]
pub async fn restart_bloom(handle: AppHandle) {
    state::SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.restart();
}

#[tauri::command]
pub async fn close_window(hwnd: isize) {
    let _ = host::close_live_window(hwnd);
}

#[tauri::command]
pub fn save_setting(app: AppHandle, key: String, value: Value) -> Result<(), String> {
    let path = config_file(&app, "settings.json")?;
    let settings = settings_file::save_setting_file(&path, key.clone(), value)?;
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({ "key": &key, "value": &settings[&key] }),
    );
    if key == "bloom-scale" || key == "bloom-notch-mode" {
        shell::place_notch(&app);
    }
    if key == "bloom-dock-mode" || key == "bloom-dock-enabled" {
        let mode = get_setting_str(&app, "bloom-dock-mode").unwrap_or_else(|| "smart".into());
        shell::place_dock(&app, &mode);
    }
    Ok(())
}

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<HashMap<String, Value>, String> {
    let path = config_file(&app, "settings.json")?;
    settings_file::read_settings_file(&path)
}

#[tauri::command]
pub fn settings_file_readable(app: AppHandle) -> bool {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
        .is_some_and(|path| path.exists() && settings_file::read_settings_file(&path).is_ok())
}

#[tauri::command]
pub async fn capture_window_thumbnail(
    hwnd: isize,
    _max_width: u32,
    _max_height: u32,
) -> Result<Option<(String, i64)>, String> {
    let now = settings_file::get_now_ms();
    Ok(host::window_png(hwnd).map(|image| (image, now)))
}

#[tauri::command]
pub fn get_volume() -> f32 {
    state::CURRENT_VOLUME.load(std::sync::atomic::Ordering::Relaxed) as f32 / 100.0
}

#[tauri::command]
pub async fn get_volume_state() -> Result<crate::types::VolumeChangeEvent, String> {
    let (volume, is_muted) = host::volume_state();
    state::CURRENT_VOLUME.store((volume * 100.0) as u32, std::sync::atomic::Ordering::Relaxed);
    Ok(crate::types::VolumeChangeEvent { volume, is_muted })
}

#[tauri::command]
pub fn get_brightness() -> u32 {
    let value = host::brightness();
    state::CURRENT_BRIGHTNESS.store(value, std::sync::atomic::Ordering::Relaxed);
    value
}

#[tauri::command]
pub async fn get_wifi_status() -> Result<WifiStatus, String> {
    Ok(host::wifi_status())
}

#[tauri::command]
pub async fn set_wifi_state(enabled: bool) -> Result<(), String> {
    host::set_wifi(enabled)
}

#[tauri::command]
pub async fn get_bluetooth_state() -> Result<bool, String> {
    Ok(host::bluetooth_on())
}

#[tauri::command]
pub async fn set_bluetooth_state(enabled: bool) -> Result<(), String> {
    host::set_bluetooth(enabled)
}

#[tauri::command]
pub fn open_bluetooth_settings() {
    host::control_center("bluetooth");
}

#[tauri::command]
pub fn open_airplane_mode_settings() {
    host::control_center("network");
}

#[tauri::command]
pub fn set_brightness(app: AppHandle, brightness: u32) {
    host::set_brightness(brightness);
    state::CURRENT_BRIGHTNESS.store(brightness, std::sync::atomic::Ordering::Relaxed);
    state::LAST_BRIGHTNESS_CHANGE.store(settings_file::get_now_ms(), std::sync::atomic::Ordering::Relaxed);
    let _ = app.emit("brightness-change", crate::types::BrightnessChangeEvent { brightness });
}

#[tauri::command]
pub async fn get_battery_saver_state() -> Result<bool, String> {
    Ok(host::battery_saver())
}

#[tauri::command]
pub fn open_battery_saver_settings() {
    host::control_center("power");
}

#[tauri::command]
pub async fn get_battery_details() -> Result<BatteryDetails, String> {
    Ok(host::battery())
}

#[tauri::command]
pub fn get_system_accent_color() -> Result<String, String> {
    Ok(host::accent())
}

#[tauri::command]
pub fn get_cpu_usage() -> Result<u32, String> {
    Ok(host::cpu_usage())
}

#[tauri::command]
pub fn get_ram_usage() -> Result<f32, String> {
    Ok(host::ram_usage())
}

#[tauri::command]
pub fn get_disk_space() -> Result<u64, String> {
    Ok(host::disk_free_gb())
}

#[tauri::command]
pub fn get_network_speed() -> Result<(u64, u64), String> {
    Ok(host::network_speed())
}

#[tauri::command]
pub async fn export_settings_to_file(app: AppHandle, window: Window) -> Result<Option<String>, String> {
    if window.label() != "settings" {
        return Err("Settings export is only available from the Settings window".into());
    }
    let source = config_file(&app, "settings.json")?;
    let content = std::fs::read_to_string(&source).map_err(|e| e.to_string())?;
    settings_file::read_settings_file(&source)?;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("Export Bloom Settings")
            .set_file_name("bloom-settings.json")
            .add_filter("JSON", &["json"])
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn import_settings_from_file(app: AppHandle, window: Window) -> Result<Option<String>, String> {
    if window.label() != "settings" {
        return Err("Settings import is only available from the Settings window".into());
    }
    let picked = tauri::async_runtime::spawn_blocking({
        let app = app.clone();
        move || {
            use tauri_plugin_dialog::DialogExt;
            app.dialog()
                .file()
                .set_parent(&window)
                .set_title("Import Bloom Settings")
                .add_filter("JSON", &["json"])
                .blocking_pick_file()
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(file_path) = picked else {
        return Ok(None);
    };
    let path = file_path.into_path().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("Settings file exceeds the 1 MiB import limit".into());
    }
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    let imported: HashMap<String, Value> =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    let dest = config_file(&app, "settings.json")?;
    let _guard = SETTINGS_FILE_LOCK.lock().map_err(|e| e.to_string())?;
    settings_file::write_settings_file(&dest, &imported)?;
    settings_file::replace_settings_cache(imported.clone());
    drop(_guard);
    for (key, value) in &imported {
        let _ = app.emit("settings-changed", serde_json::json!({ "key": key, "value": value }));
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn reset_settings(app: AppHandle) -> Result<(), String> {
    let path = config_file(&app, "settings.json")?;
    let keep = ["bloom-first-run", "bloom-app-version"];
    let _guard = SETTINGS_FILE_LOCK.lock().map_err(|e| e.to_string())?;
    let mut settings = settings_file::read_settings_file(&path)?;
    let removed: Vec<String> = settings
        .keys()
        .filter(|key| key.starts_with("bloom-") && !keep.contains(&key.as_str()))
        .cloned()
        .collect();
    for key in &removed {
        settings.remove(key);
    }
    settings_file::write_settings_file(&path, &settings)?;
    settings_file::replace_settings_cache(settings);
    drop(_guard);
    for key in removed {
        let _ = app.emit(
            "settings-external-changed",
            serde_json::json!({ "key": key, "value": Value::Null }),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn get_tray_apps() -> Vec<TrayApp> {
    host::tray_apps()
}

#[tauri::command]
pub async fn show_tray_context_menu(tray_id: String) -> Result<(), String> {
    host::tray_activate(&tray_id, true)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SystemAction {
    TaskManager,
    DiskManagement,
    DeviceManager,
    ComputerManagement,
    Settings,
    TaskbarSettings,
    InstalledApps,
    PowerOptions,
    NetworkConnections,
    TaskView,
}

#[tauri::command]
pub async fn open_system_action(action: SystemAction) -> Result<(), String> {
    match action {
        SystemAction::TaskManager => host::open_task_manager(),
        SystemAction::ComputerManagement => host::open_task_manager(),
        SystemAction::DiskManagement => host::open_command("gnome-disks", &[]),
        SystemAction::DeviceManager => host::control_center("system"),
        SystemAction::Settings => host::control_center(""),
        SystemAction::TaskbarSettings => host::control_center("ubuntu"),
        SystemAction::InstalledApps => host::control_center("applications"),
        SystemAction::PowerOptions => host::control_center("power"),
        SystemAction::NetworkConnections => host::control_center("network"),
        SystemAction::TaskView => host::toggle_task_view(),
    }
    Ok(())
}

#[tauri::command]
pub async fn activate_tray_icon(tray_id: String) -> Result<(), String> {
    host::tray_open(&tray_id)
}

#[tauri::command]
pub async fn get_tray_menu(tray_id: String) -> Vec<host::TrayMenuItem> {
    host::tray_menu(&tray_id)
}

#[tauri::command]
pub async fn activate_tray_menu_item(tray_id: String, item_id: i32) -> Result<(), String> {
    host::tray_menu_activate(&tray_id, item_id)
}

#[tauri::command]
pub async fn get_dock_extras(app: AppHandle) -> Vec<DockExtra> {
    let position = get_setting_str(&app, "bloom-dock-extras-position").unwrap_or_else(|| "off".into());
    if position != "left" && position != "right" {
        return Vec::new();
    }
    let drives_on = get_setting_str(&app, "bloom-dock-extras-drives").unwrap_or_else(|| "true".into()) != "false";
    let bin_on = get_setting_str(&app, "bloom-dock-extras-recycle-bin").unwrap_or_else(|| "true".into()) != "false";
    let folders = get_setting_str(&app, "bloom-dock-extras-folders").unwrap_or_else(|| "downloads,documents".into());
    let hidden = hidden_drive_keys(&get_setting_str(&app, "bloom-dock-extras-hidden-drives").unwrap_or_default());
    let custom = get_setting_str(&app, "bloom-dock-extras-custom-folders").unwrap_or_else(|| "[]".into());
    let mut extras = Vec::new();
    if drives_on {
        extras.extend(host::drives().into_iter().filter(|drive| {
            drive
                .id
                .strip_prefix("drive:")
                .is_none_or(|key| !hidden.iter().any(|item| item == key))
        }));
    }
    for (id, name, path) in host::user_dirs() {
        if folders.split(',').any(|part| part.trim() == id) || (folders == "downloads" && id == "downloads") {
            extras.push(DockExtra {
                id: format!("folder:{id}"),
                kind: "folder".into(),
                name: name.into(),
                path: path.display().to_string(),
                icon: host::icon_for(folder_icon(id)),
                removable: false,
            });
        }
    }
    if let Ok(folders) = serde_json::from_str::<Vec<CustomFolder>>(&custom) {
        for folder in folders {
            if std::path::Path::new(&folder.path).is_dir() {
                extras.push(DockExtra {
                    id: format!("custom:{}", folder.path.to_lowercase()),
                    kind: "custom".into(),
                    name: folder.name,
                    path: folder.path,
                    icon: host::icon_for("folder"),
                    removable: false,
                });
            }
        }
    }
    if bin_on {
        if let Some(home) = std::env::var_os("HOME") {
            extras.push(DockExtra {
                id: "recycle-bin".into(),
                kind: "recycle-bin".into(),
                name: "Trash".into(),
                path: std::path::PathBuf::from(home)
                    .join(".local/share/Trash/files")
                    .display()
                    .to_string(),
                icon: host::icon_for("user-trash"),
                removable: false,
            });
        }
    }
    extras
}

#[tauri::command]
pub async fn get_dock_drives() -> Vec<DockExtra> {
    host::drives()
}

#[tauri::command]
pub async fn open_dock_extra(_kind: String, path: String) -> Result<(), String> {
    host::run_ok("xdg-open", &[&path])
}

#[tauri::command]
pub async fn eject_dock_extra(path: String) -> Result<(), String> {
    host::eject(&path)
}

#[tauri::command]
pub async fn wifi_list_networks() -> Result<WifiNetworkList, String> {
    Ok(host::wifi_networks())
}

#[tauri::command]
pub async fn wifi_scan() -> Result<(), String> {
    host::wifi_scan()
}

#[tauri::command]
pub async fn wifi_connect(ssid: String, password: Option<String>) -> Result<(), String> {
    host::wifi_connect(&ssid, password)
}

#[tauri::command]
pub async fn wifi_disconnect() -> Result<(), String> {
    host::wifi_disconnect()
}

#[tauri::command]
pub async fn wifi_forget(ssid: String) -> Result<(), String> {
    host::wifi_forget(&ssid)
}

#[tauri::command]
pub fn wifi_open_settings(page: String) -> Result<(), String> {
    host::control_center(if page == "location" { "privacy" } else { "wifi" });
    Ok(())
}

#[tauri::command]
pub async fn bluetooth_list_devices() -> Result<Vec<BtDevice>, String> {
    Ok(host::bluetooth_devices())
}

#[tauri::command]
pub async fn bluetooth_set_connected(id: String, connected: bool) -> Result<(), String> {
    host::bluetooth_connect(&id, connected)
}

#[tauri::command]
pub async fn bluetooth_discovery(app: AppHandle, active: bool) -> Result<(), String> {
    if !active {
        let _ = app.emit("bluetooth-nearby", Vec::<host::BtNearbyDevice>::new());
        return Ok(());
    }
    // ponytail: one 8s scan, then a single list. A live BlueZ watcher is the upgrade.
    let devices = tauri::async_runtime::spawn_blocking(host::bluetooth_scan)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit("bluetooth-nearby", devices);
    Ok(())
}

#[tauri::command]
pub async fn bluetooth_pair(id: String) -> Result<(), String> {
    host::bluetooth_pair(&id)
}

#[tauri::command]
pub fn bluetooth_pairing_respond(_accept: bool, _pin: Option<String>) {}

#[tauri::command]
pub async fn bluetooth_unpair(id: String) -> Result<(), String> {
    host::bluetooth_unpair(&id)
}
