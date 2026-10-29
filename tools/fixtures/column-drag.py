#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

"""Send an X11 pointer drag for the column GTK acceptance test without xdotool."""
import ctypes as c
import sys
import time

x11 = c.CDLL("libX11.so.6")
xtest = c.CDLL("libXtst.so.6")
pointer = c.c_void_p
window = c.c_ulong
x11.XOpenDisplay.argtypes = [c.c_char_p]
x11.XOpenDisplay.restype = pointer
x11.XDefaultRootWindow.argtypes = [pointer]
x11.XDefaultRootWindow.restype = window
x11.XQueryTree.argtypes = [pointer, window, c.POINTER(window), c.POINTER(window),
                           c.POINTER(c.POINTER(window)), c.POINTER(c.c_uint)]
x11.XFetchName.argtypes = [pointer, window, c.POINTER(c.c_char_p)]
x11.XTranslateCoordinates.argtypes = [pointer, window, window, c.c_int, c.c_int,
                                      c.POINTER(c.c_int), c.POINTER(c.c_int), c.POINTER(window)]
x11.XFlush.argtypes = [pointer]
x11.XRaiseWindow.argtypes = [pointer, window]
x11.XSetInputFocus.argtypes = [pointer, window, c.c_int, c.c_ulong]
x11.XFree.argtypes = [pointer]
x11.XCloseDisplay.argtypes = [pointer]
xtest.XTestFakeMotionEvent.argtypes = [pointer, c.c_int, c.c_int, c.c_int, c.c_ulong]
xtest.XTestFakeButtonEvent.argtypes = [pointer, c.c_uint, c.c_int, c.c_ulong]

display = x11.XOpenDisplay(None)
assert display, "X11 display is required"
try:
    root = x11.XDefaultRootWindow(display)
    children = c.POINTER(window)()
    count = c.c_uint()
    returned_root, parent = window(), window()
    assert x11.XQueryTree(display, root, c.byref(returned_root), c.byref(parent),
                          c.byref(children), c.byref(count))
    target = None
    try:
        for candidate in children[:count.value]:
            name = c.c_char_p()
            if x11.XFetchName(display, candidate, c.byref(name)) and name.value:
                if name.value.decode() == sys.argv[1]:
                    target = candidate
                x11.XFree(name)
    finally:
        x11.XFree(children)
    assert target is not None, "test window was not found"
    x11.XRaiseWindow(display, target)
    x11.XSetInputFocus(display, target, 1, 0)
    left, top, child = c.c_int(), c.c_int(), window()
    assert x11.XTranslateCoordinates(display, target, root, 0, 0,
                                     c.byref(left), c.byref(top), c.byref(child))
    x, y, delta = (int(value) for value in sys.argv[2:5])
    x += left.value
    y += top.value
    xtest.XTestFakeMotionEvent(display, -1, x, y, 0)
    x11.XFlush(display)
    time.sleep(0.1)
    xtest.XTestFakeButtonEvent(display, 1, 1, 0)
    x11.XFlush(display)
    time.sleep(0.1)
    for step in range(1, 11):
        xtest.XTestFakeMotionEvent(display, -1, x + round(delta * step / 10), y, 0)
        x11.XFlush(display)
        time.sleep(0.03)
    xtest.XTestFakeButtonEvent(display, 1, 0, 0)
    x11.XFlush(display)
finally:
    x11.XCloseDisplay(display)
