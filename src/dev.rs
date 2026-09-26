//! Which copy of the app this is.
//!
//! There are two on this Mac: the one in `/Applications` holding real maps, and
//! the one being worked on. They must not share a data directory — a
//! development build that autosaves over a real map has destroyed something no
//! test catches.
//!
//! The question is asked of the executable itself rather than passed in at
//! launch. `open` forwards arguments to a bundle but not environment, and the
//! app can also be started from the Finder or from Spotlight — a marker that
//! only survives one of those launch paths is a marker that will one day let a
//! dev build write to a real map. The name of the running executable survives
//! all of them.
//!
//! `scripts/build-app.sh --dev` builds `dist/mind67Dev.app`, whose executable
//! is `mind67-dev`. `JOTMIND_DEV=1` says the same thing for `cargo run`, which
//! has no bundle at all.

/// True when this is the development build, which keeps its maps, its recents
/// list and its keymap beside the real ones rather than in them.
pub fn is_dev() -> bool {
    if std::env::var("JOTMIND_DEV").is_ok_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    std::env::current_exe().is_ok_and(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with("-dev"))
    })
}

/// What the window's title bar says, so the two copies are told apart on screen
/// as well as on the Dock.
pub fn app_title() -> &'static str {
    if is_dev() {
        "mind67 Dev"
    } else {
        "mind67"
    }
}
