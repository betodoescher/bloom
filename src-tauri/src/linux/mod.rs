//! Linux desktop shell. GNOME on Wayland cannot place a native window, so on
//! GNOME the process runs through XWayland (same approach as Coucou). wlroots,
//! KDE and COSMIC get a gtk-layer-shell overlay when the library is present.
//!
//! ponytail: the Ubuntu dock is left in place. GNOME does not let another app
//! remove its panel; hide it in the Ubuntu Dock settings if Bloom should be
//! the only bar. Global cursor position is unavailable on Wayland, so the top
//! and bottom 8px of Bloom's windows are the edge hotspots.

mod commands;
mod host;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Duration;

use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::settings_file::{self, get_bloom_scale};
use crate::state::{self, MENU_IS_OPEN, SHUTTING_DOWN};
use crate::types::{BrightnessChangeEvent, IntRect, VolumeChangeEvent};

const EXT_JS: &str = include_str!("../../linux-shell/extension.js");
const EXT_META: &str = include_str!("../../linux-shell/metadata.json");

struct Shape {
    ignore: bool,
    rects: Vec<IntRect>,
}

static SHAPES: LazyLock<Mutex<HashMap<String, Shape>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

thread_local! {
    static GTK: std::cell::RefCell<HashMap<String, gtk::ApplicationWindow>> =
        std::cell::RefCell::new(HashMap::new());
}

pub fn main() {
    if !claim_instance() {
        return;
    }
    prepare_environment();
    diagnostics::init_log();
    // A crash mid-click can leave a temporary minimize shortcut or favorite
    // order behind. Put the user's settings back before the dock is usable.
    host::restore_gesture_backup();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .invoke_handler(tauri::generate_handler![
            commands::hide_native_osd,
            commands::open_settings_window,
            commands::open_wifi_settings,
            commands::open_sound_settings,
            commands::open_notification_center,
            commands::open_system_tray,
            commands::set_ignore_cursor_events,
            commands::set_window_height,
            commands::resize_settings_window,
            commands::hide_overlay,
            commands::set_splash_fullscreen,
            commands::sync_overlay_position,
            commands::media_play_pause,
            commands::media_next,
            commands::media_previous,
            commands::media_seek,
            commands::open_media_source_app,
            commands::init_dock,
            commands::toggle_dock,
            commands::change_dock_mode,
            commands::change_notch_mode,
            commands::sync_appbar,
            commands::open_app,
            commands::launch_new_instance,
            commands::update_dock_rect,
            commands::update_notch_rect,
            commands::set_window_region,
            commands::set_dock_hovered,
            commands::set_notch_hovered,
            commands::set_notch_visible,
            commands::setup_shelf_drop,
            commands::shelf_thumbnail,
            commands::shelf_drag_icon,
            commands::shelf_prune,
            commands::shelf_open,
            commands::get_active_windows,
            commands::get_tray_apps,
            commands::show_tray_context_menu,
            commands::activate_tray_icon,
            commands::get_tray_menu,
            commands::activate_tray_menu_item,
            commands::open_system_action,
            commands::get_app_icon,
            commands::get_installed_apps,
            commands::save_pinned_apps,
            commands::load_pinned_apps,
            commands::clear_icon_cache,
            commands::set_custom_icon,
            commands::remove_custom_icon,
            commands::get_custom_icons,
            commands::set_menu_open,
            commands::focus_window,
            commands::focus_app_windows,
            commands::close_window,
            commands::quit_bloom,
            commands::restart_bloom,
            commands::get_volume,
            commands::get_volume_state,
            commands::get_brightness,
            commands::set_volume,
            commands::get_audio_sessions,
            commands::get_audio_devices,
            commands::set_audio_device,
            commands::set_app_volume,
            commands::set_app_mute,
            commands::set_volume_mixer_rect,
            commands::clear_volume_mixer_rect,
            commands::save_setting,
            commands::load_settings,
            commands::settings_file_readable,
            commands::capture_window_thumbnail,
            commands::get_wifi_status,
            commands::set_wifi_state,
            commands::get_bluetooth_state,
            commands::set_bluetooth_state,
            commands::open_bluetooth_settings,
            commands::open_airplane_mode_settings,
            commands::set_brightness,
            commands::get_battery_saver_state,
            commands::open_battery_saver_settings,
            commands::get_battery_details,
            commands::get_system_accent_color,
            commands::get_cpu_usage,
            commands::get_ram_usage,
            commands::get_disk_space,
            commands::get_network_speed,
            commands::export_settings_to_file,
            commands::import_settings_from_file,
            commands::reset_settings,
            crate::updater::check_for_updates,
            crate::updater::install_update,
            crate::updater::get_update_state,
            commands::get_dock_extras,
            commands::open_dock_extra,
            commands::eject_dock_extra,
            commands::get_dock_drives,
            commands::wifi_list_networks,
            commands::wifi_scan,
            commands::wifi_connect,
            commands::wifi_disconnect,
            commands::wifi_forget,
            commands::wifi_open_settings,
            commands::bluetooth_list_devices,
            commands::bluetooth_set_connected,
            commands::bluetooth_discovery,
            commands::bluetooth_pair,
            commands::bluetooth_pairing_respond,
            commands::bluetooth_unpair
        ])
        .setup(|app| {
            settings_file::init_settings_cache(app.handle());
            install_gnome_extension();
            for label in ["main", "dock", "overlay"] {
                if let Some(win) = app.get_webview_window(label) {
                    decorate(&win, label);
                    let _ = win.set_ignore_cursor_events(false);
                }
            }
            if let Some(win) = app.get_webview_window("main") {
                wire_shelf(&win);
            }
            if let Some(settings) = app.get_webview_window("settings") {
                let clone = settings.clone();
                settings.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = clone.hide();
                    }
                });
            }
            watch_pointer();
            start_poll(app.handle().clone());
            start_visualizer(app.handle().clone());
            watch_second_instance(app.handle().clone());
            build_tray(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                crate::updater::run_startup_check(handle).await;
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");
    app.run(|_, _| {});
}

