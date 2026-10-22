# AGENTS.md

Tauri v2 app: Rust backend + React 19/TypeScript frontend. Replaces the native Windows taskbar (dock) and adds a notch/island UI. The same UI runs on Linux (GNOME, including Ubuntu 26.04) through `src-tauri/src/linux/`. Bun is the package manager. CI runs on `windows-latest` and checks the Linux target on `ubuntu-latest`.

## Commands

Run from the repo root unless noted. Bun 1.4.0, Rust stable.

- `bun install` — deps (CI uses `bun install --frozen-lockfile`; never create a root `package-lock.json`)
- `bun run tauri dev` — full dev loop. Vite is on strict port 1420; do not start `bun run dev` separately
- `bun run build` — `tsc && vite build`; this is the frontend typecheck. `strict` + `noUnusedLocals`/`noUnusedParameters` fail the build. There is no root lint or frontend test script
- `cargo check --locked` and `cargo clippy --locked --all-targets` in `src-tauri` — CI's Rust gates
- `cargo test --locked` in `src-tauri` — pure-function unit tests. Runs fast on Windows but CI does **not** run it, so run it yourself
- `bun run bump <x.y.z>` — bumps `package.json`, `tauri.conf.json`, `Cargo.toml`, `Cargo.lock` together
- `bun run release <x.y.z>` — bump + commit + tag + push, must be on `main`. The release workflow fails unless all four version files match the tag. Before/after releasing, follow `RELEASE_CHECKLIST.md` (updater manifest verification, N‑1 install smoke test)
- `website/` is a separate static site: use `npm install` / `npm run dev` there. Root scripts and CI do not build it

## Architecture

- Four webview windows, each a separate Vite/HTML entry: `main` = notch (`index.html` → `src/main.tsx` → `src/App.tsx`), `dock` (`dock.html` → `src/Dock.tsx`), `overlay` (`overlay.html` → `src/Overlay.tsx`), `settings` (`settings.html` → `src/Settings.tsx`). Adding or renaming a window means editing `src-tauri/tauri.conf.json`, `vite.config.ts` inputs, the HTML file, and `src-tauri/capabilities/default.json`
- Rust entrypoint is `src-tauri/src/main.rs`; `lib.rs` is an empty stub. `main()` dispatches to `windows_main()` or `linux::main()`. Every `#[tauri::command]` must be registered in both `generate_handler!` lists (`main.rs` and `src/linux/mod.rs`)
- Windows: `commands.rs` = IPC surface; `services.rs` = long-lived Win32 hooks/workers (keyboard hook, thumbnail capture, audio visualizer, AppBar registration); `utils.rs` re-exports the settings cache. Linux: `src/linux/commands.rs` is the IPC surface and `src/linux/host.rs` talks to the desktop (GNOME extension, X11, nmcli, wpctl, bluetoothctl, UPower, logind). `settings_file.rs`, `state.rs` and `types.rs` are shared
- Linux settings live in the Tauri config dir (`$XDG_CONFIG_HOME/bloom/settings.json`). The GNOME extension `bloom@sehaz.space` is written to `~/.local/share/gnome-shell/extensions/` on launch; window list, focus, close, overview, notifications and thumbnails go through it. `BLOOM_X11=0` keeps native Wayland. `BLOOM_LAYER_SHELL=0` skips gtk-layer-shell. On GNOME Wayland the process forces `GDK_BACKEND=x11` so the dock can be placed (same approach as Coucou)
- Backend→frontend events use `app.emit`: `settings-changed`, `settings-external-changed`, `media-update`, `volume-change`, `brightness-change`, `dock-overlap`, `notch-overlap`, `windows-changed`, `update-available`, `auto-update-status`. Listen via `@tauri-apps/api/event`

## Settings

- Stored as a flat object of `bloom-`-prefixed keys; all values are strings (`"true"`/`"false"` for booleans). Windows: `%APPDATA%/bloom/settings.json`. Linux: `$XDG_CONFIG_HOME/bloom/settings.json`. `SETTINGS.md` documents every key
- UI write path: set `localStorage`, then `invoke("save_setting")` → backend caches it and emits `settings-changed`. External file edits are watched and emitted as `settings-external-changed`
- `src/hooks/useSettingsSync.ts` is the single sync hook for both events and normalizes string booleans/numbers
- The CSP in `tauri.conf.json` allow-lists network origins (weather APIs only); add new origins there. New Tauri plugins also need permissions in `capabilities/default.json`

## Gotchas

- On Windows the app hides the native taskbar and registers AppBars while running. Any new Windows exit/restart path must restore the taskbar and unregister AppBars, like the tray menu in `main.rs` does. A marker file restores it after a crash. Linux does not hide the Ubuntu dock
- The `windows` crate is pinned to 0.61 with an explicit feature list in `Cargo.toml`; add features there when using new Win32 APIs. Linux build packages: `build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf libssl-dev`. Runtime: NetworkManager, PipeWire (`wpctl`, `parec`), BlueZ, UPower, and `libgtk-layer-shell0` on wlroots/KDE/COSMIC
- Commits follow conventional commits (`feat:`, `fix:`); release notes are generated from them and commits touching only `website/*`, `.github/*`, or `*.md` are filtered out
