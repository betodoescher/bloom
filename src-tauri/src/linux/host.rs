//! Desktop services on Linux: GNOME Shell (via the Bloom extension), X11 as a
//! fallback, PipeWire, NetworkManager, BlueZ, UPower and logind.
//!
//! ponytail: one `busctl`/`nmcli`/`wpctl` call per question instead of a D-Bus
//! client. Swap in zbus when a call needs a signal rather than a poll.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AppInfo, AudioSessionInfo, MediaInfo, WifiStatus};

#[derive(Clone, Serialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u32,
    pub security: &'static str,
    pub connected: bool,
    pub saved: bool,
}

#[derive(Clone, Serialize)]
pub struct WifiNetworkList {
    pub networks: Vec<WifiNetwork>,
    pub location_denied: bool,
}

#[derive(Clone, Serialize)]
pub struct BtDevice {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub connected: bool,
    pub battery: Option<u8>,
    pub can_connect: bool,
}

#[derive(Clone, Serialize)]
pub struct BtNearbyDevice {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
}

#[derive(Clone, Serialize)]
pub struct BatteryDetails {
    pub present: bool,
    pub percent: Option<u8>,
    pub plugged_in: bool,
    pub charging: bool,
    pub seconds_left: Option<u64>,
    pub seconds_to_full: Option<u64>,
    pub health_percent: Option<u8>,
    pub power_mode: Option<&'static str>,
}

#[derive(Clone, Serialize)]
pub struct DockExtra {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub path: String,
    pub icon: Option<String>,
    pub removable: bool,
}

#[derive(Clone, Serialize)]
pub struct TrayApp {
    pub name: String,
    pub path: String,
    pub tray_ids: Vec<String>,
    pub window_handles: Vec<isize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveWindow {
    pub id: isize,
    pub app_id: String,
    pub name: String,
    pub title: String,
    pub focused: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

pub fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn run_ok(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err.trim();
        Err(if msg.is_empty() {
            format!("{program} failed")
        } else {
            msg.to_string()
        })
    }
}

fn busctl(system: bool, args: &[&str]) -> Option<Value> {
    let mut cmd = Command::new("busctl");
    if !system {
        cmd.arg("--user");
    }
    cmd.arg("--json=short");
    cmd.args(args);
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok().map(normalize_busctl)
}

/// systemd 259 (`busctl --json=short` on Ubuntu 26.04) wraps a method's single
/// return value in an extra array: an `a{sv}` arrives as `[{...}]` and an `as`
/// as `[[...]]`. `get-property` is not wrapped.
fn normalize_busctl(mut value: Value) -> Value {
    let typ = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let Some(data) = value.get("data").cloned() else {
        return value;
    };
    let Some(items) = data.as_array() else {
        return value;
    };
    if items.len() != 1 {
        return value;
    }
    let unwrap = (typ.starts_with("a{") && items[0].is_object())
        || (typ.starts_with('a') && !typ.starts_with("a{") && items[0].is_array());
    if unwrap {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("data".into(), items[0].clone());
        }
    }
    value
}

fn bus_text(value: &Value) -> Option<String> {
    let data = value.get("data")?;
    data.as_str()
        .or_else(|| data.as_array().and_then(|items| items.first()).and_then(|item| item.as_str()))
        .map(|text| text.to_string())
}

fn field<'a>(dict: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a Value> {
    dict.get(key)?.get("data")
}

fn as_dict(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.get("data")?.as_object()
}

fn num_of(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|n| n as f64))
        .or_else(|| value.as_u64().map(|n| n as f64))
}

pub fn open_command(program: &str, args: &[&str]) {
    let _ = Command::new(program).args(args).spawn();
}

pub fn control_center(page: &str) {
    if page.is_empty() {
        open_command("gnome-control-center", &[]);
    } else {
        open_command("gnome-control-center", &[page]);
    }
}

pub fn open_task_manager() {
    if Command::new("resources").spawn().is_ok() || Command::new("gnome-system-monitor").spawn().is_ok() {
        return;
    }
    let _ = Command::new("x-terminal-emulator").args(["-e", "htop"]).spawn();
}

// --- windows ----------------------------------------------------------------

const SHELL_DEST: &str = "space.sehaz.Bloom";
const SHELL_PATH: &str = "/space/sehaz/Bloom";
const SHELL_IFACE: &str = "space.sehaz.Bloom";

fn shell_call(method: &str, extra: &[&str]) -> Option<Value> {
    let mut args = vec!["call", SHELL_DEST, SHELL_PATH, SHELL_IFACE, method];
    args.extend_from_slice(extra);
    busctl(false, &args)
}

pub fn shell_action(method: &str, extra: &[&str]) -> bool {
    let mut args = vec!["call", SHELL_DEST, SHELL_PATH, SHELL_IFACE, method];
    args.extend_from_slice(extra);
    Command::new("busctl")
        .arg("--user")
        .args(args)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

pub fn live_windows() -> Vec<LiveWindow> {
    if let Some(value) = shell_call("GetState", &[]) {
        if let Some(json) = bus_text(&value) {
            if let Some(windows) = parse_shell_windows(&json) {
                if !windows.is_empty() {
                    return windows;
                }
            }
        }
    }
    if let Some(windows) = atspi_windows() {
        if !windows.is_empty() {
            return windows;
        }
    }
    x11_windows()
}

pub fn parse_shell_windows(json: &str) -> Option<Vec<LiveWindow>> {
    let value: Value = serde_json::from_str(json).ok()?;
    let list = value.get("windows")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|item| {
                Some(LiveWindow {
                    id: item.get("id")?.as_i64()? as isize,
                    app_id: item.get("app_id")?.as_str()?.to_string(),
                    name: item.get("name")?.as_str()?.to_string(),
                    title: item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                    focused: item.get("focused").and_then(|v| v.as_bool()).unwrap_or(false),
                    maximized: item.get("maximized").and_then(|v| v.as_bool()).unwrap_or(false),
                    fullscreen: item
                        .get("fullscreen")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                })
            })
            .collect(),
    )
}

pub fn activate_window(id: isize) -> bool {
    dispatch_window(id, true)
}

/// Raises a window and never minimizes it. Multi-window dock clicks use this
/// so a second click moves to another window instead of hiding the group.
pub fn raise_window(id: isize) -> bool {
    dispatch_window(id, false)
}

/// Same rule as the Windows taskbar: with several windows, a click raises the
/// most recent one, and the next click visits another window instead of
/// minimizing the group.
pub fn cycle_app_windows(hwnds: &[isize]) {
    let Some(&most_recent) = hwnds.first() else {
        return;
    };
    let focused = live_windows().into_iter().find(|window| window.focused).map(|window| window.id);
    let mut ring = hwnds.to_vec();
    ring.sort_unstable();
    let target = match focused.and_then(|id| ring.iter().position(|hwnd| *hwnd == id)) {
        Some(index) => ring[(index + 1) % ring.len()],
        None => most_recent,
    };
    let _ = raise_window(target);
}

fn dispatch_window(id: isize, minimize_if_focused: bool) -> bool {
    let token = id.to_string();
    if shell_action("Activate", &["t", &token]) {
        return true;
    }
    // Shell ids and AT-SPI ids share this function. An AT-SPI id must not be
    // forwarded to X11: the hash can collide with a real window.
    if atspi_known(id) {
        return gnome_gesture(id, minimize_if_focused);
    }
    x11_focus_or_minimize(id as u32, minimize_if_focused)
}

const ATSPI_SCRIPT: &str = include_str!("../../linux-shell/atspi.py");

struct AtspiProc {
    child: Child,
    stdin: ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
}

type AtspiTarget = (String, String, u32);

