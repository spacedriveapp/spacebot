import AppKit
import SwiftRs

@_cdecl("set_titlebar_style")
public func setTitlebarStyle(window: NSWindow, fullScreen: Bool) {
    window.titlebarAppearsTransparent = true
    if fullScreen {
        window.toolbar = nil
    } else {
        // Invisible toolbar pads the traffic lights down from the top edge
        let toolbar = NSToolbar(identifier: "window_invisible_toolbar")
        toolbar.showsBaselineSeparator = false
        window.toolbar = toolbar
    }
    window.titleVisibility = fullScreen ? .visible : .hidden
}

@_cdecl("lock_app_theme")
public func lockAppTheme(themeType: Int) {
    let theme: NSAppearance?
    switch themeType {
    case 0:
        theme = NSAppearance(named: .aqua)
    case 1:
        theme = NSAppearance(named: .darkAqua)
    default:
        theme = nil
    }
    NSApp.appearance = theme
}