mod diagnostics {
    pub fn init_log() {
        crate::diagnostics::initialize();
    }
}

fn claim_instance() -> bool {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let path = dir.join("bloom.sock");
    if let Ok(mut stream) = UnixStream::connect(&path) {
        let _ = stream.write_all(b"settings\n");
        return false;
    }
    let _ = std::fs::remove_file(&path);
    match UnixListener::bind(&path) {
        Ok(listener) => {
            let _ = listener.set_nonblocking(true);
            *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);
            true
        }
        Err(_) => true,
    }
}

fn watch_second_instance(app: AppHandle) {
    let listener = LISTENER.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(listener) = listener else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut buf = [0u8; 16];
                    let _ = stream.read(&mut buf);
                    let open = app.clone();
                    let _ = app.run_on_main_thread(move || commands::open_settings_window(open));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
}

pub fn prepare_environment() {
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let opt = std::env::var("BLOOM_X11").unwrap_or_default();
    let display = std::env::var("DISPLAY").unwrap_or_default();
    if should_prefer_x11(&session, &desktop, &opt, &display) {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    if std::env::var_os("GDK_DPI_SCALE").is_none() && is_gnome(&desktop) {
        if let Some(text) = host::run("gsettings", &["get", "org.gnome.desktop.interface", "text-scaling-factor"]) {
            if let Some(scale) = dpi_scale_for(&text) {
                std::env::set_var("GDK_DPI_SCALE", scale);
            }
        }
    }
}

fn is_gnome(desktop: &str) -> bool {
    desktop.split(':').any(|part| part.eq_ignore_ascii_case("gnome"))
}

fn should_prefer_x11(session: &str, desktop: &str, opt: &str, display: &str) -> bool {
    session.eq_ignore_ascii_case("wayland")
        && is_gnome(desktop)
        && opt != "0"
        && !display.trim().is_empty()
}

fn dpi_scale_for(text: &str) -> Option<String> {
    let factor: f64 = text.trim().parse().ok()?;
    if !(0.5..=3.0).contains(&factor) || (factor - 1.0).abs() < 0.01 {
        return None;
    }
    Some(format!("{:.4}", 1.0 / factor))
}

fn install_gnome_extension() {
    if !is_gnome(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()) {
        return;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let dir = PathBuf::from(home).join(".local/share/gnome-shell/extensions/bloom@sehaz.space");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = std::fs::write(dir.join("extension.js"), EXT_JS);
    let _ = std::fs::write(dir.join("metadata.json"), EXT_META);
    let _ = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.gnome.Shell.Extensions",
            "--object-path",
            "/org/gnome/Shell/Extensions",
            "--method",
            "org.gnome.Shell.Extensions.EnableExtension",
            "bloom@sehaz.space",
        ])
        .output();
}