static ATSPI: Mutex<Option<AtspiProc>> = Mutex::new(None);
static ATSPI_REFS: LazyLock<Mutex<HashMap<isize, AtspiTarget>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn spawn_atspi() -> Option<AtspiProc> {
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(ATSPI_SCRIPT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    Some(AtspiProc {
        child,
        stdin,
        stdout: std::io::BufReader::new(stdout),
    })
}

fn drop_atspi(slot: &mut Option<AtspiProc>) {
    if let Some(mut dead) = slot.take() {
        let _ = dead.child.kill();
    }
}

fn atspi_exchange(request: &Value) -> Option<Value> {
    let mut slot = ATSPI.lock().ok()?;
    for _ in 0..2 {
        if slot.is_none() {
            *slot = spawn_atspi();
        }
        if slot.is_none() {
            return None;
        }
        let line = serde_json::to_string(request).ok()?;
        let wrote = {
            let proc = slot.as_mut()?;
            writeln!(proc.stdin, "{line}").is_ok() && proc.stdin.flush().is_ok()
        };
        if !wrote {
            drop_atspi(&mut slot);
            continue;
        }
        let mut response = String::new();
        let read = {
            let proc = slot.as_mut()?;
            proc.stdout.read_line(&mut response)
        };
        match read {
            Ok(n) if n > 0 => return serde_json::from_str(response.trim()).ok(),
            _ => drop_atspi(&mut slot),
        }
    }
    None
}

fn atspi_hwnd(app: &str, title: &str, index: u32) -> isize {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in app.bytes().chain([0]).chain(title.bytes()).chain(index.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let id = (hash & 0x7fff_ffff) as isize;
    if id == 0 { 1 } else { id }
}

fn atspi_windows() -> Option<Vec<LiveWindow>> {
    let response = atspi_exchange(&serde_json::json!({"cmd": "list"}))?;
    if response.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let items = response.get("windows")?.as_array()?.clone();
    let mut refs = HashMap::new();
    let mut windows = Vec::new();
    for item in items {
        let Some(app) = item.get("app").and_then(|v| v.as_str()).map(str::to_string) else {
            continue;
        };
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let index = item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let id = atspi_hwnd(&app, &title, index);
        let active = item.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
        let focused_bit = item.get("focused").and_then(|v| v.as_bool()).unwrap_or(false);
        let iconified = item.get("iconified").and_then(|v| v.as_bool()).unwrap_or(false);
        let showing = item.get("showing").and_then(|v| v.as_bool()).unwrap_or(false);
        refs.insert(id, (app.clone(), title.clone(), index));
        windows.push(LiveWindow {
            id,
            app_id: app.clone(),
            name: app,
            title,
            focused: focused_bit || (active && showing && !iconified),
            maximized: false,
            fullscreen: false,
        });
    }
    if let Ok(mut slot) = ATSPI_REFS.lock() {
        *slot = refs;
    }
    Some(windows)
}

fn atspi_known(id: isize) -> bool {
    ATSPI_REFS.lock().ok().is_some_and(|refs| refs.contains_key(&id))
}

/// One dock click at a time. The gesture rewrites a keybinding for a few
/// hundred milliseconds and puts the previous value back on the way out.
static GESTURE: Mutex<()> = Mutex::new(());

fn gnome_gesture(id: isize, minimize_if_focused: bool) -> bool {
    let _hold = GESTURE.lock().unwrap_or_else(|err| err.into_inner());
    restore_gesture_backup();
    let Some(windows) = atspi_windows() else {
        return false;
    };
    let Some(window) = windows.iter().find(|window| window.id == id) else {
        return false;
    };
    if window.focused && minimize_if_focused {
        return minimize_focused_window();
    }
    present_desktop_app(&window.name, &window.title)
}

fn present_desktop_app(app_name: &str, title: &str) -> bool {
    let apps = installed();
    let Some(desktop) = desktop_for_window(&apps, app_name, title) else {
        return false;
    };
    let current = gsettings_get("org.gnome.shell", "favorite-apps").unwrap_or_default();
    let ids = desktop_ids_from_gsettings(&current);
    let slot = ids.iter().position(|id| id == &desktop.id);
    let (slot, restore_favorites) = match slot {
        // Slots 0..=8 are Super+1..Super+9, which gnome-shell already grabs.
        // Later favorites have no such chord, so they borrow slot 0 briefly.
        Some(index) if index < 9 => (index, false),
        _ => {
            let mut next = ids;
            next.retain(|id| id != &desktop.id);
            next.insert(0, desktop.id.clone());
            remember_gesture("favorite-apps", &current);
            if !gsettings_set("org.gnome.shell", "favorite-apps", &gsettings_string_array(&next)) {
                forget_gesture("favorite-apps");
                return false;
            }
            std::thread::sleep(Duration::from_millis(300));
            (0, true)
        }
    };
    let Some(code) = super_digit_code(slot) else {
        if restore_favorites {
            restore_favorite_apps();
        }
        return false;
    };
    let pressed = ydotool_key(&[
        "125:1".to_string(),
        format!("{code}:1"),
        format!("{code}:0"),
        "125:0".to_string(),
    ]);
    if restore_favorites {
        // The shell reads the favorite list when the chord arrives, which is
        // after ydotool has returned.
        std::thread::sleep(Duration::from_millis(180));
        restore_favorite_apps();
    }
    pressed
}

fn minimize_focused_window() -> bool {
    let Some(previous) = gsettings_get("org.gnome.desktop.wm.keybindings", "minimize") else {
        return false;
    };
    remember_gesture("minimize", &previous);
    if !gsettings_set(
        "org.gnome.desktop.wm.keybindings",
        "minimize",
        "['<Alt>F9']",
    ) {
        forget_gesture("minimize");
        return false;
    }
    // Mutter picks up wm keybindings asynchronously. The chord has to wait.
    std::thread::sleep(Duration::from_millis(450));
    let pressed = ydotool_key(&["56:1".into(), "67:1".into(), "67:0".into(), "56:0".into()]);
    restore_minimize_binding();
    pressed
}

fn super_digit_code(slot: usize) -> Option<u16> {
    match slot {
        0..=8 => Some(2 + slot as u16),
        9 => Some(11),
        _ => None,
    }
}

fn desktop_ids_from_gsettings(text: &str) -> Vec<String> {
    text.split(|ch: char| matches!(ch, '\'' | '"' | ',' | '[' | ']'))
        .map(str::trim)
        .filter(|id| id.ends_with(".desktop"))
        .map(str::to_string)
        .collect()
}

fn gsettings_string_array(ids: &[String]) -> String {
    let body = ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(", ");
    format!("[{body}]")
}

fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let text = run("gsettings", &["get", schema, key])?;
    let text = text.trim();
    if text.is_empty() { None } else { Some(text.to_string()) }
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> bool {
    Command::new("gsettings")
        .args(["set", schema, key, value])
        .status()
        .is_ok_and(|status| status.success())
}

fn ydotool_key(chords: &[String]) -> bool {
    let socket = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|dir| PathBuf::from(dir).join(".ydotool_socket"))
        .unwrap_or_else(|| PathBuf::from("/run/user/1000/.ydotool_socket"));
    Command::new("ydotool")
        .env("YDOTOOL_SOCKET", socket)
        .arg("key")
        .args(chords)
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Default, Serialize, Deserialize)]
struct GestureBackup {
    #[serde(default)]
    minimize: Option<String>,
    #[serde(default)]
    favorite_apps: Option<String>,
}

fn gesture_backup_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(home).join(".config"));
    Some(base.join("com.sehaz.bloom").join("gesture-backup.json"))
}

