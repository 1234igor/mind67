//! One short native sheet, shown until the user acknowledges it.
pub fn show_once(window: &mut gpui::Window, cx: &mut gpui::App) {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSString, NSUserDefaults};
        let key = NSString::from_str("welcomeAcknowledgedV1");
        if NSUserDefaults::standardUserDefaults().boolForKey(&key) {
            return;
        }
        let answer = window.prompt(gpui::PromptLevel::Info, "Welcome to mind67",
            Some("Type to name an idea.\nTab adds a child; Return adds a sibling.\nYour map saves automatically. ⌘/ shows shortcuts."), &["Get Started"], cx);
        cx.spawn(async move |_| {
            if answer.await == Ok(0) {
                NSUserDefaults::standardUserDefaults()
                    .setBool_forKey(true, &NSString::from_str("welcomeAcknowledgedV1"));
            }
        })
        .detach();
    }
}