fn decorate(win: &WebviewWindow, label: &str) {
    let Ok(gw) = win.gtk_window() else {
        return;
    };
    gw.set_accept_focus(false);
    gw.set_keep_above(true);
    let _ = gw.set_skip_taskbar_hint(true);
    let _ = gw.set_skip_pager_hint(true);
    if label != "overlay" && !try_layer(&gw, label) {
        gw.set_type_hint(gtk::gdk::WindowTypeHint::Dock);
    }
    GTK.with(|map| {
        map.borrow_mut().insert(label.to_string(), gw);
    });
}

struct LayerFns {
    supported: unsafe extern "C" fn() -> i32,
    init: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow),
    set_namespace: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, *const libc::c_char),
    set_layer: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, i32),
    set_anchor: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, i32, i32),
    set_exclusive: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, i32),
    set_keyboard: unsafe extern "C" fn(*mut gtk::ffi::GtkWindow, i32),
}

fn layer_fns() -> Option<&'static LayerFns> {
    static FNS: OnceLock<Option<LayerFns>> = OnceLock::new();
    FNS.get_or_init(|| unsafe {
        let handle = libc::dlopen(c"libgtk-layer-shell.so.0".as_ptr(), libc::RTLD_LAZY);
        if handle.is_null() {
            return None;
        }
        let sym = |name: &[u8]| libc::dlsym(handle, name.as_ptr() as *const libc::c_char);
        let supported = sym(b"gtk_layer_is_supported\0");
        let init = sym(b"gtk_layer_init_for_window\0");
        let set_namespace = sym(b"gtk_layer_set_namespace\0");
        let set_layer = sym(b"gtk_layer_set_layer\0");
        let set_anchor = sym(b"gtk_layer_set_anchor\0");
        let set_exclusive = sym(b"gtk_layer_set_exclusive_zone\0");
        let set_keyboard = sym(b"gtk_layer_set_keyboard_mode\0");
        if [supported, init, set_namespace, set_layer, set_anchor, set_exclusive, set_keyboard]
            .iter()
            .any(|ptr| ptr.is_null())
        {
            return None;
        }
        Some(LayerFns {
            supported: std::mem::transmute(supported),
            init: std::mem::transmute(init),
            set_namespace: std::mem::transmute(set_namespace),
            set_layer: std::mem::transmute(set_layer),
            set_anchor: std::mem::transmute(set_anchor),
            set_exclusive: std::mem::transmute(set_exclusive),
            set_keyboard: std::mem::transmute(set_keyboard),
        })
    })
    .as_ref()
}

fn try_layer(gw: &gtk::ApplicationWindow, label: &str) -> bool {
    if std::env::var("BLOOM_LAYER_SHELL").ok().as_deref() == Some("0") {
        return false;
    }
    let Some(fns) = layer_fns() else {
        return false;
    };
    if unsafe { (fns.supported)() } == 0 || gw.is_realized() {
        return false;
    }
    let ptr = gw.upcast_ref::<gtk::Window>().to_glib_none().0;
    let name = if label == "dock" { c"bloom-dock" } else { c"bloom" };
    unsafe {
        (fns.init)(ptr);
        (fns.set_namespace)(ptr, name.as_ptr());
        (fns.set_layer)(ptr, 3);
        (fns.set_anchor)(ptr, 2, 1);
        if label == "dock" {
            (fns.set_anchor)(ptr, 0, 1);
            (fns.set_anchor)(ptr, 1, 1);
            (fns.set_anchor)(ptr, 3, 1);
            (fns.set_exclusive)(ptr, 64);
        } else {
            (fns.set_exclusive)(ptr, -1);
        }
        (fns.set_keyboard)(ptr, 0);
    }
    true
}

pub fn set_click_through(window: &tauri::Window, ignore: bool) {
    if let Ok(mut shapes) = SHAPES.lock() {
        shapes.entry(window.label().to_string()).or_insert(Shape { ignore: false, rects: Vec::new() }).ignore = ignore;
    }
    apply_shape(&window.label());
}

