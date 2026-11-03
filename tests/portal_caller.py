#!/usr/bin/env python3
"""A GTK 3 app asking xdg-desktop-portal for a file over its own window, for tests/real_portal.rs.

Run with GDK_BACKEND=x11 and GTK_USE_PORTAL=1, so the request carries `x11:<its window id>` as
the parent. Prints `caller xid=0x..` once the window is up, then `picked <path>` or
`cancelled`, and exits.
"""

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("GdkX11", "3.0")
from gi.repository import GdkX11, GLib, Gtk  # noqa: E402

window = Gtk.Window(title="Portal caller")
window.set_default_size(900, 600)
window.connect("destroy", Gtk.main_quit)
window.show_all()


def done(dialog, response):
    if response == Gtk.ResponseType.ACCEPT:
        print(f"picked {dialog.get_filename()}", flush=True)
    else:
        print("cancelled", flush=True)
    Gtk.main_quit()


def ask():
    gdk_window = window.get_window()
    if not isinstance(gdk_window, GdkX11.X11Window):
        print("not on X11", flush=True)
        Gtk.main_quit()
        return False
    print(f"caller xid=0x{gdk_window.get_xid():x}", flush=True)
    dialog = Gtk.FileChooserNative.new("Open a report", window, Gtk.FileChooserAction.OPEN, None, None)
    dialog.set_modal(True)
    dialog.connect("response", done)
    dialog.show()
    window.dialog = dialog
    return False


# Mapped first, so the window id it exports is one the window manager knows.
GLib.timeout_add(500, ask)
Gtk.main()
