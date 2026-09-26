//! On-disk document: the map, where the camera was parked, and the theme.
//!
//! Saves are atomic (write a sibling temp file, then rename) so a crash mid-write
//! can never leave a half-written map behind.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::Graph;

/// Where the camera was left, so reopening resumes the same view.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pose {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

/// Where the window itself was left, in screen points.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl WindowRect {
    /// Reject a saved rectangle that could not be used: off-screen, degenerate,
    /// or non-finite. A window restored to somewhere with no display is a window
    /// the user cannot get back.
    pub fn usable(self) -> bool {
        [self.x, self.y, self.w, self.h].iter().all(|v| v.is_finite())
            && self.w >= 320.0
            && self.h >= 240.0
            && self.w <= 20_000.0
            && self.h <= 20_000.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doc {
    pub graph: Graph,
    #[serde(default)]
    pub camera: Option<Pose>,
    #[serde(default)]
    pub dark: bool,
    /// Window frame at the last save. Absent on a first run, and dropped on
    /// load if it does not describe a window anyone could reach.
    #[serde(default)]
    pub window: Option<WindowRect>,
}

impl Doc {
    pub fn new(graph: Graph, camera: Option<Pose>, dark: bool, window: Option<WindowRect>) -> Self {
        Self {
            graph,
            camera,
            dark,
            window,
        }
    }
}

/// Where this app keeps the maps it made itself, its recents list and its
/// keymap. Everything persisted hangs off this one directory, which is why the
/// development build can be kept away from real maps by changing one string.
fn data_dir() -> Option<PathBuf> {
    let mut p = crate::sandbox::home();
    if cfg!(target_os = "macos") {
        p.push("Library");
        p.push("Application Support");
    } else {
        p.push(".local");
        p.push("share");
    }
    p.push(if crate::dev::is_dev() {
        "jotmind-dev"
    } else {
        "jotmind"
    });
    Some(p)
}

/// The directory the image store sits in — one store for the app, not one per
/// document. Documents are app-managed and can be switched at any time, and the
/// store is content-addressed, so a picture pasted into one map resolves in
/// every other. (`images::path` appends the store's own subdirectory.)
pub fn images_root() -> PathBuf {
    data_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The map you get when you have never opened anything else.
pub fn home_path() -> Option<PathBuf> {
    data_dir().map(|mut p| {
        p.push("map.json");
        p
    })
}

/// An unused path in the data dir for File ▸ New: `untitled.json`, then
/// `untitled-2.json`, and so on — so a new map never lands on top of one that
/// is already there. `None` only when there is no home directory to write to.
pub fn new_document_path() -> Option<PathBuf> {
    let dir = data_dir()?;
    for n in 1..10_000 {
        let name = if n == 1 {
            "untitled.json".to_string()
        } else {
            format!("untitled-{n}.json")
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Which document a fresh launch opens: whatever was last opened, falling back
/// to the home map. `JOTMIND_FILE` overrides both — handy for scratch maps and
/// for the smoke run, which must not touch a real one.
pub fn startup_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("JOTMIND_FILE") {
        return Some(PathBuf::from(p));
    }
    recents()
        .into_iter()
        .find(|p| p.is_file())
        .or_else(home_path)
}

fn recents_path() -> Option<PathBuf> {
    data_dir().map(|mut p| {
        p.push("recent.json");
        p
    })
}

/// How many documents the Open Recent menu remembers.
const MAX_RECENTS: usize = 8;

/// Most recently opened first. Silently empty if the list is missing or bad —
/// a broken recents file is a convenience lost, never a document lost.
pub fn recents() -> Vec<PathBuf> {
    let Some(path) = recents_path() else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<PathBuf>>(&raw)
        .unwrap_or_default()
        .into_iter()
        .map(|p| crate::sandbox::restore(&p))
        .collect()
}

/// Put `path` at the head of the list. Writing this is best-effort: it is not
/// the user's work, so a failure must not surface as a warning about their map.
pub fn remember(path: &Path) {
    if let Err(err) = crate::sandbox::remember(path) {
        eprintln!("jotmind: could not bookmark document: {err}");
    }
    let Some(list_path) = recents_path() else {
        return;
    };
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let list = promote(recents(), canonical);
    if let Some(dir) = list_path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        let _ = fs::write(&list_path, json);
    }
}

fn keymap_path() -> Option<PathBuf> {
    data_dir().map(|mut p| {
        p.push("keymap.json");
        p
    })
}

/// The user's keyboard-shortcut overrides: action name → chord (e.g.
/// `"ToggleMinimap" -> "cmd-m"`). Silently empty if the file is missing or bad
/// — a broken keymap is the defaults, never a crash.
pub fn load_keymap() -> std::collections::HashMap<String, String> {
    let Some(path) = keymap_path() else {
        return Default::default();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return Default::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Persist the shortcut overrides. Best-effort: losing a customisation is a
/// convenience gone, never a document.
pub fn save_keymap(map: &std::collections::HashMap<String, String>) {
    let Some(path) = keymap_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = fs::write(&path, json);
    }
}

/// Forget every remembered document. Best-effort, like `remember`: a failure
/// to rewrite the list is a convenience lost, never a document lost.
pub fn clear_recents() {
    let Some(path) = recents_path() else {
        return;
    };
    let _ = fs::write(&path, "[]");
}

/// `path` to the head, once, with the tail bounded. Split from `remember` so
/// the ordering rules are testable without a real list on disk to write over.
fn promote(mut list: Vec<PathBuf>, path: PathBuf) -> Vec<PathBuf> {
    list.retain(|p| p != &path);
    list.insert(0, path);
    list.truncate(MAX_RECENTS);
    list
}

/// What the Open Recent menu shows for a path: the file's own name, and the
/// folder it sits in when that is the only thing telling two apart.
pub fn recent_label(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    match path.parent().and_then(|p| p.file_name()) {
        Some(dir) => format!("{name}  —  {}", dir.to_string_lossy()),
        None => name,
    }
}

pub fn load_from(path: &Path) -> io::Result<Doc> {
    let raw = fs::read_to_string(path)?;
    let mut doc: Doc =
        serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    // Never trust a file we did not write in this run.
    doc.graph.sanitize();
    if let Some(c) = doc.camera {
        if !(c.x.is_finite() && c.y.is_finite() && c.zoom.is_finite() && c.zoom > 0.0) {
            doc.camera = None;
        }
    }
    doc.window = doc.window.filter(|w| w.usable());
    Ok(doc)
}

pub fn save_to(path: &Path, doc: &Doc) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // `serde_json` writes a non-finite f32 as `null`, and `Pose`'s fields are
    // bare f32s, so the very next load fails to deserialise and the map opens
    // read-only for ever after. The loader already refuses a bad pose; the
    // check has to be on this side of the round trip too, or a single NaN
    // makes the document unopenable.
    let mut doc = Doc {
        graph: doc.graph.clone(),
        camera: doc.camera.filter(|c| {
            c.x.is_finite() && c.y.is_finite() && c.zoom.is_finite() && c.zoom > 0.0
        }),
        dark: doc.dark,
        window: doc.window.filter(|w| w.usable()),
    };
    doc.graph.drop_non_finite_positions();
    let json = serde_json::to_string_pretty(&doc)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    #[cfg(target_os = "macos")]
    {
        // Foundation creates an authorized replacement file for a document
        // chosen in NSOpenPanel; an arbitrary Rust sibling temp file is denied
        // by App Sandbox even when the destination itself is writable.
        use objc2_foundation::{NSData, NSDataWritingOptions, NSString, NSURL};
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        NSData::with_bytes(json.as_bytes()).writeToURL_options_error(&url, NSDataWritingOptions::Atomic)
            .map_err(|e| io::Error::other(e.to_string()))?;
        fs::File::open(path)?.sync_all()?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Atomic *and* durable: fsync the temp file's bytes before the rename, then
        // fsync the directory so the rename entry itself survives a power cut. A
        // plain write+rename can leave both the temp and the target empty if the
        // machine loses power in the window between them.
        use std::io::Write;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(json.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        if let Some(dir) = path.parent() {
            if let Ok(dir) = fs::File::open(dir) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    }
}

/// What happened when we tried to open the saved map.
pub enum Load {
    /// Opened it. This session owns the file.
    Opened(Doc),
    /// Nothing saved yet — first run. This session owns the file.
    Fresh,
    /// The file is there but unreadable (bad JSON, permissions, IO). We must
    /// **not** write over it: the user's map may still be recoverable by hand.
    Unreadable(String),
}

/// Classify what a specific path holds. The never-clobber-an-unreadable-file
/// rule lives here so it is testable.
pub fn load_path(path: &Path) -> Load {
    match load_from(path) {
        Ok(doc) => Load::Opened(doc),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Load::Fresh,
        Err(e) => {
            let msg = format!("could not read {}: {e}", path.display());
            eprintln!("mind-map-rust: {msg}");
            eprintln!("mind-map-rust: opening read-only so the file is not overwritten");
            Load::Unreadable(msg)
        }
    }
}

/// Write the map to the document that is open. `Err` carries a message fit for
/// the status line — the app runs as a bundle, so nobody ever sees stderr and a
/// silent failure is indistinguishable from a save.
pub fn save(path: &Path, doc: &Doc) -> Result<(), String> {
    save_to(path, doc).map_err(|e| format!("could not save: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Direction;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    fn scratch(name: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "mind-map-test-{}-{n}-{name}.json",
            std::process::id()
        ));
        p
    }

    #[test]
    fn the_recents_list_promotes_without_duplicating_and_stays_bounded() {
        let p = |s: &str| PathBuf::from(s);
        // Re-opening something already in the list moves it up rather than
        // adding a second row that opens the same file.
        let list = promote(vec![p("/a"), p("/b"), p("/c")], p("/c"));
        assert_eq!(list, vec![p("/c"), p("/a"), p("/b")]);

        // The menu is a shortcut, not a history: it stops at MAX_RECENTS.
        let mut list = Vec::new();
        for i in 0..(MAX_RECENTS + 5) {
            list = promote(list, p(&format!("/map{i}")));
        }
        assert_eq!(list.len(), MAX_RECENTS);
        assert_eq!(list[0], p(&format!("/map{}", MAX_RECENTS + 4)));
    }

    #[test]
    fn roundtrip_preserves_map_and_camera() {
        let path = scratch("roundtrip");
        let mut g = Graph::demo();
        g.enter_edit();
        g.set_focused_text("edited root".into());
        let doc = Doc::new(
            g.clone(),
            Some(Pose {
                x: 12.5,
                y: -3.0,
                zoom: 1.75,
            }),
            true,
            None,
        );
        save_to(&path, &doc).expect("save");

        let back = load_from(&path).expect("load");
        assert_eq!(back.graph.nodes.len(), g.nodes.len());
        assert_eq!(back.graph.edges.len(), g.edges.len());
        assert_eq!(back.graph.focused().text, "edited root");
        assert!(back.dark);
        let cam = back.camera.expect("camera");
        assert!((cam.zoom - 1.75).abs() < 1e-6);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_replaces_previous_and_leaves_no_temp() {
        let path = scratch("atomic");
        save_to(&path, &Doc::new(Graph::new("one"), None, false, None)).unwrap();
        save_to(&path, &Doc::new(Graph::new("two"), None, false, None)).unwrap();
        assert_eq!(load_from(&path).unwrap().graph.focused().text, "two");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_not_an_error_state() {
        let path = scratch("missing");
        assert!(load_from(&path).is_err());
    }

    #[test]
    fn corrupt_file_is_rejected_not_panicked_on() {
        let path = scratch("corrupt");
        fs::write(&path, "{ not json at all").unwrap();
        assert!(load_from(&path).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn an_unreadable_file_is_never_treated_as_a_fresh_start() {
        // The caller uses Fresh to mean "this session owns the file and may
        // autosave over it". Confusing a corrupt map for a missing one would
        // overwrite a map the user could still rescue by hand.
        let path = scratch("unreadable");
        assert!(matches!(load_path(&path), Load::Fresh), "missing -> Fresh");

        fs::write(&path, "{\"graph\": OOPS}").unwrap();
        match load_path(&path) {
            Load::Unreadable(msg) => assert!(msg.contains("could not read")),
            _ => panic!("corrupt file must report Unreadable, not Fresh/Opened"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_dropped_bookkeeping_field_still_loads() {
        // README promises the file is safe to hand-edit; losing one line must
        // not cost the map. Everything but `nodes` is derivable by sanitize.
        let path = scratch("partial");
        fs::write(
            &path,
            r#"{"graph":{"nodes":{"1":{"text":"kept","x":0,"y":0,"parent":null}}}}"#,
        )
        .unwrap();
        let doc = load_from(&path).expect("partial file loads");
        assert_eq!(doc.graph.nodes.len(), 1);
        assert_eq!(doc.graph.focused().text, "kept");
        assert_eq!(doc.graph.root_id, 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn the_window_frame_survives_a_round_trip_and_a_bad_one_does_not() {
        let path = scratch("window");
        let rect = WindowRect { x: 120.0, y: 64.0, w: 1400.0, h: 900.0 };
        save_to(&path, &Doc::new(Graph::new("root"), None, false, Some(rect))).unwrap();
        let back = load_from(&path).unwrap().window.expect("a frame");
        assert_eq!((back.x, back.y, back.w, back.h), (120.0, 64.0, 1400.0, 900.0));

        // A frame nobody could use is dropped rather than restored: a window
        // reopened at 1x1, or at a NaN origin, is a window you cannot get back.
        for bad in [
            WindowRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 },
            WindowRect { x: f32::NAN, y: 0.0, w: 900.0, h: 700.0 },
            WindowRect { x: 0.0, y: 0.0, w: 900.0, h: f32::INFINITY },
        ] {
            save_to(&path, &Doc::new(Graph::new("root"), None, false, Some(bad))).unwrap();
            assert!(load_from(&path).unwrap().window.is_none(), "{bad:?} was kept");
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_non_finite_pose_never_reaches_the_file() {
        // serde_json writes a non-finite f32 as `null`, and Pose's fields are
        // bare f32s — so one NaN written out means the map never loads again.
        let path = scratch("nonfinite");
        let g = Graph::new("root");
        let doc = Doc::new(
            g,
            Some(Pose {
                x: f32::INFINITY,
                y: f32::NAN,
                zoom: 1.0,
            }),
            false,
            None,
        );
        save_to(&path, &doc).expect("save");
        // The camera has to be absent, not present-with-nulls: `"camera": {"x":
        // null}` is what makes the next load fail outright.
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(raw["camera"].is_null(), "the pose was written: {}", raw["camera"]);
        let back = load_from(&path).expect("the file must still load");
        assert!(back.camera.is_none(), "the bad pose was dropped, not written");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn a_non_finite_node_position_never_reaches_the_file() {
        let path = scratch("nonfinite-node");
        let mut g = Graph::new("root");
        g.nodes.get_mut(&g.root_id).unwrap().x = f32::NAN;
        save_to(&path, &Doc::new(g, None, false, None)).expect("save");
        let back = load_from(&path).expect("the file must still load");
        assert!(back.graph.nodes.values().all(|n| n.x.is_finite()));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn hand_edited_file_is_sanitized_on_load() {
        let path = scratch("sanitize");
        let mut g = Graph::new("root");
        g.create_from_focused(Direction::Right, "child");
        let doc = Doc::new(g, None, false, None);
        let mut raw: serde_json::Value = serde_json::to_value(&doc).unwrap();
        // Point an edge at a node that does not exist, and focus a ghost.
        raw["graph"]["edges"]["3"]["to"] = serde_json::json!(9999);
        raw["graph"]["focused_id"] = serde_json::json!(4242);
        fs::write(&path, serde_json::to_string(&raw).unwrap()).unwrap();

        let back = load_from(&path).expect("load");
        assert_eq!(back.graph.focused_id, back.graph.root_id, "focus repaired");
        // The dangling edge goes and the parent link it should have described
        // is rebuilt, so nothing is drawn to a node that is not there and no
        // child is left with no line into it.
        assert_eq!(back.graph.edges.len(), 1, "dangling edge replaced, not just dropped");
        let e = back.graph.edges.values().next().unwrap();
        assert_eq!(e.from, back.graph.root_id);
        assert_eq!(back.graph.nodes[&e.to].parent, Some(back.graph.root_id));
        let _ = fs::remove_file(&path);
    }
}
