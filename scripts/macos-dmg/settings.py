# dmgbuild settings for the first-install DMG (#993). `defines` comes from make-dmg.sh:
#   app         path to the stapled Okilum.app
#   icon        volume icon (the app's Okilum.icns)
#   background  background.png; dmgbuild pairs it with background@2x.png for Retina
import os.path

application = defines['app']  # noqa: F821 - provided by dmgbuild
format = 'UDZO'
filesystem = 'HFS+'
files = [application]
symlinks = {'Applications': '/Applications'}
icon = defines['icon']  # noqa: F821
background = defines['background']  # noqa: F821
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
default_view = 'icon-view'
window_rect = ((200, 120), (660, 400))
icon_size = 128
text_size = 13
arrange_by = None
icon_locations = {
    os.path.basename(application): (165, 200),
    'Applications': (495, 200),
}
# No hide_extensions: it writes Finder info onto the signed bundle, which
# `codesign --verify --strict` rejects. Finder hides `.app` extensions anyway.