fn read_gesture_backup() -> GestureBackup {
    let Some(path) = gesture_backup_path() else {
        return GestureBackup::default();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return GestureBackup::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_gesture_backup(backup: &GestureBackup) {
    let Some(path) = gesture_backup_path() else {
        return;
    };
    if backup.minimize.is_none() && backup.favorite_apps.is_none() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(backup) {
        let _ = std::fs::write(path, text);
    }
}

fn remember_gesture(kind: &str, value: &str) {
    let mut backup = read_gesture_backup();
    match kind {
        "minimize" => backup.minimize = Some(value.to_string()),
        "favorite-apps" => backup.favorite_apps = Some(value.to_string()),
        _ => return,
    }
    write_gesture_backup(&backup);
}

fn forget_gesture(kind: &str) {
    let mut backup = read_gesture_backup();
    match kind {
        "minimize" => backup.minimize = None,
        "favorite-apps" => backup.favorite_apps = None,
        _ => return,
    }
    write_gesture_backup(&backup);
}

fn restore_minimize_binding() {
    let backup = read_gesture_backup();
    if let Some(previous) = backup.minimize {
        let _ = gsettings_set("org.gnome.desktop.wm.keybindings", "minimize", &previous);
        forget_gesture("minimize");
    }
}

fn restore_favorite_apps() {
    let backup = read_gesture_backup();
    if let Some(previous) = backup.favorite_apps {
        let _ = gsettings_set("org.gnome.shell", "favorite-apps", &previous);
        forget_gesture("favorite-apps");
    }
}

pub fn restore_gesture_backup() {
    restore_minimize_binding();
    restore_favorite_apps();
}

pub fn close_live_window(id: isize) -> bool {
    // The extension speaks mutter ids. An AT-SPI hash must never be forwarded
    // there or to X11: the hash can collide with a real window, and Close
    // still returns success when it does not find one.
    if shell_tracks(id) {
        let token = id.to_string();
        return shell_action("Close", &["t", &token]);
    }
    let _hold = GESTURE.lock().unwrap_or_else(|err| err.into_inner());
    restore_gesture_backup();
    if let Some((app, title, index)) = atspi_target_for(id) {
        return close_atspi_window(id, &app, &title, index);
    }
    x11_client_message(id as u32, b"_NET_CLOSE_WINDOW")
}

fn shell_tracks(id: isize) -> bool {
    let Some(value) = shell_call("GetState", &[]) else {
        return false;
    };
    let Some(json) = bus_text(&value) else {
        return false;
    };
    parse_shell_windows(&json).is_some_and(|windows| windows.iter().any(|win| win.id == id))
}

fn atspi_target_for(id: isize) -> Option<AtspiTarget> {
    if let Some(target) = ATSPI_REFS.lock().ok().and_then(|refs| refs.get(&id).cloned()) {
        return Some(target);
    }
    atspi_windows()?;
    ATSPI_REFS.lock().ok().and_then(|refs| refs.get(&id).cloned())
}

fn close_atspi_window(id: isize, app: &str, title: &str, index: u32) -> bool {
    if atspi_window_close(app, title, index) {
        return true;
    }
    if let Some(xid) = x11_id_for_title(title) {
        if x11_client_message(xid, b"_NET_CLOSE_WINDOW") {
            return true;
        }
    }
    close_with_shortcut(id, app, title)
}

fn atspi_window_close(app: &str, title: &str, index: u32) -> bool {
    let action = "window.close";
    if !is_window_close_action(action) {
        return false;
    }
    let response = atspi_exchange(&serde_json::json!({
        "cmd": "close",
        "app": app,
        "title": title,
        "index": index,
        "action": action,
    }));
    response.and_then(|value| value.get("ok").and_then(|ok| ok.as_bool())) == Some(true)
}

fn close_with_shortcut(id: isize, app: &str, title: &str) -> bool {
    let before = atspi_windows().unwrap_or_default();
    let already = before.iter().any(|win| win.id == id && win.focused);
    let others: Vec<isize> = before
        .iter()
        .filter(|win| win.focused && win.name != app)
        .map(|win| win.id)
        .collect();
    if !already && !present_desktop_app(app, title) {
        return false;
    }
    if !already {
        // Mutter and AT-SPI notice the new focus a moment after the chord.
        std::thread::sleep(Duration::from_millis(280));
    }
    let Some(after) = atspi_windows() else {
        return false;
    };
    let raised = after.iter().any(|win| win.id == id && win.focused)
        || after.iter().any(|win| win.name == app && win.title == title && win.focused);
    let blocked = others.iter().any(|other| {
        after
            .iter()
            .any(|win| win.id == *other && win.focused && win.name != app)
    });
    if !raised || blocked {
        return false;
    }
    let chord = alt_f4_chord().map(str::to_string);
    ydotool_key(&chord)
}

fn is_window_close_action(name: &str) -> bool {
    name == "window.close"
}

fn alt_f4_chord() -> [&'static str; 4] {
    // KEY_LEFTALT = 56, KEY_F4 = 62. Close is already bound to Alt+F4.
    ["56:1", "62:1", "62:0", "56:0"]
}

pub fn toggle_overview() {
    if shell_action("Overview", &[]) {
        return;
    }
    // Eval is refused unless the shell is in unsafe mode. Super+A is the
    // grabbed shortcut for the application grid, which is the Ubuntu menu.
    // A second click hides it through the overview property.
    if overview_is_open(&overview_property()) {
        let _ = set_overview_open(false);
        return;
    }
    let opened = ydotool_key(&["125:1".into(), "30:1".into(), "30:0".into(), "125:0".into()]);
    if !opened {
        let _ = set_overview_open(true);
    }
}

pub fn toggle_task_view() {
    if shell_action("Overview", &[]) {
        return;
    }
    // Task View is the window overview. Start uses Super+A for the app grid.
    let _ = set_overview_open(!overview_is_open(&overview_property()));
}

fn overview_property() -> String {
    run(
        "busctl",
        &[
            "--user",
            "get-property",
            "org.gnome.Shell",
            "/org/gnome/Shell",
            "org.gnome.Shell",
            "OverviewActive",
        ],
    )
    .unwrap_or_default()
}

fn overview_is_open(text: &str) -> bool {
    text.split_whitespace().next() == Some("b") && text.split_whitespace().nth(1) == Some("true")
}

fn set_overview_open(open: bool) -> bool {
    let value = if open { "true" } else { "false" };
    Command::new("busctl")
        .args([
            "--user",
            "set-property",
            "org.gnome.Shell",
            "/org/gnome/Shell",
            "org.gnome.Shell",
            "OverviewActive",
            "b",
            value,
        ])
        .status()
        .is_ok_and(|status| status.success())
}

pub fn toggle_notifications() {
    if !shell_action("Notifications", &[]) {
        control_center("notifications");
    }
}

pub fn window_png(id: isize) -> Option<String> {
    let id = id.to_string();
    let value = shell_call("Thumbnail", &["t", &id])?;
    let b64 = bus_text(&value)?;
    if b64.is_empty() {
        return None;
    }
    Some(format!("data:image/png;base64,{b64}"))
}

fn x11_windows() -> Vec<LiveWindow> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    use x11rb::rust_connection::RustConnection;
    let Ok((conn, screen)) = RustConnection::connect(None) else {
        return Vec::new();
    };
    let root = conn.setup().roots[screen].root;
    let Some(list_atom) = intern(&conn, b"_NET_CLIENT_LIST") else {
        return Vec::new();
    };
    let Some(reply) = conn
        .get_property(false, root, list_atom, AtomEnum::WINDOW, 0, 4096)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Vec::new();
    };
    let Some(ids) = reply.value32() else {
        return Vec::new();
    };
    let active = intern(&conn, b"_NET_ACTIVE_WINDOW")
        .and_then(|atom| {
            conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
                .ok()?
                .reply()
                .ok()
        })
        .and_then(|reply| reply.value32()?.next());
    let name_atom = intern(&conn, b"_NET_WM_NAME");
    let utf8 = intern(&conn, b"UTF8_STRING");
    let class_atom = intern(&conn, b"WM_CLASS");
    ids.map(|id| {
        let title = name_atom
            .zip(utf8)
            .and_then(|(atom, utf8)| prop_string(&conn, id, atom, utf8))
            .unwrap_or_default();
        let class = class_atom
            .and_then(|atom| prop_string(&conn, id, atom, AtomEnum::STRING.into()))
            .unwrap_or_default();
        let name = class.split('\0').next().unwrap_or("Window").to_string();
        if name.eq_ignore_ascii_case("bloom")
            || name.eq_ignore_ascii_case("mutter-x11-frames")
            || name.to_ascii_lowercase().contains("com.sehaz.bloom")
        {
            return None;
        }
        Some(        LiveWindow {
            id: id as isize,
            app_id: name.clone(),
            name: if name.is_empty() { title.clone() } else { name },
            title,
            focused: Some(id) == active,
            maximized: false,
            fullscreen: false,
        })
    })
    .flatten()
    .filter(|win| !win.name.is_empty())
    .collect()
}

fn x11_id_for_title(title: &str) -> Option<u32> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let hits: Vec<u32> = x11_windows()
        .into_iter()
        .filter(|win| win.title == title)
        .map(|win| win.id as u32)
        .collect();
    if hits.len() == 1 { Some(hits[0]) } else { None }
}

fn intern(conn: &x11rb::rust_connection::RustConnection, name: &[u8]) -> Option<u32> {
    use x11rb::protocol::xproto::ConnectionExt;
    conn.intern_atom(false, name).ok()?.reply().ok().map(|r| r.atom)
}

fn prop_string(
    conn: &x11rb::rust_connection::RustConnection,
    window: u32,
    atom: u32,
    type_: u32,
) -> Option<String> {
    use x11rb::protocol::xproto::ConnectionExt;
    let reply = conn.get_property(false, window, atom, type_, 0, 1024).ok()?.reply().ok()?;
    let bytes = reply.value;
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim_matches('\0').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn x11_client_message(window: u32, message: &[u8]) -> bool {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask, CLIENT_MESSAGE_EVENT};
    use x11rb::rust_connection::RustConnection;
    if window == 0 {
        return false;
    }
    let Ok((conn, screen)) = RustConnection::connect(None) else {
        return false;
    };
    let root = conn.setup().roots[screen].root;
    let Some(kind) = intern(&conn, message) else {
        return false;
    };
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: kind,
        data: ClientMessageData::from([0u32, 1, 0, 0, 0]),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok())
    .is_some()
}

fn x11_focus_or_minimize(window: u32, toggle: bool) -> bool {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask, CLIENT_MESSAGE_EVENT,
    };
    use x11rb::rust_connection::RustConnection;
    if window == 0 {
        return false;
    }
    let Ok((conn, screen)) = RustConnection::connect(None) else {
        return false;
    };
    let root = conn.setup().roots[screen].root;
    let active = intern(&conn, b"_NET_ACTIVE_WINDOW").and_then(|atom| {
        let reply = conn
            .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let active = reply.value32()?.next();
        active
    });
    let (message, data): (&[u8], [u32; 5]) = if toggle && active == Some(window) {
        let Some(hidden) = intern(&conn, b"_NET_WM_STATE_HIDDEN") else {
            return false;
        };
        (b"_NET_WM_STATE", [2, hidden, 0, 0, 1])
    } else {
        (b"_NET_ACTIVE_WINDOW", [1, 0, 0, 0, 0])
    };
    let Some(kind) = intern(&conn, message) else {
        return false;
    };
    let event = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window,
        type_: kind,
        data: ClientMessageData::from(data),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )
    .ok()
    .and_then(|cookie| cookie.check().ok())
    .is_some()
}