pub fn set_regions(window: &tauri::Window, rects: &[IntRect]) {
    if let Ok(mut shapes) = SHAPES.lock() {
        let entry = shapes.entry(window.label().to_string()).or_insert(Shape { ignore: false, rects: Vec::new() });
        entry.rects = rects.to_vec();
        entry.ignore = false;
    }
    apply_shape(&window.label());
}

fn apply_shape(label: &str) {
    let (ignore, rects) = SHAPES
        .lock()
        .ok()
        .and_then(|shapes| shapes.get(label).map(|shape| (shape.ignore, shape.rects.clone())))
        .unwrap_or((false, Vec::new()));
    GTK.with(|map| {
        let Some(gw) = map.borrow().get(label).cloned() else {
            return;
        };
        let Some(gdk_win) = gw.window() else {
            return;
        };
        let region = gtk::cairo::Region::create();
        if !ignore {
            let scale = gw.scale_factor() as f64;
            for rect in rects {
                if rect.width <= 0 || rect.height <= 0 {
                    continue;
                }
                let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(
                    (rect.x as f64 * scale).floor() as i32,
                    (rect.y as f64 * scale).floor() as i32,
                    (rect.width as f64 * scale).ceil() as i32,
                    (rect.height as f64 * scale).ceil() as i32,
                ));
            }
            let strip = (8.0 * scale).ceil().max(1.0) as i32;
            let width = gdk_win.width().max(1);
            let height = gdk_win.height();
            if label == "main" {
                let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(0, 0, width, strip));
            } else if label == "dock" {
                let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(0, (height - strip).max(0), width, strip));
            }
        }
        gdk_win.input_shape_combine_region(&region, 0, 0);
    });
}

fn watch_pointer() {
    let mut notch_edge = false;
    let mut dock_edge = false;
    let mut notch_inside = false;
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
            return gtk::glib::ControlFlow::Break;
        }
        let app = APP.get().cloned();
        let (over_notch, notch_y) = pointer_y("main");
        let (over_dock, dock_from_bottom) = pointer_from_bottom("dock");
        if let Some(app) = app {
            if over_notch && notch_y < 12 {
                if !notch_edge {
                    notch_edge = true;
                    let _ = app.emit("notch-edge-hover", true);
                }
            } else if notch_edge {
                notch_edge = false;
                let _ = app.emit("notch-edge-hover", false);
            }
            if over_notch {
                notch_inside = true;
            } else if notch_inside {
                notch_inside = false;
                let _ = app.emit("notch-pointer-left", ());
            }
            if over_dock && dock_from_bottom < 12 {
                if !dock_edge {
                    dock_edge = true;
                    let _ = app.emit("dock-edge-hover", true);
                }
            } else if dock_edge {
                dock_edge = false;
                let _ = app.emit("dock-edge-hover", false);
            }
            if MENU_IS_OPEN.load(std::sync::atomic::Ordering::Relaxed) && !over_dock {
                let _ = app.emit("dock-dismiss-menu", ());
            }
        }
        gtk::glib::ControlFlow::Continue
    });
}

static APP: OnceLock<AppHandle> = OnceLock::new();

fn pointer_y(label: &str) -> (bool, i32) {
    GTK.with(|map| {
        let Some(gw) = map.borrow().get(label).cloned() else {
            return (false, 0);
        };
        match pointer_pos(&gw) {
            Some((_, y)) => (true, y),
            None => (false, 0),
        }
    })
}

fn pointer_from_bottom(label: &str) -> (bool, i32) {
    GTK.with(|map| {
        let Some(gw) = map.borrow().get(label).cloned() else {
            return (false, 9999);
        };
        let Some(window) = gw.window() else {
            return (false, 9999);
        };
        match pointer_pos(&gw) {
            Some((_, y)) => (true, window.height() - y),
            None => (false, 9999),
        }
    })
}

fn pointer_pos(gw: &gtk::ApplicationWindow) -> Option<(i32, i32)> {
    let ours = gw.window()?.toplevel();
    let seat = gw.display().default_seat()?;
    let pointer = seat.pointer()?;
    let (under, x, y) = pointer.window_at_position();
    if under?.toplevel() != ours {
        return None;
    }
    Some((x, y))
}

