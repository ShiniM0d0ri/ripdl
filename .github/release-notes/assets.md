**Windows**: `...-windows-setup.exe` installs ripdl (the GUI auto-spawns the
daemon on first run). `...-windows-portable.zip` is unzip-and-run - no admin
needed.

**Linux**: the `.deb` / `.rpm` install CLI + daemon + GUI with runtime
dependencies declared. Portable tar users need the GUI runtime installed for
the window to start (`libwebkit2gtk-4.1-0 libgtk-3-0
libayatana-appindicator3`). The CLI and daemon alone need nothing beyond libc.

**Browser extension**: `ripdl-browser-extension-*.zip` - unzip and load it via
chrome://extensions > Developer mode > Load unpacked (see ext/README.md inside
the zip).
