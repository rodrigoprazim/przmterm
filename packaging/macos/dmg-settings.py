# The disk image's window, for dmgbuild (https://dmgbuild.readthedocs.io).
# scripts/macos-app.sh runs it as:
#
#   dmgbuild -s packaging/macos/dmg-settings.py -D app=<PrzmTerm.app>
#            -D background=<tiff> -D icon=<icns> "<volume name>" <dmg>
#
# dmgbuild writes the window's layout itself, without Finder, so it works
# on a build machine nobody is logged in to. The background is drawn by
# scripts/installer-art.py; the icon positions below must match its ICON_Y,
# APP_X and APPLICATIONS_X.
#
# `defines` is given to this file by dmgbuild.
# ruff: noqa: F821

import os.path

app = defines["app"]
app_name = os.path.basename(app)

format = "UDZO"
filesystem = "HFS+"
files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]
background = defines["background"]

# 660 x 420 of picture, plus the title bar.
window_rect = ((200, 140), (660, 448))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = True

icon_size = 100
text_size = 13
arrange_by = None
icon_locations = {
    app_name: (140, 235),
    "Applications": (520, 235),
}
