# Persistent AT-SPI helper. One JSON command per stdin line, one JSON line out.
# ponytail: Super+h is the minimize fallback when a frame has no iconify action.
# It can hit the wrong window if AT-SPI marks a background frame active. The
# upgrade is the GNOME extension's Meta.Window.minimize once the shell loads it.
import json
import shutil
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

Atspi.init()

SKIP_APPS = {"gnome-shell", "mutter-x11-frames", "bloom", "gjs"}
SKIP_TITLES = {"bloom dock", "bloom", "overlay"}


def flags(acc):
    st = acc.get_state_set()

    def has(name):
        return bool(st.contains(getattr(Atspi.StateType, name)))

    return {
        "active": has("ACTIVE"),
        "focused": has("FOCUSED"),
        "iconified": has("ICONIFIED"),
        "showing": has("SHOWING"),
    }


def frames():
    desktop = Atspi.get_desktop(0)
    found = []
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app is None:
            continue
        app_name = app.get_name() or ""
        low = app_name.lower()
        if low in SKIP_APPS or "com.sehaz.bloom" in low:
            continue
        counts = {}
        for j in range(app.get_child_count()):
            child = app.get_child_at_index(j)
            if child is None:
                continue
            try:
                if child.get_role_name() != "frame":
                    continue
            except Exception:
                continue
            title = child.get_name() or ""
            if not title.strip() or title.lower() in SKIP_TITLES:
                continue
            key = (app_name, title)
            index = counts.get(key, 0)
            counts[key] = index + 1
            row = {"app": app_name, "title": title, "index": index}
            row.update(flags(child))
            found.append((child, row))
    return found


def foreground(st):
    return st["showing"] and not st["iconified"] and (st["focused"] or st["active"])


def grab(acc):
    try:
        Atspi.Component.grab_focus(acc)
    except Exception:
        try:
            acc.grab_focus()
        except Exception:
            pass


def minimize_action(acc):
    try:
        count = Atspi.Action.get_n_actions(acc)
    except Exception:
        return False
    for i in range(count):
        try:
            name = (Atspi.Action.get_action_name(acc, i) or "").lower()
        except Exception:
            continue
        if "minimiz" in name or "iconif" in name:
            try:
                Atspi.Action.do_action(acc, i)
                return True
            except Exception:
                return False
    return False


def reply(payload):
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def do_list():
    rows = [row for _, row in frames()]
    rows.sort(key=lambda row: row["iconified"])
    reply({"ok": True, "windows": rows})


def do_toggle(app, title, index):
    target = None
    for acc, row in frames():
        if row["app"] == app and row["title"] == title and row["index"] == index:
            target = acc
            break
    if target is None:
        reply({"ok": False})
        return
    st = flags(target)
    if not foreground(st):
        grab(target)
        reply({"ok": True})
        return
    if minimize_action(target):
        reply({"ok": True})
        return
    grab(target)
    time.sleep(0.12)
    st = flags(target)
    if foreground(st) and shutil.which("wtype"):
        subprocess.run(["wtype", "-M", "super", "-k", "h", "-m", "super"], check=False)
    reply({"ok": True})


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
        cmd = req.get("cmd")
        if cmd == "list":
            do_list()
        elif cmd == "toggle":
            do_toggle(req.get("app", ""), req.get("title", ""), int(req.get("index", 0)))
        else:
            reply({"ok": False})
    except Exception:
        reply({"ok": False})