pub fn apps_from_windows(windows: &[LiveWindow]) -> Vec<AppInfo> {
    let desktops = installed();
    let mut groups: Vec<AppInfo> = Vec::new();
    for win in windows {
        let desktop = desktop_for_window(&desktops, &win.name, &win.title);
        let path = desktop
            .map(|entry| entry.path.clone())
            .unwrap_or_else(|| win.app_id.clone());
        let name = desktop
            .map(|entry| entry.name.clone())
            .unwrap_or_else(|| win.name.clone());
        let executable = desktop.map(|entry| entry.exec.clone());
        if let Some(existing) = groups.iter_mut().find(|app| app.path == path) {
            let list = existing.all_hwnds.get_or_insert_with(Vec::new);
            list.push((win.id, win.title.clone()));
            if win.focused {
                existing.hwnd = Some(win.id);
                if let Some(pos) = list.iter().position(|(id, _)| *id == win.id) {
                    let item = list.remove(pos);
                    list.insert(0, item);
                }
            }
            continue;
        }
        groups.push(AppInfo {
            name,
            path,
            icon: None,
            is_running: true,
            hwnd: Some(win.id),
            executable,
            all_hwnds: Some(vec![(win.id, win.title.clone())]),
        });
    }
    groups
}

// --- applications ------------------------------------------------------------

#[derive(Clone)]
struct DesktopEntry {
    id: String,
    name: String,
    path: String,
    exec: String,
    wm_class: String,
}

fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&xdg) {
            dirs.push(dir.join("applications"));
        }
    }
    for extra in [
        "/usr/share/applications",
        "/usr/local/share/applications",
        "/var/lib/snapd/desktop/applications",
    ] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn parse_desktop(path: &Path) -> Option<DesktopEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = String::new();
    let mut wm_class = String::new();
    let mut no_display = false;
    let mut hidden = false;
    let mut app = false;
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Type" => app = value == "Application",
            "Name" if name.is_none() => name = Some(value.to_string()),
            "Exec" => exec = value.to_string(),
            "StartupWMClass" => wm_class = value.to_string(),
            "NoDisplay" => no_display = value == "true",
            "Hidden" => hidden = value == "true",
            _ => {}
        }
    }
    if !app || no_display || hidden {
        return None;
    }
    Some(DesktopEntry {
        id: path.file_name()?.to_str()?.to_string(),
        name: name?,
        path: path.display().to_string(),
        exec,
        wm_class,
    })
}

fn scan_desktops() -> Vec<DesktopEntry> {
    let mut seen = HashMap::<String, DesktopEntry>::new();
    for dir in desktop_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop(&path) {
                seen.entry(app.id.clone()).or_insert(app);
            }
        }
    }
    let mut apps: Vec<_> = seen.into_values().collect();
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn installed() -> Vec<DesktopEntry> {
    static CACHE: Mutex<Option<(Instant, Vec<DesktopEntry>)>> = Mutex::new(None);
    let mut slot = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    let fresh = slot.as_ref().is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(30));
    if !fresh {
        *slot = Some((Instant::now(), scan_desktops()));
    }
    slot.as_ref().map(|(_, apps)| apps.clone()).unwrap_or_default()
}

fn pick_desktop<'a>(apps: &'a [DesktopEntry], app_id: &str) -> Option<&'a DesktopEntry> {
    let id = app_id.trim();
    if id.is_empty() {
        return None;
    }
    let file = if id.ends_with(".desktop") {
        id.to_string()
    } else {
        format!("{id}.desktop")
    };
    if let Some(app) = apps.iter().find(|app| app.id.eq_ignore_ascii_case(&file)) {
        return Some(app);
    }
    let needle = id.trim_end_matches(".desktop").to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    apps.iter()
        .find(|app| app.wm_class.eq_ignore_ascii_case(&needle))
        .or_else(|| {
            apps.iter()
                .filter(|app| app.id.to_ascii_lowercase().contains(&needle))
                .min_by_key(|app| app.id.len())
        })
}

fn compact_id(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn desktop_for_window<'a>(
    apps: &'a [DesktopEntry],
    app_name: &str,
    title: &str,
) -> Option<&'a DesktopEntry> {
    let name = app_name.trim();
    if !name.is_empty() {
        if let Some(hit) = apps
            .iter()
            .filter(|app| app.name.eq_ignore_ascii_case(name))
            .min_by_key(|app| app.id.len())
        {
            return Some(hit);
        }
    }
    let title_l = title.to_ascii_lowercase();
    if let Some(hit) = apps
        .iter()
        .filter(|app| {
            let n = app.name.to_ascii_lowercase();
            n.len() >= 4 && title_l.contains(&n)
        })
        .max_by_key(|app| app.name.len())
    {
        return Some(hit);
    }
    if let Some(hit) = pick_desktop(apps, name) {
        return Some(hit);
    }
    let compact = compact_id(name);
    if compact.len() < 6 {
        return None;
    }
    apps.iter()
        .filter(|app| compact_id(&app.id).contains(&compact))
        .min_by_key(|app| app.id.len())
}

pub fn favorite_apps() -> Vec<AppInfo> {
    let text = run("gsettings", &["get", "org.gnome.shell", "favorite-apps"]).unwrap_or_default();
    let apps = installed();
    text.split(|ch: char| matches!(ch, '\'' | '"' | ',' | '[' | ']'))
        .map(str::trim)
        .filter(|id| id.ends_with(".desktop"))
        .filter_map(|id| pick_desktop(&apps, id))
        .map(|app| AppInfo {
            name: app.name.clone(),
            path: app.path.clone(),
            icon: None,
            is_running: false,
            hwnd: None,
            executable: Some(app.exec.clone()),
            all_hwnds: None,
        })
        .collect()
}

pub fn installed_apps(running: &[LiveWindow]) -> Vec<AppInfo> {
    let running_ids: Vec<&str> = running.iter().map(|win| win.app_id.as_str()).collect();
    installed()
        .into_iter()
        .map(|app| {
            let is_running = running_ids.iter().any(|id| *id == app.id || *id == app.name);
            AppInfo {
                name: app.name,
                path: app.path,
                icon: None,
                is_running,
                hwnd: None,
                executable: Some(app.exec),
                all_hwnds: None,
            }
        })
        .collect()
}

