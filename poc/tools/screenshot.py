#!/usr/bin/env python3
"""Capture a real screenshot of this Wayland/GNOME session via the
xdg-desktop-portal Screenshot request.

Why this exists: grim/xdotool/GNOME's private Shell.Screenshot D-Bus method
all fail or are refused in this session (grim needs wlr-screencopy, which
Mutter doesn't implement; the private Shell.Screenshot interface is
access-denied by design so callers can't bypass portal consent). The actual
xdg-desktop-portal.Screenshot method DOES work here -- the only reason
earlier one-shot tools (`gdbus call`, etc.) appeared to hang/fail is that
they close their D-Bus connection right after getting the request handle
back, before the portal's async Response signal is ever emitted. This
script keeps one connection open to receive that signal.

Usage:
    python3 screenshot.py [output.png] [--delay SECONDS] [--interactive]
"""
import argparse
import shutil
import sys
import time
from pathlib import Path
from urllib.parse import urlparse, unquote

from gi.repository import Gio, GLib


def capture(output_path: Path, interactive: bool = False, timeout_s: int = 15) -> Path:
    loop = GLib.MainLoop()
    result = {}

    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    unique_name = bus.get_unique_name()
    sender_token = unique_name[1:].replace(".", "_")
    handle_token = f"neovibe_shot_{int(time.time() * 1000) % 1_000_000}"
    request_path = f"/org/freedesktop/portal/desktop/request/{sender_token}/{handle_token}"

    def on_response(connection, sender_name, object_path, interface_name, signal_name, parameters, user_data):
        code, results = parameters.unpack()
        result["code"] = code
        result["results"] = results
        loop.quit()

    sub_id = bus.signal_subscribe(
        "org.freedesktop.portal.Desktop",
        "org.freedesktop.portal.Request",
        "Response",
        request_path,
        None,
        Gio.DBusSignalFlags.NONE,
        on_response,
        None,
    )

    options = {
        "handle_token": GLib.Variant("s", handle_token),
        "interactive": GLib.Variant("b", interactive),
    }

    reply = bus.call_sync(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        GLib.Variant("(sa{sv})", ("", options)),
        GLib.VariantType("(o)"),
        Gio.DBusCallFlags.NONE,
        -1,
        None,
    )
    returned_path = reply.unpack()[0]
    if returned_path != request_path:
        # Portal can rewrite the token if collided; resubscribe to the real path.
        bus.signal_unsubscribe(sub_id)
        sub_id = bus.signal_subscribe(
            "org.freedesktop.portal.Desktop",
            "org.freedesktop.portal.Request",
            "Response",
            returned_path,
            None,
            Gio.DBusSignalFlags.NONE,
            on_response,
            None,
        )

    def on_timeout():
        loop.quit()
        return False

    timeout_id = GLib.timeout_add_seconds(timeout_s, on_timeout)
    loop.run()
    GLib.source_remove(timeout_id)
    bus.signal_unsubscribe(sub_id)

    if "code" not in result:
        raise TimeoutError(
            f"No Response signal from portal after {timeout_s}s "
            "(a permission dialog may be waiting for a human to click it)"
        )
    if result["code"] != 0:
        raise RuntimeError(f"Screenshot request failed/cancelled, code={result['code']}")

    uri = result["results"]["uri"]
    src_path = Path(unquote(urlparse(uri).path))

    output_path.parent.mkdir(parents=True, exist_ok=True)
    if src_path != output_path:
        shutil.move(str(src_path), str(output_path))
    return output_path


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("output", nargs="?", default=None, help="Output PNG path (default: timestamped in cwd)")
    parser.add_argument("--delay", type=float, default=0.0, help="Seconds to sleep before capturing")
    parser.add_argument("--interactive", action="store_true", help="Let the user pick a region/window (shows UI)")
    args = parser.parse_args()

    if args.delay > 0:
        time.sleep(args.delay)

    output_path = Path(args.output) if args.output else Path(f"screenshot-{int(time.time())}.png")
    try:
        saved = capture(output_path, interactive=args.interactive)
    except Exception as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)
    print(str(saved))


if __name__ == "__main__":
    main()