pub fn place_notch(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let Ok(Some(monitor)) = win.primary_monitor() else {
        return;
    };
    let pos = monitor.position();
    let size = monitor.size();
    let scale = monitor.scale_factor();
    let height = (420.0 * get_bloom_scale(app) * scale) as u32;
    let _ = win.set_position(tauri::PhysicalPosition::new(pos.x, pos.y));
    let _ = win.set_size(tauri::PhysicalSize::new(size.width, height.max(1)));
    let _ = win.show();
}

pub fn place_dock(app: &AppHandle, _mode: &str) {
    let Some(win) = app.get_webview_window("dock") else {
        return;
    };
    if !settings_file::dock_enabled() {
        let _ = win.hide();
        return;
    }
    let Ok(Some(monitor)) = win.primary_monitor() else {
        return;
    };
    let pos = monitor.position();
    let size = monitor.size();
    let height = win.outer_size().map(|s| s.height).unwrap_or(600).max(80);
    let y = pos.y + size.height as i32 - height as i32;
    let _ = win.set_size(tauri::PhysicalSize::new(size.width, height));
    let _ = win.set_position(tauri::PhysicalPosition::new(pos.x, y));
    let _ = win.show();
}

pub fn cover_monitor(win: &WebviewWindow) {
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let pos = monitor.position();
        let size = monitor.size();
        let _ = win.set_position(tauri::PhysicalPosition::new(pos.x, pos.y));
        let _ = win.set_size(tauri::PhysicalSize::new(size.width, size.height));
    }
}

pub fn sync_overlay(app: &AppHandle) {
    if state::OVERLAY_IN_SPLASH.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Some(win) = app.get_webview_window("overlay") {
        cover_monitor(&win);
    }
}

fn wire_shelf(win: &WebviewWindow) {
    let app = win.app_handle().clone();
    win.on_window_event(move |event| {
        let tauri::WindowEvent::DragDrop(drop) = event else {
            return;
        };
        match drop {
            tauri::DragDropEvent::Enter { paths, .. } => {
                let _ = app.emit("shelf-drag-enter", serde_json::json!({ "paths": paths }));
            }
            tauri::DragDropEvent::Drop { paths, .. } => {
                let _ = app.emit("shelf-drag-leave", ());
                let _ = app.emit("shelf-drag-drop", serde_json::json!({ "paths": paths }));
            }
            tauri::DragDropEvent::Leave => {
                let _ = app.emit("shelf-drag-leave", ());
            }
            _ => {}
        }
    });
}