pub fn launch(path: &str) -> Result<(), String> {
    if path == "start" {
        toggle_overview();
        return Ok(());
    }
    if path.ends_with(".desktop") {
        return run_ok("gio", &["launch", path]);
    }
    if Path::new(path).exists() {
        return Command::new(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    run_ok("gtk-launch", &[path])
}

pub fn launch_new(path: &str) -> Result<(), String> {
    if let Some(app) = installed().into_iter().find(|app| app.path == path) {
        let exec = clean_exec(&app.exec);
        if !exec.is_empty() {
            return Command::new("sh")
                .arg("-c")
                .arg(exec)
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
    }
    launch(path)
}

fn clean_exec(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|part| !part.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
}

static ICONS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn clear_icons() {
    if let Ok(mut cache) = ICONS.lock() {
        *cache = Some(HashMap::new());
    }
}

pub fn icon_for(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let mut cache = ICONS.lock().ok()?;
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(hit) = cache.get(name) {
        return Some(hit.clone());
    }
    let path = resolve_icon(name)?;
    let bytes = std::fs::read(&path).ok()?;
    let mime = match path.extension().and_then(|ext| ext.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/png",
    };
    let uri = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    );
    cache.insert(name.to_string(), uri.clone());
    Some(uri)
}

fn resolve_icon(name: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(name);
    if direct.is_file() {
        return Some(direct);
    }
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/icons"));
    }
    roots.push(PathBuf::from("/usr/share/icons/Yaru"));
    roots.push(PathBuf::from("/usr/share/icons/hicolor"));
    roots.push(PathBuf::from("/usr/share/icons/Adwaita"));
    roots.push(PathBuf::from("/usr/share/pixmaps"));
    let names = [format!("{name}.png"), format!("{name}.svg"), name.to_string()];
    let subs = [
        "48x48/places",
        "48x48/devices",
        "scalable/places",
        "scalable/devices",
        "32x32/places",
        "32x32/devices",
        "scalable/apps",
        "48x48/apps",
        "64x64/apps",
        "32x32/apps",
        "apps",
        "",
    ];
    for root in roots {
        for sub in subs {
            for file in &names {
                let path = if sub.is_empty() {
                    root.join(file)
                } else {
                    root.join(sub).join(file)
                };
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }
    None
}

// --- audio / media -----------------------------------------------------------

static PLAYER: Mutex<Option<String>> = Mutex::new(None);

pub fn volume_state() -> (f32, bool) {
    let text = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).unwrap_or_default();
    parse_wpctl(&text).unwrap_or((0.0, false))
}

pub fn parse_wpctl(text: &str) -> Option<(f32, bool)> {
    let rest = text.trim().strip_prefix("Volume:")?.trim();
    let mut parts = rest.split_whitespace();
    let level: f32 = parts.next()?.parse().ok()?;
    let muted = rest.contains("MUTED");
    Some((level.clamp(0.0, 1.0), muted))
}

pub fn set_volume(level: f32) {
    let level = format!("{:.3}", level.clamp(0.0, 1.0));
    let _ = run_ok("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &level]);
    if level != "0.000" {
        let _ = run_ok("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]);
    }
}

#[derive(Clone, Serialize)]
pub struct AudioEndpoint {
    pub id: String,
    pub name: String,
    pub selected: bool,
}

#[derive(Clone, Serialize)]
pub struct AudioDevices {
    pub outputs: Vec<AudioEndpoint>,
    pub inputs: Vec<AudioEndpoint>,
}

pub fn audio_devices() -> AudioDevices {
    let text = run("wpctl", &["status"]).unwrap_or_default();
    let mut devices = parse_audio_endpoints(&text);
    if !devices.outputs.iter().any(|item| item.selected) {
        if let Some(name) = default_node_description("@DEFAULT_AUDIO_SINK@") {
            mark_default(&mut devices.outputs, &name);
        }
    }
    if !devices.inputs.iter().any(|item| item.selected) {
        if let Some(name) = default_node_description("@DEFAULT_AUDIO_SOURCE@") {
            mark_default(&mut devices.inputs, &name);
        }
    }
    devices
}

pub fn set_audio_device(id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("unknown audio device".into());
    }
    run_ok("wpctl", &["set-default", id])
}

pub fn parse_audio_endpoints(text: &str) -> AudioDevices {
    let mut outputs = Vec::new();
    let mut inputs = Vec::new();
    let mut section = "";
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "Video" {
            break;
        }
        if trimmed.contains("Sinks:") {
            section = "out";
            continue;
        }
        if trimmed.contains("Sources:") {
            section = "in";
            continue;
        }
        if trimmed.contains("Filters:") || trimmed.contains("Streams:") || trimmed.contains("Devices:") {
            section = "";
            continue;
        }
        let Some((selected, id, name)) = parse_endpoint_line(line) else {
            continue;
        };
        let endpoint = AudioEndpoint { id, name, selected };
        match section {
            "out" => outputs.push(endpoint),
            "in" => inputs.push(endpoint),
            _ => {}
        }
    }
    AudioDevices { outputs, inputs }
}

fn parse_endpoint_line(line: &str) -> Option<(bool, String, String)> {
    let rest = line.trim_start_matches(|ch: char| !ch.is_ascii_digit() && ch != '*');
    let selected = rest.starts_with('*');
    let rest = rest.trim_start_matches('*').trim();
    let (id, name) = rest.split_once('.')?;
    let id = id.trim();
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let name = name.split(" [vol:").next().unwrap_or(name).trim();
    if name.is_empty() {
        return None;
    }
    Some((selected, id.to_string(), name.to_string()))
}

fn default_node_description(target: &str) -> Option<String> {
    let text = run("wpctl", &["inspect", target])?;
    for line in text.lines() {
        let Some((_, value)) = line.split_once("node.description") else {
            continue;
        };
        let value = value.trim().trim_start_matches('=').trim().trim_matches('"');
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn mark_default(list: &mut [AudioEndpoint], description: &str) {
    let desc = description.trim().to_ascii_lowercase();
    if desc.is_empty() {
        return;
    }
    let Some(index) = list.iter().position(|item| {
        let name = item.name.to_ascii_lowercase();
        name == desc || name.starts_with(&desc) || desc.starts_with(&name)
    }) else {
        return;
    };
    for (i, item) in list.iter_mut().enumerate() {
        item.selected = i == index;
    }
}

pub fn audio_sessions() -> Vec<AudioSessionInfo> {
    let text = run("wpctl", &["status"]).unwrap_or_default();
    parse_streams(&text)
        .into_iter()
        .filter_map(|(id, name)| {
            let (volume, is_muted) = run("wpctl", &["get-volume", &id.to_string()])
                .and_then(|text| parse_wpctl(&text))
                .unwrap_or((1.0, false));
            Some(AudioSessionInfo {
                pid: id,
                name,
                process_path: None,
                volume,
                is_muted,
            })
        })
        .collect()
}

pub fn parse_streams(text: &str) -> Vec<(u32, String)> {
    let mut streams = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.contains("Streams:") {
            streams = true;
            continue;
        }
        if streams && (line.starts_with("Video") || line.starts_with("Settings")) {
            break;
        }
        if !streams {
            continue;
        }
        let trimmed = line.trim();
        let Some((id, name)) = trimmed.split_once('.') else {
            continue;
        };
        if name.trim_start().starts_with("output_") || name.contains("input_") {
            continue;
        }
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        out.push((id, name.to_string()));
    }
    out
}

pub fn set_node_volume(id: u32, volume: f32) {
    let level = format!("{:.3}", volume.clamp(0.0, 1.0));
    let _ = run_ok("wpctl", &["set-volume", &id.to_string(), &level]);
}

pub fn set_node_mute(id: u32, muted: bool) {
    let _ = run_ok("wpctl", &[
        "set-mute",
        &id.to_string(),
        if muted { "1" } else { "0" },
    ]);
}

pub fn media() -> MediaInfo {
    let names = busctl(false, &["call", "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListNames"]);
    let players: Vec<String> = names
        .as_ref()
        .and_then(|value| value.get("data"))
        .and_then(|data| data.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|item| item.as_str())
                .filter(|name| name.starts_with("org.mpris.MediaPlayer2."))
                .map(|name| name.to_string())
                .collect()
        })
        .unwrap_or_default();
    let mut best: Option<MediaInfo> = None;
    let mut best_name = None;
    for name in &players {
        let Some(value) = busctl(false, &[
            "call", name, "/org/mpris/MediaPlayer2", "org.freedesktop.DBus.Properties", "GetAll", "s",
            "org.mpris.MediaPlayer2.Player",
        ]) else {
            continue;
        };
        let Some(info) = media_from_player(&value) else {
            continue;
        };
        if info.is_playing {
            best = Some(info);
            best_name = Some(name.clone());
            break;
        }
        if best.is_none() && info.has_media {
            best = Some(info);
            best_name = Some(name.clone());
        }
    }
    if let Ok(mut slot) = PLAYER.lock() {
        *slot = best_name;
    }
    best.unwrap_or(MediaInfo {
        title: String::new(),
        artist: String::new(),
        is_playing: false,
        has_media: false,
        artwork: None,
        position_ms: 0,
        duration_ms: 0,
        seek_enabled: false,
        position_updated_at: 0,
    })
}

fn media_from_player(value: &Value) -> Option<MediaInfo> {
    let dict = as_dict(value)?;
    let status = field(dict, "PlaybackStatus").and_then(|v| v.as_str()).unwrap_or("");
    let meta = field(dict, "Metadata")?.as_object()?;
    let title = meta_str(meta, "xesam:title").unwrap_or_default();
    if title.is_empty() && status.is_empty() {
        return None;
    }
    let artist = meta
        .get("xesam:artist")
        .and_then(|v| v.get("data"))
        .and_then(|data| data.as_array())
        .and_then(|list| list.first())
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let length_us = meta
        .get("mpris:length")
        .and_then(|v| v.get("data"))
        .and_then(num_of)
        .unwrap_or(0.0);
    let position_us = field(dict, "Position").and_then(num_of).unwrap_or(0.0);
    let art = meta
        .get("mpris:artUrl")
        .and_then(|v| v.get("data"))
        .and_then(|v| v.as_str())
        .and_then(art_data_uri);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Some(MediaInfo {
        title,
        artist,
        is_playing: status == "Playing",
        has_media: status == "Playing" || status == "Paused",
        artwork: art.map(|uri| vec![uri]),
        position_ms: (position_us / 1000.0) as i64,
        duration_ms: (length_us / 1000.0) as i64,
        seek_enabled: field(dict, "CanSeek").and_then(|v| v.as_bool()).unwrap_or(false),
        position_updated_at: now,
    })
}

fn meta_str(meta: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    meta.get(key)?.get("data")?.as_str().map(|s| s.to_string())
}

static ART: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn art_data_uri(url: &str) -> Option<String> {
    let mut cache = ART.lock().ok()?;
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(hit) = cache.get(url) {
        return Some(hit.clone());
    }
    let bytes = if let Some(path) = url.strip_prefix("file://") {
        std::fs::read(path).ok()?
    } else {
        let out = Command::new("curl")
            .args(["-fsSL", "--max-time", "4", url])
            .output()
            .ok()?;
        if !out.status.success() || out.stdout.is_empty() {
            return None;
        }
        out.stdout
    };
    let mime = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else {
        "image/png"
    };
    let uri = format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    cache.insert(url.to_string(), uri.clone());
    Some(uri)
}

fn player() -> Option<String> {
    PLAYER.lock().ok()?.clone()
}

pub fn media_simple(method: &str) {
    let Some(name) = player() else {
        return;
    };
    let _ = Command::new("busctl")
        .args([
            "--user", "call", &name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player",
            method,
        ])
        .output();
}

pub fn media_seek(position_ms: f64) {
    let Some(name) = player() else {
        return;
    };
    let Some(value) = busctl(false, &[
        "call", &name, "/org/mpris/MediaPlayer2", "org.freedesktop.DBus.Properties", "Get", "ss",
        "org.mpris.MediaPlayer2.Player", "Metadata",
    ]) else {
        return;
    };
    let data = value.get("data").unwrap_or(&Value::Null);
    let variant = data.as_array().and_then(|items| items.first()).unwrap_or(data);
    let track = variant
        .get("data")
        .and_then(|meta| meta.get("mpris:trackid"))
        .and_then(|track| track.get("data"))
        .and_then(|id| id.as_str())
        .unwrap_or("/");
    let micros = ((position_ms.max(0.0)) * 1000.0) as i64;
    let micros = micros.to_string();
    let _ = Command::new("busctl")
        .args([
            "--user", "call", &name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player",
            "SetPosition", "ox", track, &micros,
        ])
        .output();
}

pub fn open_media_app() {
    let Some(name) = player() else {
        return;
    };
    let desktop = busctl(false, &[
        "get-property", &name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2", "DesktopEntry",
    ])
    .and_then(|value| value.get("data").and_then(|d| d.as_str()).map(|s| s.to_string()));
    if let Some(desktop) = desktop {
        let _ = run_ok("gtk-launch", &[&desktop]);
    }
}

pub fn monitor_source() -> Option<String> {
    let sink = run("pactl", &["get-default-sink"])?;
    let sink = sink.trim();
    let sources = run("pactl", &["list", "short", "sources"])?;
    sources.lines().find_map(|line| {
        let name = line.split_whitespace().nth(1)?;
        name.contains(&format!("{sink}.monitor")).then(|| name.to_string())
    })
}

// --- brightness, power, metrics ---------------------------------------------

pub fn brightness() -> u32 {
    let Some((_, current, max)) = backlight() else {
        return 100;
    };
    if max == 0 {
        return 100;
    }
    ((current as u128 * 100) / max as u128).min(100) as u32
}

pub fn set_brightness(percent: u32) {
    let Some((name, _, max)) = backlight() else {
        return;
    };
    let value = (max as u128 * percent.min(100) as u128 / 100).to_string();
    let _ = Command::new("busctl")
        .args([
            "call",
            "org.freedesktop.login1",
            "/org/freedesktop/login1/session/auto",
            "org.freedesktop.login1.Session",
            "SetBrightness",
            "ssu",
            "backlight",
            &name,
            &value,
        ])
        .output();
}

fn backlight() -> Option<(String, u64, u64)> {
    let dir = std::fs::read_dir("/sys/class/backlight").ok()?.next()?.ok()?;
    let path = dir.path();
    let current = std::fs::read_to_string(path.join("brightness")).ok()?.trim().parse().ok()?;
    let max = std::fs::read_to_string(path.join("max_brightness")).ok()?.trim().parse().ok()?;
    Some((dir.file_name().to_str()?.to_string(), current, max))
}

pub fn battery() -> BatteryDetails {
    let value = busctl(true, &[
        "call",
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.DBus.Properties",
        "GetAll",
        "s",
        "org.freedesktop.UPower.Device",
    ]);
    let Some(dict) = value.as_ref().and_then(as_dict) else {
        return BatteryDetails {
            present: false,
            percent: None,
            plugged_in: true,
            charging: false,
            seconds_left: None,
            seconds_to_full: None,
            health_percent: None,
            power_mode: power_mode(),
        };
    };
    let present = field(dict, "IsPresent").and_then(|v| v.as_bool()).unwrap_or(false);
    let state = field(dict, "State").and_then(num_of).unwrap_or(0.0) as u32;
    let percent = field(dict, "Percentage").and_then(num_of).map(|n| n.round() as u8);
    let charging = state == 1 || state == 5;
    let plugged_in = charging || state == 4;
    let left = field(dict, "TimeToEmpty").and_then(num_of).unwrap_or(0.0) as u64;
    let full = field(dict, "TimeToFull").and_then(num_of).unwrap_or(0.0) as u64;
    let energy_full = field(dict, "EnergyFull").and_then(num_of).unwrap_or(0.0);
    let design = field(dict, "EnergyFullDesign").and_then(num_of).unwrap_or(0.0);
    let health = (design > 0.0).then(|| ((energy_full / design) * 100.0).round().clamp(0.0, 100.0) as u8);
    BatteryDetails {
        present,
        percent: present.then_some(percent).flatten(),
        plugged_in: !present || plugged_in,
        charging,
        seconds_left: (present && left > 0 && left < 24 * 60 * 60).then_some(left),
        seconds_to_full: (present && full > 0 && full < 24 * 60 * 60).then_some(full),
        health_percent: health,
        power_mode: power_mode(),
    }
}

fn power_mode() -> Option<&'static str> {
    let text = run("powerprofilesctl", &["get"])?;
    Some(match text.trim() {
        "power-saver" => "Best power efficiency",
        "performance" => "Best performance",
        _ => "Balanced",
    })
}

