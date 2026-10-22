//! Settings file shared by Windows and Linux. Values stay strings, including
//! `"true"` / `"false"`, matching `%APPDATA%/bloom/settings.json` and
//! `$XDG_CONFIG_HOME/bloom/settings.json`.

use std::collections::HashMap;

use serde_json::Value;

pub fn get_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Parse the editable settings file without discarding a UTF-8 BOM. A parse
/// failure must reach the caller rather than turning into an empty map: the
/// next save would otherwise overwrite all of the user's preferences.
pub fn read_settings_file(path: &std::path::Path) -> Result<HashMap<String, Value>, String> {
    match std::fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(content.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("Invalid settings file: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(e) => Err(format!("Could not read settings file: {e}")),
    }
}

/// Write a complete file in the same directory and replace the old file only
/// after the new contents are ready. A crash or write failure leaves the old file.
pub fn atomic_write_file(path: &std::path::Path, content: &[u8]) -> Result<(), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_file_name(format!(
        "{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("bloom"),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(e) = std::fs::write(&temp, content) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Could not write {}: {e}", temp.display()));
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Could not replace {}: {e}", path.display()));
    }
    Ok(())
}

pub fn write_settings_file(
    path: &std::path::Path,
    settings: &HashMap<String, Value>,
) -> Result<(), String> {
    let content = serde_json::to_vec(settings).map_err(|e| e.to_string())?;
    atomic_write_file(path, &content)
}

/// The four webviews may save at once. Keep each read/modify/write transaction
/// together so one window cannot replace another window's new setting.
pub fn save_setting_file(
    path: &std::path::Path,
    key: String,
    value: Value,
) -> Result<HashMap<String, Value>, String> {
    let _guard = crate::state::SETTINGS_FILE_LOCK
        .lock()
        .map_err(|e| e.to_string())?;
    let mut settings = read_settings_file(path)?;
    settings.insert(key, value);
    write_settings_file(path, &settings)?;
    replace_settings_cache(settings.clone());
    Ok(settings)
}

/// Load settings.json into the in-memory cache. Call once at startup.
pub fn init_settings_cache(app: &tauri::AppHandle) {
    use crate::state::SETTINGS_CACHE;
    use tauri::Manager;
    let cache = SETTINGS_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(path) = app
        .path()
        .app_config_dir()
        .ok()
        .map(|p| p.join("settings.json"))
    {
        if let Ok(settings) = read_settings_file(&path) {
            if let Ok(mut cache) = cache.lock() {
                *cache = settings;
            }
        }
    }
}

/// Replace the entire settings cache (used by save_setting and the file watcher).
pub fn replace_settings_cache(new_settings: HashMap<String, Value>) {
    let cache = crate::state::SETTINGS_CACHE
        .get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    if let Ok(mut cache) = cache.lock() {
        *cache = new_settings;
    }
}

pub fn get_bloom_scale(_app: &tauri::AppHandle) -> f64 {
    get_setting_str(_app, "bloom-scale")
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(1.0)
}

/// Read any string value from the settings cache. Returns None if the key is absent.
pub fn get_setting_str(_app: &tauri::AppHandle, key: &str) -> Option<String> {
    let cache = crate::state::SETTINGS_CACHE.get()?;
    let guard = cache.lock().ok()?;
    guard.get(key)?.as_str().map(|s| s.to_string())
}

fn dock_setting_enabled(value: Option<&Value>) -> bool {
    value.is_none_or(|value| value.as_str() == Some("true"))
}

/// An absent setting uses the fresh-install default. Any explicit value other
/// than the string "true" leaves the desktop's own bar in charge.
pub fn dock_enabled() -> bool {
    crate::state::SETTINGS_CACHE.get().is_none_or(|cache| {
        cache
            .lock()
            .map(|settings| dock_setting_enabled(settings.get("bloom-dock-enabled")))
            .unwrap_or(false)
    })
}
