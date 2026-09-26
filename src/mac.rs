//! Two things gpui 0.2.2 has no API for, both reached through AppKit directly.
//!
//! * The Help menu's search field. AppKit installs it into whichever menu the
//!   app names as its help menu, and gpui never names one.
//! * Hiding the window's close / minimise / zoom buttons. `TitlebarOptions`
//!   can move them but not take them away, and this app wants them absent
//!   until the pointer goes looking for them.
//!
//! Everything here runs on the main thread, from the render pass or from
//! startup, and is a no-op anywhere the window is not up yet.

#[cfg(target_os = "macos")]
pub fn use_help_menu() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(main) = app.mainMenu() else {
        return;
    };
    for i in 0..main.numberOfItems() {
        let Some(item) = main.itemAtIndex(i) else {
            continue;
        };
        if item.title().to_string() == "Help" {
            if let Some(submenu) = item.submenu() {
                app.setHelpMenu(Some(&submenu));
            }
            return;
        }
    }
}

/// Show or hide the traffic lights. Returns whether a window was there to act
/// on — the caller uses that to know whether the state actually took, since the
/// first render can beat the window into existence.
#[cfg(target_os = "macos")]
pub fn set_traffic_lights(visible: bool) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowButton};

    const BUTTONS: [NSWindowButton; 3] = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    let mut touched = false;
    for i in 0..windows.count() {
        let window = windows.objectAtIndex(i);
        for button in BUTTONS {
            if let Some(button) = window.standardWindowButton(button) {
                button.setHidden(!visible);
                touched = true;
            }
        }
    }
    touched
}

/// The App-menu items gpui 0.2.2 has no API for — the standard About panel and
/// the Hide / Hide-Others / Show-All block every Mac app carries. Sent straight
/// to the shared NSApplication. `about_panel` builds its panel from the bundle's
/// Info.plist.
#[cfg(target_os = "macos")]
pub fn about_panel() {
    nsapp(|app, nil| unsafe {
        let _: () = objc2::msg_send![app, orderFrontStandardAboutPanel: nil];
    });
}

#[cfg(target_os = "macos")]
pub fn hide_app() {
    nsapp(|app, nil| unsafe {
        let _: () = objc2::msg_send![app, hide: nil];
    });
}

#[cfg(target_os = "macos")]
pub fn hide_others() {
    nsapp(|app, nil| unsafe {
        let _: () = objc2::msg_send![app, hideOtherApplications: nil];
    });
}

#[cfg(target_os = "macos")]
pub fn show_all() {
    nsapp(|app, nil| unsafe {
        let _: () = objc2::msg_send![app, unhideAllApplications: nil];
    });
}

/// Run `f` with the shared NSApplication and a nil sender, on the main thread.
#[cfg(target_os = "macos")]
fn nsapp(f: impl FnOnce(&objc2_app_kit::NSApplication, *mut objc2::runtime::AnyObject)) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    f(&app, std::ptr::null_mut());
}

#[cfg(not(target_os = "macos"))]
pub fn about_panel() {}
#[cfg(not(target_os = "macos"))]
pub fn hide_app() {}
#[cfg(not(target_os = "macos"))]
pub fn hide_others() {}
#[cfg(not(target_os = "macos"))]
pub fn show_all() {}

#[cfg(not(target_os = "macos"))]
pub fn use_help_menu() {}

#[cfg(not(target_os = "macos"))]
pub fn set_traffic_lights(_visible: bool) -> bool {
    true
}