pub fn battery_saver() -> bool {
    run("powerprofilesctl", &["get"]).map(|t| t.trim() == "power-saver").unwrap_or(false)
}

pub fn cpu_usage() -> u32 {
    static LAST: Mutex<Option<(u64, u64)>> = Mutex::new(None);
    let Ok(text) = std::fs::read_to_string("/proc/stat") else {
        return 0;
    };
    let Some(sample) = parse_cpu(&text) else {
        return 0;
    };
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let usage = if let Some(prev) = *last {
        cpu_percent(prev, sample)
    } else {
        0
    };
    *last = Some(sample);
    usage
}

pub fn parse_cpu(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let mut nums = line.split_whitespace().skip(1).map(|n| n.parse::<u64>().unwrap_or(0));
    let user = nums.next()?;
    let nice = nums.next()?;
    let system = nums.next()?;
    let idle = nums.next()?;
    let iowait = nums.next().unwrap_or(0);
    let irq = nums.next().unwrap_or(0);
    let soft = nums.next().unwrap_or(0);
    let steal = nums.next().unwrap_or(0);
    let total = user + nice + system + idle + iowait + irq + soft + steal;
    Some((total, idle + iowait))
}

pub fn cpu_percent(prev: (u64, u64), next: (u64, u64)) -> u32 {
    let total = next.0.saturating_sub(prev.0);
    let idle = next.1.saturating_sub(prev.1);
    if total == 0 {
        0
    } else {
        ((total - idle) * 100 / total) as u32
    }
}

pub fn ram_usage() -> f32 {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return 0.0;
    };
    let mut total = 0.0;
    let mut available = 0.0;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next().unwrap_or("0").parse::<f32>().unwrap_or(0.0);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next().unwrap_or("0").parse::<f32>().unwrap_or(0.0);
        }
    }
    if total <= 0.0 {
        0.0
    } else {
        ((total - available) * 100.0 / total).clamp(0.0, 100.0)
    }
}

pub fn disk_free_gb() -> u64 {
    let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
    let path = std::ffi::CString::new("/").unwrap_or_default();
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return 0;
    }
    let free = stat.f_bavail as u64 * stat.f_frsize;
    free / (1024 * 1024 * 1024)
}

pub fn network_speed() -> (u64, u64) {
    static LAST: Mutex<Option<(Instant, u64, u64)>> = Mutex::new(None);
    let Ok(text) = std::fs::read_to_string("/proc/net/dev") else {
        return (0, 0);
    };
    let (rx, tx) = parse_net(&text);
    let now = Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let speed = if let Some((then, prev_tx, prev_rx)) = *last {
        let ms = now.duration_since(then).as_millis().max(1);
        let rate = |cur: u64, prev: u64| ((cur.saturating_sub(prev) as u128 * 1000) / ms) as u64;
        (rate(tx, prev_tx), rate(rx, prev_rx))
    } else {
        (0, 0)
    };
    *last = Some((now, tx, rx));
    speed
}

pub fn parse_net(text: &str) -> (u64, u64) {
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in text.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        if iface.trim() == "lo" {
            continue;
        }
        let mut cols = rest.split_whitespace();
        rx = rx.saturating_add(cols.next().unwrap_or("0").parse().unwrap_or(0));
        let tx_col = cols.nth(7).unwrap_or("0");
        tx = tx.saturating_add(tx_col.parse().unwrap_or(0));
    }
    (rx, tx)
}

pub fn accent() -> String {
    let name = run("gsettings", &["get", "org.gnome.desktop.interface", "accent-color"])
        .unwrap_or_default();
    accent_hex(name.trim().trim_matches('\'')).to_string()
}

pub fn accent_hex(name: &str) -> &'static str {
    match name {
        "blue" => "#3584e4",
        "teal" => "#2190a4",
        "green" => "#3a944a",
        "yellow" => "#c88800",
        "orange" => "#e66100",
        "red" => "#e62d42",
        "pink" => "#d56199",
        "purple" => "#9141ac",
        "slate" => "#6f8396",
        _ => "#3584e4",
    }
}

// --- wifi / bluetooth --------------------------------------------------------

pub fn wifi_status() -> WifiStatus {
    let radio = run("nmcli", &["-t", "-f", "WIFI", "radio"]).unwrap_or_default();
    let enabled = radio.trim() == "enabled";
    let networks = wifi_networks();
    let connected = networks.networks.iter().find(|net| net.connected);
    WifiStatus {
        enabled,
        connected: connected.is_some(),
        ssid: connected.map(|net| net.ssid.clone()),
    }
}

