import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const IFACE = `<node>
  <interface name="space.sehaz.Bloom">
    <method name="GetState">
      <arg type="s" direction="out" name="json"/>
    </method>
    <method name="Activate">
      <arg type="t" direction="in" name="id"/>
    </method>
    <method name="Close">
      <arg type="t" direction="in" name="id"/>
    </method>
    <method name="Thumbnail">
      <arg type="t" direction="in" name="id"/>
      <arg type="s" direction="out" name="png"/>
    </method>
    <method name="Overview"/>
    <method name="Notifications"/>
  </interface>
</node>`;

function eligible(win) {
    const type = win.get_window_type();
    if (type !== Meta.WindowType.NORMAL && type !== Meta.WindowType.DIALOG)
        return false;
    if (win.is_skip_taskbar())
        return false;
    const wm = (win.get_wm_class() || '').toLowerCase();
    return wm !== 'bloom' && !wm.includes('com.sehaz.bloom');
}

function findWindow(id) {
    const wanted = Number(id);
    for (const actor of global.get_window_actors()) {
        const win = actor.meta_window;
        if (win && win.get_id() === wanted)
            return win;
    }
    return null;
}

export default class BloomExtension extends Extension {
    enable() {
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, '/space/sehaz/Bloom');
        this._nameId = Gio.DBus.session.own_name(
            'space.sehaz.Bloom',
            Gio.BusNameOwnerFlags.REPLACE,
            null,
            null);
        try {
            Gio._promisify(Shell.Screenshot.prototype, 'screenshot_area', 'screenshot_area_finish');
        } catch (e) {
            log(`bloom: screenshot promisify failed: ${e}`);
        }
    }

    disable() {
        if (this._nameId)
            Gio.DBus.session.unown_name(this._nameId);
        this._dbus?.unexport();
        this._dbus = null;
    }

    GetState() {
        const tracker = Shell.WindowTracker.get_default();
        const focus = global.display.get_focus_window();
        const windows = [];
        for (const actor of global.get_window_actors()) {
            const win = actor.meta_window;
            if (!win || !eligible(win))
                continue;
            const app = tracker.get_window_app(win);
            const maximized = win.get_maximized();
            windows.push({
                id: win.get_id(),
                app_id: app ? app.get_id() : (win.get_wm_class() || 'unknown'),
                name: app ? app.get_name() : (win.get_title() || 'Window'),
                title: win.get_title() || '',
                focused: win === focus,
                maximized: maximized !== 0,
                fullscreen: win.is_fullscreen(),
            });
        }
        return JSON.stringify({windows});
    }

    Activate(id) {
        const win = findWindow(id);
        if (!win)
            return;
        if (win.minimized) {
            const workspace = win.get_workspace();
            if (workspace)
                workspace.activate(global.get_current_time());
            win.unminimize();
            win.activate(global.get_current_time());
            return;
        }
        if (win.has_focus()) {
            win.minimize();
            return;
        }
        const workspace = win.get_workspace();
        if (workspace)
            workspace.activate(global.get_current_time());
        win.activate(global.get_current_time());
    }

    Close(id) {
        findWindow(id)?.delete(global.get_current_time());
    }

    Overview() {
        Main.overview.toggle();
    }

    Notifications() {
        const menu = Main.panel.statusArea.dateMenu?.menu;
        if (menu)
            menu.toggle();
    }

    async ThumbnailAsync([id], invocation) {
        let png = '';
        try {
            const win = findWindow(id);
            if (win && !win.minimized) {
                const rect = win.get_frame_rect();
                const path = `${GLib.get_user_runtime_dir()}/bloom-thumb-${id}.png`;
                const shot = new Shell.Screenshot();
                await shot.screenshot_area(
                    rect.x, rect.y,
                    Math.max(rect.width, 1), Math.max(rect.height, 1),
                    path);
                const [, bytes] = GLib.file_get_contents(path);
                png = GLib.base64_encode(bytes);
            }
        } catch (e) {
            log(`bloom: thumbnail failed: ${e}`);
        }
        invocation.return_value(new GLib.Variant('(s)', [png]));
    }
}