fn start_poll(app: AppHandle) {
    let _ = APP.set(app.clone());
    std::thread::spawn(move || {
        let mut last_media = String::new();
        let mut last_vol = (u32::MAX, false);
        let mut last_bright = u32::MAX;
        let mut last_windows = String::new();
        let mut last_wifi = String::new();
        let mut tick = 0u32;
        loop {
            if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let media = host::media();
            state::ANY_MEDIA_PLAYING.store(media.is_playing, std::sync::atomic::Ordering::Relaxed);
            let media_key = format!("{}|{}|{}|{}", media.title, media.artist, media.is_playing, media.has_media);
            if media_key != last_media {
                last_media = media_key;
                let _ = app.emit("media-update", &media);
            }
            let (volume, is_muted) = host::volume_state();
            let vol_i = (volume * 100.0) as u32;
            if (vol_i, is_muted) != last_vol {
                last_vol = (vol_i, is_muted);
                state::CURRENT_VOLUME.store(vol_i, std::sync::atomic::Ordering::Relaxed);
                let _ = app.emit("volume-change", VolumeChangeEvent { volume, is_muted });
            }
            let brightness = host::brightness();
            if brightness != last_bright {
                last_bright = brightness;
                state::CURRENT_BRIGHTNESS.store(brightness, std::sync::atomic::Ordering::Relaxed);
                let _ = app.emit("brightness-change", BrightnessChangeEvent { brightness });
            }
            if tick % 2 == 0 {
                let windows = host::live_windows();
                let key = windows
                    .iter()
                    .map(|win| format!("{}:{}:{}:{}", win.id, win.title, win.focused, win.maximized))
                    .collect::<String>();
                if key != last_windows {
                    last_windows = key;
                    let maximized = windows.iter().any(|win| win.focused && win.maximized);
                    let full = windows.iter().any(|win| win.focused && (win.fullscreen || win.maximized));
                    state::CURRENT_FOREGROUND_MAXIMIZED.store(maximized, std::sync::atomic::Ordering::Relaxed);
                    state::CURRENT_FOREGROUND_FULLSCREEN.store(full, std::sync::atomic::Ordering::Relaxed);
                    let _ = app.emit("windows-changed", ());
                    let _ = app.emit("dock-maximized", maximized);
                    let _ = app.emit("notch-overlap", full);
                }
            }
            if tick % 5 == 0 {
                let status = host::wifi_status();
                let key = format!("{}:{}:{:?}", status.enabled, status.connected, status.ssid);
                if key != last_wifi {
                    last_wifi = key;
                    let _ = app.emit("wifi-changed", ());
                }
                let _ = app.emit("bluetooth-changed", ());
            }
            tick = tick.wrapping_add(1);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

fn start_visualizer(app: AppHandle) {
    std::thread::spawn(move || {
        use rustfft::num_complex::Complex;
        use rustfft::FftPlanner;
        const N: usize = 512;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N);
        let mut max_band = [0.12f32; 5];
        loop {
            if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            if !state::ANY_MEDIA_PLAYING.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(400));
                continue;
            }
            let Some(source) = host::monitor_source() else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            let Ok(mut child) = std::process::Command::new("parec")
                .args(["-d", &source, "--format=s16le", "--rate=44100", "--channels=1", "--latency-msec=40"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            };
            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill();
                continue;
            };
            let mut raw = [0u8; N * 2];
            while state::ANY_MEDIA_PLAYING.load(std::sync::atomic::Ordering::Relaxed)
                && !SHUTTING_DOWN.load(std::sync::atomic::Ordering::Relaxed)
            {
                if stdout.read_exact(&mut raw).is_err() {
                    break;
                }
                let mut input: Vec<Complex<f32>> = raw
                    .chunks_exact(2)
                    .enumerate()
                    .map(|(i, sample)| {
                        let value = i16::from_le_bytes([sample[0], sample[1]]) as f32 / 32768.0;
                        let window = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos());
                        Complex::new(value * window, 0.0)
                    })
                    .collect();
                fft.process(&mut input);
                let ranges = [(1usize, 2usize), (2, 6), (6, 18), (18, 60), (60, 200)];
                let weight = [1.2f32, 1.2, 1.5, 2.8, 5.0];
                let mut bands = [0.0f32; 5];
                for (idx, (start, end)) in ranges.iter().enumerate() {
                    let mut total = 0.0f32;
                    let mut count = 0.0f32;
                    for bin in *start..*end {
                        if bin >= N / 2 {
                            break;
                        }
                        total += input[bin].norm();
                        count += 1.0;
                    }
                    let mag = (total / count.max(1.0)) * weight[idx];
                    if mag > max_band[idx] {
                        max_band[idx] = mag;
                    } else {
                        max_band[idx] *= 0.99;
                    }
                    bands[idx] = (mag / max_band[idx].max(0.12)).min(1.0);
                }
                let _ = app.emit("audio-visualization", serde_json::json!({ "frequencies": bands }));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    });
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let quit = MenuItem::with_id(app, "quit", "Quit Bloom", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "Restart Bloom", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Open Settings", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings, &restart, &quit])?;
    let handle = app.handle().clone();
    TrayIconBuilder::new()
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("Bloom")
        .menu(&menu)
        .on_menu_event(move |_, event| match event.id().as_ref() {
            "quit" => {
                SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                handle.exit(0);
            }
            "restart" => {
                SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::Relaxed);
                handle.restart();
            }
            "settings" => commands::open_settings_window(handle.clone()),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                commands::open_settings_window(tray.app_handle().clone());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_is_only_forced_for_gnome_wayland() {
        assert!(should_prefer_x11("wayland", "ubuntu:GNOME", "", ":0"));
        assert!(!should_prefer_x11("wayland", "ubuntu:GNOME", "0", ":0"));
        assert!(!should_prefer_x11("wayland", "KDE", "", ":0"));
        assert!(!should_prefer_x11("x11", "ubuntu:GNOME", "", ":0"));
        assert!(!should_prefer_x11("wayland", "ubuntu:GNOME", "", ""));
    }

    #[test]
    fn text_scaling_is_inverted() {
        assert_eq!(dpi_scale_for("1.25\n").as_deref(), Some("0.8000"));
        assert_eq!(dpi_scale_for("1.0"), None);
    }
}