pub fn set_wifi(enabled: bool) -> Result<(), String> {
    run_ok("nmcli", &["radio", "wifi", if enabled { "on" } else { "off" }])
}

pub fn wifi_networks() -> WifiNetworkList {
    let text = run("nmcli", &["-t", "-f", "IN-USE,SSID,SIGNAL,SECURITY", "dev", "wifi", "list"])
        .unwrap_or_default();
    let saved = saved_ssids();
    let mut networks = Vec::new();
    for line in text.lines() {
        let Some(net) = parse_wifi_line(line, &saved) else {
            continue;
        };
        if net.ssid.is_empty() || networks.iter().any(|other: &WifiNetwork| other.ssid == net.ssid) {
            continue;
        }
        networks.push(net);
    }
    WifiNetworkList {
        networks,
        location_denied: false,
    }
}

fn saved_ssids() -> Vec<String> {
    run("nmcli", &["-t", "-f", "NAME", "connection", "show"])
        .unwrap_or_default()
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

pub fn parse_wifi_line(line: &str, saved: &[String]) -> Option<WifiNetwork> {
    let cols = split_nmcli(line);
    if cols.len() < 3 {
        return None;
    }
    let ssid = cols[1].clone();
    let signal = cols[2].parse().unwrap_or(0);
    let security = cols.get(3).map(|s| s.as_str()).unwrap_or("");
    Some(WifiNetwork {
        connected: cols[0].contains('*'),
        saved: saved.iter().any(|name| name == &ssid),
        ssid,
        signal,
        security: wifi_security(security),
    })
}

pub fn split_nmcli(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                cur.push(next);
            }
        } else if ch == ':' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(ch);
        }
    }
    out.push(cur);
    out
}

fn wifi_security(text: &str) -> &'static str {
    let text = text.to_ascii_lowercase();
    if text.contains("wpa3") {
        "wpa3"
    } else if text.contains("802.1x") {
        "enterprise"
    } else if text.contains("wpa2") {
        "wpa2"
    } else if text.contains("wpa") {
        "wpa"
    } else if text.contains("wep") {
        "wep"
    } else if text.is_empty() || text == "--" {
        "open"
    } else {
        "other"
    }
}

pub fn wifi_scan() -> Result<(), String> {
    run_ok("nmcli", &["device", "wifi", "rescan"])
}

pub fn wifi_connect(ssid: &str, password: Option<String>) -> Result<(), String> {
    if let Some(password) = password.filter(|p| !p.is_empty()) {
        run_ok("nmcli", &["-w", "25", "device", "wifi", "connect", ssid, "password", &password])
    } else {
        run_ok("nmcli", &["-w", "25", "device", "wifi", "connect", ssid])
    }
}

pub fn wifi_disconnect() -> Result<(), String> {
    let text = run("nmcli", &["-t", "-f", "DEVICE,TYPE", "device"]).unwrap_or_default();
    let device = text.lines().find_map(|line| {
        let (dev, kind) = line.split_once(':')?;
        (kind == "wifi").then_some(dev.to_string())
    });
    let Some(device) = device else {
        return Err("No Wi-Fi device".into());
    };
    run_ok("nmcli", &["device", "disconnect", &device])
}

pub fn wifi_forget(ssid: &str) -> Result<(), String> {
    run_ok("nmcli", &["connection", "delete", ssid])
}

pub fn bluetooth_on() -> bool {
    run("bluetoothctl", &["show"])
        .map(|text| text.lines().any(|line| line.trim() == "Powered: yes"))
        .unwrap_or(false)
}

pub fn set_bluetooth(enabled: bool) -> Result<(), String> {
    run_ok("bluetoothctl", &["power", if enabled { "on" } else { "off" }])
}

pub fn bluetooth_devices() -> Vec<BtDevice> {
    let text = run("bluetoothctl", &["devices"]).unwrap_or_default();
    let connected = run("bluetoothctl", &["devices", "Connected"]).unwrap_or_default();
    parse_bt(&text)
        .into_iter()
        .map(|(id, name)| BtDevice {
            connected: connected.contains(&id),
            kind: bt_kind(&name),
            can_connect: true,
            battery: None,
            id,
            name,
        })
        .collect()
}

pub fn parse_bt(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("[NEW] Device ")
                .or_else(|| line.strip_prefix("Device "))?;
            let (id, name) = rest.split_once(' ')?;
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            Some((id.to_string(), name.to_string()))
        })
        .collect()
}

fn bt_kind(name: &str) -> &'static str {
    let name = name.to_ascii_lowercase();
    if name.contains("head") || name.contains("buds") || name.contains("speaker") || name.contains("audio") {
        "audio"
    } else if name.contains("mouse") {
        "mouse"
    } else if name.contains("key") {
        "keyboard"
    } else if name.contains("phone") {
        "phone"
    } else if name.contains("controller") || name.contains("gamepad") {
        "gamepad"
    } else {
        "other"
    }
}

pub fn bluetooth_connect(id: &str, connected: bool) -> Result<(), String> {
    run_ok("bluetoothctl", &[if connected { "connect" } else { "disconnect" }, id])
}

pub fn bluetooth_scan() -> Vec<BtNearbyDevice> {
    let known = bluetooth_devices();
    let text = run("bluetoothctl", &["--timeout", "8", "scan", "on"]).unwrap_or_default();
    let mut seen = HashMap::new();
    for (id, name) in parse_bt(&text) {
        if known.iter().any(|dev| dev.id == id) {
            continue;
        }
        seen.insert(id, name);
    }
    let mut list: Vec<_> = seen
        .into_iter()
        .map(|(id, name)| BtNearbyDevice {
            kind: bt_kind(&name),
            id,
            name,
        })
        .collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    list
}

pub fn bluetooth_pair(id: &str) -> Result<(), String> {
    run_ok("timeout", &["40", "bluetoothctl", "pair", id])
}

pub fn bluetooth_unpair(id: &str) -> Result<(), String> {
    run_ok("bluetoothctl", &["remove", id])
}

// --- tray, folders, drives ---------------------------------------------------

pub fn tray_apps() -> Vec<TrayApp> {
    let value = busctl(false, &[
        "get-property",
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
        "RegisteredStatusNotifierItems",
    ]);
    let Some(items) = value.as_ref().and_then(|v| v.get("data")).and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str())
        .map(|item| {
            let (service, path) = item.split_once('@').unwrap_or((item, "/StatusNotifierItem"));
            let title = busctl(false, &[
                "get-property", service, path, "org.kde.StatusNotifierItem", "Title",
            ])
            .and_then(|v| v.get("data").and_then(|d| d.as_str()).map(|s| s.to_string()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| service.to_string());
            TrayApp {
                name: title,
                path: item.to_string(),
                tray_ids: vec![item.to_string()],
                window_handles: Vec::new(),
            }
        })
        .collect()
}

pub fn tray_activate(id: &str, menu: bool) -> Result<(), String> {
    let (service, path) = id.split_once('@').unwrap_or((id, "/StatusNotifierItem"));
    let method = if menu { "ContextMenu" } else { "Activate" };
    run_ok("busctl", &[
        "--user", "call", service, path, "org.kde.StatusNotifierItem", method, "ii", "0", "0",
    ])
}

pub fn user_dirs() -> Vec<(&'static str, &'static str, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut map = HashMap::from([
        ("XDG_DESKTOP_DIR", home.join("Desktop")),
        ("XDG_DOWNLOAD_DIR", home.join("Downloads")),
        ("XDG_DOCUMENTS_DIR", home.join("Documents")),
        ("XDG_PICTURES_DIR", home.join("Pictures")),
        ("XDG_MUSIC_DIR", home.join("Music")),
        ("XDG_VIDEOS_DIR", home.join("Videos")),
    ]);
    if let Some(text) = std::env::var_os("HOME").and_then(|home| {
        std::fs::read_to_string(PathBuf::from(home).join(".config/user-dirs.dirs")).ok()
    }) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').replace("$HOME", &home.display().to_string());
            if let Some(slot) = map.get_mut(key) {
                *slot = PathBuf::from(value);
            }
        }
    }
    vec![
        ("desktop", "Desktop", map["XDG_DESKTOP_DIR"].clone()),
        ("downloads", "Downloads", map["XDG_DOWNLOAD_DIR"].clone()),
        ("documents", "Documents", map["XDG_DOCUMENTS_DIR"].clone()),
        ("pictures", "Pictures", map["XDG_PICTURES_DIR"].clone()),
        ("music", "Music", map["XDG_MUSIC_DIR"].clone()),
        ("videos", "Videos", map["XDG_VIDEOS_DIR"].clone()),
    ]
}

pub fn drives() -> Vec<DockExtra> {
    let Ok(text) = std::fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut cols = line.split_whitespace();
        let Some(source) = cols.next() else { continue };
        let Some(target) = cols.next() else { continue };
        let removable = target.starts_with("/media/") || target.starts_with("/run/media/") || target.starts_with("/mnt/");
        if target != "/" && !removable {
            continue;
        }
        if source.starts_with("tmpfs") || source.starts_with("overlay") {
            continue;
        }
        let name = Path::new(target)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(if target == "/" { "Computer" } else { target })
            .to_string();
        let label = if target == "/" { "Computer".into() } else { name };
        out.push(DockExtra {
            id: format!("drive:{target}"),
            kind: "drive".into(),
            name: label,
            path: target.to_string(),
            icon: icon_for(if target == "/" { "computer" } else { "drive-harddisk" }),
            removable,
        });
    }
    out
}

pub fn eject(path: &str) -> Result<(), String> {
    let source = run("findmnt", &["-n", "-o", "SOURCE", "--target", path])
        .ok_or_else(|| "This folder is not a drive".to_string())?;
    let source = source.trim();
    run_ok("udisksctl", &["unmount", "-b", source])?;
    let _ = run_ok("udisksctl", &["power-off", "-b", source]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_line_unescapes_colons_and_marks_the_active_network() {
        let net = parse_wifi_line(r"*:Home\:5G:80:WPA2", &["Home:5G".into()]).unwrap();
        assert_eq!(net.ssid, "Home:5G");
        assert_eq!(net.signal, 80);
        assert!(net.connected && net.saved);
        assert_eq!(net.security, "wpa2");
    }

    #[test]
    fn wpctl_reads_level_and_mute() {
        assert_eq!(parse_wpctl("Volume: 0.32\n"), Some((0.32, false)));
        assert_eq!(parse_wpctl("Volume: 0.10 [MUTED]\n"), Some((0.1, true)));
    }

    #[test]
    fn cpu_delta_ignores_idle() {
        assert_eq!(cpu_percent((100, 80), (200, 100)), 80);
        assert_eq!(parse_cpu("cpu  10 0 10 80 0 0 0 0\n").unwrap(), (100, 80));
    }

    #[test]
    fn net_skips_loopback_and_sums_octets() {
        let text = "hdr\nhdr\n lo: 9 0 0 0 0 0 0 0 9\n eth0: 100 0 0 0 0 0 0 0 40\n";
        assert_eq!(parse_net(text), (100, 40));
    }

    #[test]
    fn shell_window_json_round_trips() {
        let windows = parse_shell_windows(
            r#"{"windows":[{"id":7,"app_id":"org.gnome.Nautilus.desktop","name":"Files","title":"Home","focused":true,"maximized":false,"fullscreen":false}]}"#,
        )
        .unwrap();
        assert_eq!(windows[0].id, 7);
        assert!(windows[0].focused);
    }

    #[test]
    fn accent_names_map_to_yaru_colors() {
        assert_eq!(accent_hex("orange"), "#e66100");
        assert_eq!(accent_hex("nope"), "#3584e4");
    }

    #[test]
    fn bluetooth_lines_keep_the_address() {
        let found = parse_bt("Device AA:BB:CC:DD:EE:FF Headphones\n");
        assert_eq!(found, vec![("AA:BB:CC:DD:EE:FF".into(), "Headphones".into())]);
        let found = parse_bt("[NEW] Device 11:22:33:44:55:66 Mouse\n[CHG] Device 11:22:33:44:55:66 RSSI: -40\n");
        assert_eq!(found, vec![("11:22:33:44:55:66".into(), "Mouse".into())]);
    }

    #[test]
    fn mpris_get_all_survives_the_systemd_259_wrapper() {
        let raw = serde_json::json!({
            "type": "a{sv}",
            "data": [{
                "PlaybackStatus": {"type": "s", "data": "Playing"},
                "CanSeek": {"type": "b", "data": true},
                "Position": {"type": "x", "data": 1_500_000},
                "Metadata": {"type": "a{sv}", "data": {
                    "xesam:title": {"type": "s", "data": "Dive"},
                    "xesam:artist": {"type": "as", "data": ["Ed Sheeran"]},
                    "mpris:length": {"type": "t", "data": 2_000_000}
                }}
            }]
        });
        let info = media_from_player(&normalize_busctl(raw)).unwrap();
        assert_eq!(info.title, "Dive");
        assert_eq!(info.artist, "Ed Sheeran");
        assert!(info.is_playing && info.has_media && info.seek_enabled);
        assert_eq!(info.position_ms, 1500);
        assert_eq!(info.duration_ms, 2000);
        let names = normalize_busctl(serde_json::json!({
            "type": "as",
            "data": [["org.mpris.MediaPlayer2.spotify"]]
        }));
        assert_eq!(names["data"][0], "org.mpris.MediaPlayer2.spotify");
    }

    #[test]
    fn wm_class_finds_the_snap_desktop_file() {
        let apps = vec![DesktopEntry {
            id: "spotify_spotify.desktop".into(),
            name: "Spotify".into(),
            path: "/var/lib/snapd/desktop/applications/spotify_spotify.desktop".into(),
            exec: "/snap/bin/spotify".into(),
            wm_class: "spotify".into(),
        }];
        let found = pick_desktop(&apps, "spotify").unwrap();
        assert_eq!(found.id, "spotify_spotify.desktop");
    }

    #[test]
    fn close_uses_the_gtk_window_action_or_alt_f4() {
        assert!(is_window_close_action("window.close"));
        assert!(!is_window_close_action("page.close"));
        assert!(!is_window_close_action("win.close-current-page"));
        assert!(!is_window_close_action("window.minimize"));
        assert_eq!(alt_f4_chord(), ["56:1", "62:1", "62:0", "56:0"]);
    }

    #[test]
    fn overview_property_reports_whether_the_menu_is_open() {
        assert!(overview_is_open("b true\n"));
        assert!(!overview_is_open("b false"));
        assert!(!overview_is_open(""));
    }

    #[test]
    fn favorite_slot_uses_the_shell_shortcut_keys() {
        assert_eq!(super_digit_code(0), Some(2));
        assert_eq!(super_digit_code(8), Some(10));
        assert_eq!(super_digit_code(9), Some(11));
        assert_eq!(super_digit_code(10), None);
        let ids = desktop_ids_from_gsettings(
            "['google-chrome.desktop', 'microsoft-edge.desktop', 'org.gnome.Ptyxis.desktop']",
        );
        assert_eq!(
            ids,
            vec![
                "google-chrome.desktop",
                "microsoft-edge.desktop",
                "org.gnome.Ptyxis.desktop",
            ]
        );
        assert_eq!(
            gsettings_string_array(&ids),
            "['google-chrome.desktop', 'microsoft-edge.desktop', 'org.gnome.Ptyxis.desktop']"
        );
    }

    #[test]
    fn window_name_picks_the_pinned_desktop_file() {
        let apps = vec![
            DesktopEntry {
                id: "com.google.Chrome.desktop".into(),
                name: "Google Chrome".into(),
                path: "/usr/share/applications/com.google.Chrome.desktop".into(),
                exec: "chrome".into(),
                wm_class: String::new(),
            },
            DesktopEntry {
                id: "google-chrome.desktop".into(),
                name: "Google Chrome".into(),
                path: "/usr/share/applications/google-chrome.desktop".into(),
                exec: "google-chrome".into(),
                wm_class: String::new(),
            },
            DesktopEntry {
                id: "spotify_spotify.desktop".into(),
                name: "Spotify".into(),
                path: "/snap/spotify.desktop".into(),
                exec: "spotify".into(),
                wm_class: "spotify".into(),
            },
            DesktopEntry {
                id: "org.gnome.Ptyxis.desktop".into(),
                name: "Terminal".into(),
                path: "/usr/share/applications/org.gnome.Ptyxis.desktop".into(),
                exec: "ptyxis".into(),
                wm_class: String::new(),
            },
        ];
        assert_eq!(
            desktop_for_window(&apps, "Google Chrome", "New Tab").unwrap().id,
            "google-chrome.desktop"
        );
        assert_eq!(
            desktop_for_window(&apps, "Chromium", "Spotify Premium").unwrap().id,
            "spotify_spotify.desktop"
        );
        assert_eq!(
            desktop_for_window(&apps, "ptyxis", "shell").unwrap().id,
            "org.gnome.Ptyxis.desktop"
        );
    }

    #[test]
    fn wpctl_status_lists_sinks_and_sources() {
        let text = "\
Audio
 ├─ Devices:
 │      51. Fifine Microphone
 ├─ Sinks:
 │      68. Meteor Lake Speaker [vol: 0.96]
 │     117. QCY H3 Lite [vol: 0.41]
 ├─ Sources:
 │  *   63. Fifine Microphone Mono [vol: 0.20]
 │      64. Camera [vol: 0.54]
 ├─ Filters:
 │  *   99. bluez_output
Video
 ├─ Sources:
 │  *   91. Integrated Camera
";
        let mut devices = parse_audio_endpoints(text);
        assert_eq!(devices.outputs.len(), 2);
        assert!(!devices.outputs.iter().any(|item| item.selected));
        assert_eq!(devices.outputs[1].name, "QCY H3 Lite");
        mark_default(&mut devices.outputs, "QCY H3 Lite");
        assert!(devices.outputs[1].selected);
        assert!(!devices.outputs[0].selected);
        assert_eq!(devices.inputs.len(), 2);
        assert!(devices.inputs[0].selected);
        assert_eq!(devices.inputs[0].id, "63");
    }
}
