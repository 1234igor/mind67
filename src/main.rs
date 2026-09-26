//! mind67 — a keyboard-first mind map on a free, zoomable canvas.
//! Stack: Rust + GPUI (same defining UI framework as Zed). No Zed source copied.

mod onboarding;
mod sandbox;
mod camera;
mod convert;
mod dev;
mod image_bank;
mod images;
mod export_svg;
mod graph;
mod history;
mod mac;
mod markdown;
mod paint;
mod perf;
mod pinch;
mod store;
mod theme;
mod ui;

use std::borrow::Cow;
use std::env;

use gpui::{
    actions, prelude::*, px, size, App, Application, Bounds, Menu, MenuItem,
    WindowBackgroundAppearance, WindowBounds, WindowOptions,
};

use graph::Graph;
use theme::Theme;
use ui::MindMapView;

/// Open a document from the Open Recent submenu. Carries which slot was picked
/// rather than the path itself: the menu is rebuilt from `store::recents()`
/// every time the list changes, so the index is always read against the list
/// the user was actually looking at.
#[derive(Clone, Debug, Default, PartialEq, gpui::Action)]
#[action(namespace = jotmind, no_json)]
pub struct OpenRecent {
    pub slot: usize,
}

actions!(
    jotmind,
    [
        // grow
        CreateRight,
        CreateLeft,
        CreateUp,
        CreateDown,
        DeleteNode,
        TidyLayout,
        // navigate
        NavLeft,
        NavRight,
        NavUp,
        NavDown,
        Escape,
        FocusPrevSibling,
        FocusNextSibling,
        CreateChild,
        CreateSibling,
        ToggleFold,
        OpenFind,
        FindNext,
        FindPrev,
        FindBackspace,
        FindToggleReplace,
        FindReplaceAll,
        SelectAllNodes,
        // restructure the tree from the keyboard
        SelectSiblings,
        MoveSiblingUp,
        MoveSiblingDown,
        Outdent,
        Indent,
        // browse jumps
        FocusRoot,
        FocusLast,
        // reposition
        NudgeLeft,
        NudgeRight,
        NudgeUp,
        NudgeDown,
        // reposition, a bigger step (⇧ + arrow)
        NudgeLeftBig,
        NudgeRightBig,
        NudgeUpBig,
        NudgeDownBig,
        // camera
        ZoomIn,
        ZoomOut,
        ZoomFit,
        ZoomReset,
        // write
        ToggleEdgeLabel,
        Commit,
        Backspace,
        DeleteForward,
        DeleteWordLeft,
        ClearFocusText,
        CaretLeft,
        CaretRight,
        CaretWordLeft,
        CaretWordRight,
        CaretHome,
        CaretEnd,
        CaretUp,
        CaretDown,
        InsertNewline,
        // select — the same motions with ⇧ held
        SelectLeft,
        SelectRight,
        SelectWordLeft,
        SelectWordRight,
        SelectHome,
        SelectEnd,
        SelectUp,
        SelectDown,
        SelectAll,
        // clipboard — text while editing, nodes while browsing
        CutText,
        CopyText,
        PasteText,
        CutNodes,
        CopyNodes,
        PasteNodes,
        DuplicateNodes,
        // session
        Undo,
        Redo,
        NewDocument,
        OpenDocument,
        SaveNow,
        SaveAs,
        RevertDocument,
        DuplicateDocument,
        RevealInFinder,
        ClearRecents,
        // import / export (logic lives in convert.rs / export_svg.rs)
        ExportMarkdown,
        ExportOpml,
        ExportOutline,
        ExportSvg,
        ImportMarkdown,
        ImportOpml,
        // per-node colour
        SetColor0,
        SetColor1,
        SetColor2,
        SetColor3,
        SetColor4,
        SetColor5,
        SetColor6,
        // view
        ToggleMinimap,
        // window / app shell
        About,
        HideApp,
        HideOthers,
        ShowAll,
        MinimizeWindow,
        ZoomWindow,
        ToggleFullScreen,
        CloseWindow,
        /// The only row an empty Open Recent submenu can hold. Nothing listens
        /// for it — a menu cannot hold a row that is merely greyed out.
        NoRecents,
        // Menu-only twins of ⇥, ⇧⇥ and ↩. GPUI turns a binding into a real
        // macOS key equivalent, but its key table stops at the arrows and the
        // function keys: `tab` and `enter` fall through as the literal strings
        // "tab" and "enter", which AppKit then draws as "T" and "E". A row that
        // states the wrong key is worse than one that states none, so these
        // three point at actions nothing is bound to and spell their own key in
        // the title. They dispatch straight into the real handlers.
        MenuNewChild,
        MenuNewSibling,
        MenuCommit,
        ToggleTheme,
        ToggleHelp,
        HelpBackspace,
        Quit,
    ]
);

/// Zed's buffer font: GPUI maps `.ZedMono` / "Zed Plex Mono" → **Lilex**.
/// Register shipped Lilex so it works even when not installed system-wide.
fn load_code_font(cx: &mut App) {
    let bytes = include_bytes!("../assets/fonts/Lilex-Regular.ttf");
    if let Err(err) = cx.text_system().add_fonts(vec![Cow::Borrowed(bytes)]) {
        eprintln!("mind-map-rust: failed to load Lilex font: {err:?}");
        eprintln!("mind-map-rust: install with `brew install --cask font-lilex` or check assets/fonts/");
    }
}

/// Modality lives in the keymap, not in the handlers.
///
/// GPUI stops propagating a key the moment a binding matches — *before* the
/// handler runs — so a handler that matches and then early-returns because the
/// mode is wrong eats the key and nothing else can have it. That is exactly why
/// ⌫ used to do nothing at all in Browse: `backspace` matched the text handler,
/// which returned because nothing was being edited. Scoping the binding instead
/// means the wrong-mode binding never matches, and the right one gets its turn.
const BROWSE: Option<&str> = Some("MindMap && mode == browse");
const EDIT: Option<&str> = Some("MindMap && mode == edit");
/// Driving the map: either Browse or Edit, but not while the shortcut panel is
/// up and not while the find field has the keyboard. Excluding `find` is what
/// stops `enter` matching both `Commit` and `FindNext` — two bindings on one
/// key, with the winner decided by table order rather than by intent.
const MAP: Option<&str> = Some("MindMap && mode != help && mode != find");
/// Works everywhere, including over the panel.
const ANY: Option<&str> = Some("MindMap");
/// Session-level: nothing to do with what the keyboard is pointed at, so these
/// stay live while the find field has it. Only the shortcut panel blocks them.
const SESSION: Option<&str> = Some("MindMap && mode != help");
/// While the find field is open, so ↩ steps through matches rather than
/// dropping into a label.
const FIND: Option<&str> = Some("MindMap && mode == find");
/// While the shortcut panel is up, so ⌫ edits its search box.
const HELP_CTX: Option<&str> = Some("MindMap && mode == help");

/// The keyboard is in two halves. `bind_fixed` holds the structural, modal
/// bindings — arrows, the caret, clipboard, delete — that are the app's grammar
/// and are not up for customising. `bind_rebindable` holds the *verbs* a user
/// might reasonably remap, each read from the keymap overrides. `apply_keymap`
/// lays both down, and can be called again after an edit to swap a chord live.
pub fn apply_keymap(cx: &mut App, overrides: &std::collections::HashMap<String, String>) {
    cx.clear_key_bindings();
    bind_fixed(cx);
    bind_rebindable(cx, overrides);
}

/// The customisable verbs: `(action name, human label, default chord)`. The
/// single source of truth for both the bindings and the editor list.
pub const REBINDABLE: &[(&str, &str, &str)] = &[
    ("NewDocument", "New map", "cmd-n"),
    ("OpenDocument", "Open map", "cmd-o"),
    ("SaveNow", "Save now", "cmd-s"),
    ("SaveAs", "Save as", "cmd-shift-s"),
    ("TidyLayout", "Tidy layout", "cmd-r"),
    ("ToggleFold", "Fold / unfold", "cmd-."),
    ("OpenFind", "Find", "cmd-f"),
    ("ToggleEdgeLabel", "Edge label", "cmd-l"),
    ("DuplicateNodes", "Duplicate", "cmd-d"),
    ("ZoomIn", "Zoom in", "cmd-="),
    ("ZoomOut", "Zoom out", "cmd--"),
    ("ZoomFit", "Fit map", "cmd-0"),
    ("ZoomReset", "Actual size", "cmd-1"),
    ("ToggleTheme", "Paper / night", "cmd-t"),
    ("ToggleMinimap", "Minimap", "cmd-shift-m"),
    ("ToggleHelp", "Shortcuts panel", "cmd-/"),
];

/// The chord a rebindable action answers to right now: the override, or its
/// default from [`REBINDABLE`].
pub fn chord_for(name: &str, overrides: &std::collections::HashMap<String, String>) -> String {
    if let Some(c) = overrides.get(name) {
        return c.clone();
    }
    REBINDABLE
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, _, d)| (*d).to_string())
        .unwrap_or_default()
}

fn bind_rebindable(cx: &mut App, overrides: &std::collections::HashMap<String, String>) {
    // Build the list rather than a fixed array: a verb whose chord is the empty
    // string is *unbound* (its chord was stolen by another rebind), and
    // `KeyBinding::new` panics on an empty chord — so it has to be skipped.
    fn push<A: gpui::Action>(
        v: &mut Vec<gpui::KeyBinding>,
        name: &str,
        action: A,
        ctx: Option<&str>,
        ov: &std::collections::HashMap<String, String>,
    ) {
        let ch = chord_for(name, ov);
        if !ch.is_empty() {
            v.push(gpui::KeyBinding::new(&ch, action, ctx));
        }
    }
    let mut binds = Vec::new();
    push(&mut binds, "NewDocument", NewDocument, SESSION, overrides);
    push(&mut binds, "OpenDocument", OpenDocument, SESSION, overrides);
    push(&mut binds, "SaveNow", SaveNow, SESSION, overrides);
    push(&mut binds, "SaveAs", SaveAs, SESSION, overrides);
    push(&mut binds, "TidyLayout", TidyLayout, MAP, overrides);
    push(&mut binds, "ToggleFold", ToggleFold, BROWSE, overrides);
    push(&mut binds, "OpenFind", OpenFind, ANY, overrides);
    push(&mut binds, "ToggleEdgeLabel", ToggleEdgeLabel, MAP, overrides);
    push(&mut binds, "DuplicateNodes", DuplicateNodes, BROWSE, overrides);
    push(&mut binds, "ZoomIn", ZoomIn, SESSION, overrides);
    push(&mut binds, "ZoomOut", ZoomOut, SESSION, overrides);
    push(&mut binds, "ZoomFit", ZoomFit, SESSION, overrides);
    push(&mut binds, "ZoomReset", ZoomReset, SESSION, overrides);
    push(&mut binds, "ToggleTheme", ToggleTheme, SESSION, overrides);
    push(&mut binds, "ToggleMinimap", ToggleMinimap, SESSION, overrides);
    push(&mut binds, "ToggleHelp", ToggleHelp, ANY, overrides);
    cx.bind_keys(binds);
}

fn bind_fixed(cx: &mut App) {
    use gpui::KeyBinding as K;
    cx.bind_keys([
        // Create children (cmd + arrow)
        K::new("cmd-right", CreateRight, MAP),
        K::new("cmd-left", CreateLeft, MAP),
        K::new("cmd-up", CreateUp, MAP),
        K::new("cmd-down", CreateDown, MAP),
        K::new("cmd-shift-backspace", DeleteNode, MAP),
        // Find. ⌘G keeps stepping without going back to the field.
        K::new("enter", FindNext, FIND),
        K::new("shift-enter", FindPrev, FIND),
        K::new("cmd-g", FindNext, ANY),
        K::new("cmd-shift-g", FindPrev, ANY),
        // ⌫ edits the query, it does not step backwards through matches.
        K::new("backspace", FindBackspace, FIND),
        // ⇥ swaps between the query and the replacement field; ⌘↩ runs the
        // replace across every match.
        K::new("tab", FindToggleReplace, FIND),
        K::new("cmd-enter", FindReplaceAll, FIND),
        // Arrows are modal. In Edit they drive the caret. In Browse they *move*
        // the selection — a bare arrow one grid step, ⇧ a bigger stride — and
        // navigation between nodes moves onto ⌥arrow. Selecting something and
        // steering it with the arrows is the gesture people reach for first.
        K::new("left", NudgeLeft, BROWSE),
        K::new("right", NudgeRight, BROWSE),
        K::new("up", NudgeUp, BROWSE),
        K::new("down", NudgeDown, BROWSE),
        K::new("shift-left", NudgeLeftBig, BROWSE),
        K::new("shift-right", NudgeRightBig, BROWSE),
        K::new("shift-up", NudgeUpBig, BROWSE),
        K::new("shift-down", NudgeDownBig, BROWSE),
        K::new("left", CaretLeft, EDIT),
        K::new("right", CaretRight, EDIT),
        K::new("up", CaretUp, EDIT),
        K::new("down", CaretDown, EDIT),
        // Walk the map, spatially: parent on ←, a child on →, a sibling on ↑↓.
        K::new("alt-left", NavLeft, BROWSE),
        K::new("alt-right", NavRight, BROWSE),
        K::new("alt-up", NavUp, BROWSE),
        K::new("alt-down", NavDown, BROWSE),
        // Extra structural nav
        K::new("escape", Escape, ANY),
        // ⇥ = child, ⇧⇥ = sibling: the convention every other mind map uses,
        // and the two verbs that were hardest to reach.
        K::new("tab", CreateChild, BROWSE),
        K::new("shift-tab", CreateSibling, BROWSE),
        // ⌘↩ = child, ⌘⇧↩ = sibling — reachable straight from Edit (both
        // handlers leave_edit first) so you can name a node and branch off it
        // without first stepping back to Browse.
        K::new("cmd-enter", CreateChild, MAP),
        K::new("cmd-shift-enter", CreateSibling, MAP),
        // Restructure an existing node: reorder it among its siblings (⌥⇧↑↓) or
        // change its depth (⌥⇧← promote to beside the parent, ⌥⇧→ demote under
        // the sibling above). Browse-only; the same chords select text in Edit.
        K::new("alt-shift-up", MoveSiblingUp, BROWSE),
        K::new("alt-shift-down", MoveSiblingDown, BROWSE),
        K::new("alt-shift-left", Outdent, BROWSE),
        K::new("alt-shift-right", Indent, BROWSE),
        // Browse jumps: Home to the root, End to the bottom-most node.
        K::new("home", FocusRoot, BROWSE),
        K::new("end", FocusLast, BROWSE),
        // Strict sibling walk stays reachable from the View menu; on the
        // keyboard ⌥↑ ⌥↓ are the spatial move now.
        // Reposition still answers to the ⌘⌥ pair as well, so muscle memory and
        // the menu's accelerators keep working.
        K::new("cmd-alt-left", NudgeLeft, MAP),
        K::new("cmd-alt-right", NudgeRight, MAP),
        K::new("cmd-alt-up", NudgeUp, MAP),
        K::new("cmd-alt-down", NudgeDown, MAP),
        // Word-wise caret movement.
        K::new("alt-left", CaretWordLeft, EDIT),
        K::new("alt-right", CaretWordRight, EDIT),
        // Selection: the same motions with ⇧.
        K::new("shift-left", SelectLeft, EDIT),
        K::new("shift-right", SelectRight, EDIT),
        K::new("shift-up", SelectUp, EDIT),
        K::new("shift-down", SelectDown, EDIT),
        K::new("alt-shift-left", SelectWordLeft, EDIT),
        K::new("alt-shift-right", SelectWordRight, EDIT),
        K::new("shift-home", SelectHome, EDIT),
        K::new("shift-end", SelectEnd, EDIT),
        // ⌘A means "select everything here", and what "here" is depends on
        // whether you are in a label or on the map.
        K::new("cmd-a", SelectAll, EDIT),
        K::new("cmd-a", SelectAllNodes, BROWSE),
        // Clipboard. ⌘C means "copy what I have picked", and in Browse what
        // you have picked is nodes — copying their label text instead is the
        // wrong noun once nodes are selectable objects.
        K::new("cmd-x", CutText, EDIT),
        K::new("cmd-c", CopyText, EDIT),
        K::new("cmd-v", PasteText, EDIT),
        K::new("cmd-x", CutNodes, BROWSE),
        K::new("cmd-c", CopyNodes, BROWSE),
        K::new("cmd-v", PasteNodes, BROWSE),
        // ⇧ variant of zoom-in stays a fixed alias, so the primary chord is the
        // only one anyone has to think about customising.
        K::new("cmd-shift-=", ZoomIn, SESSION),
        // Edit chrome
        K::new("enter", Commit, MAP),
        // ⇧↩ drops a hard line break into a label rather than leaving Edit.
        K::new("shift-enter", InsertNewline, EDIT),
        // Delete: ⌫ takes text while you are writing and the selected nodes
        // while you are not. `backspace` is the key labelled *delete* on a Mac
        // keyboard; gpui's `delete` is fn⌫.
        K::new("backspace", Backspace, EDIT),
        K::new("delete", DeleteForward, EDIT),
        K::new("backspace", DeleteNode, BROWSE),
        K::new("delete", DeleteNode, BROWSE),
        K::new("alt-backspace", DeleteWordLeft, EDIT),
        K::new("cmd-backspace", ClearFocusText, EDIT),
        K::new("home", CaretHome, EDIT),
        K::new("end", CaretEnd, EDIT),
        // Session
        K::new("cmd-z", Undo, SESSION),
        K::new("cmd-shift-z", Redo, SESSION),
        // Paint the selection: 0 clears, 1–6 pick a colour. Bare digits, and
        // only in Browse — in Edit they are just text.
        K::new("0", SetColor0, BROWSE),
        K::new("1", SetColor1, BROWSE),
        K::new("2", SetColor2, BROWSE),
        K::new("3", SetColor3, BROWSE),
        K::new("4", SetColor4, BROWSE),
        K::new("5", SetColor5, BROWSE),
        K::new("6", SetColor6, BROWSE),
        // The shortcut panel searches itself, so ⌫ trims its query.
        K::new("backspace", HelpBackspace, HELP_CTX),
        K::new("cmd-q", Quit, ANY),
        // Window / app shell. All ⌘-modified, so none collides with typing, and
        // none has a mode-specific second meaning — safe to fire from anywhere.
        K::new("cmd-h", HideApp, ANY),
        K::new("cmd-alt-h", HideOthers, ANY),
        K::new("cmd-m", MinimizeWindow, ANY),
        K::new("ctrl-cmd-f", ToggleFullScreen, ANY),
        K::new("cmd-w", CloseWindow, ANY),
        // ⌘, opens the settings/shortcuts panel — the Mac-standard Preferences
        // key. A second alias for ToggleHelp alongside ⌘/.
        K::new("cmd-,", ToggleHelp, SESSION),
    ]);
}

/// Rebuild the menu bar. Called at startup and again whenever the recents list
/// changes, because Open Recent is the one submenu made of data rather than of
/// a fixed list of verbs.
///
/// Nothing here spells its own shortcut out. GPUI reads the keymap and hands
/// each item a real key equivalent, so a typed-in "⌘Z" only ever showed up
/// twice — once as part of the title, once in the column macOS draws itself.
pub fn set_menus(cx: &mut App) {
    let recents: Vec<MenuItem> = store::recents()
        .iter()
        .enumerate()
        .map(|(slot, path)| MenuItem::action(store::recent_label(path), OpenRecent { slot }))
        .collect();
    let recents = if recents.is_empty() {
        vec![MenuItem::action("Nothing Yet", NoRecents)]
    } else {
        let mut items = recents;
        items.push(MenuItem::separator());
        items.push(MenuItem::action("Clear Menu", ClearRecents));
        items
    };

    cx.set_menus(vec![
        Menu {
            name: "mind67".into(),
            items: vec![
                MenuItem::action("About mind67", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", ToggleHelp),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide mind67", HideApp),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit mind67", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New", NewDocument),
                MenuItem::action("Duplicate", DuplicateDocument),
                MenuItem::separator(),
                MenuItem::action("Open…", OpenDocument),
                MenuItem::Submenu(Menu {
                    name: "Open Recent".into(),
                    items: recents,
                }),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindow),
                MenuItem::separator(),
                // The map is written a beat after every change; this is for
                // when you want to *know* it landed, not for when you have to.
                MenuItem::action("Save Now", SaveNow),
                MenuItem::action("Save As…", SaveAs),
                MenuItem::action("Revert to Saved", RevertDocument),
                MenuItem::action("Reveal in Finder", RevealInFinder),
                MenuItem::separator(),
                MenuItem::Submenu(Menu {
                    name: "Import".into(),
                    items: vec![
                        MenuItem::action("Markdown…", ImportMarkdown),
                        MenuItem::action("OPML…", ImportOpml),
                    ],
                }),
                MenuItem::Submenu(Menu {
                    name: "Export".into(),
                    items: vec![
                        MenuItem::action("Image (SVG)…", ExportSvg),
                        MenuItem::action("Markdown…", ExportMarkdown),
                        MenuItem::action("OPML…", ExportOpml),
                        MenuItem::action("Plain Outline…", ExportOutline),
                    ],
                }),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", Undo),
                MenuItem::action("Redo", Redo),
                MenuItem::separator(),
                MenuItem::action("Cut", CutNodes),
                MenuItem::action("Copy", CopyNodes),
                MenuItem::action("Paste", PasteNodes),
                MenuItem::action("Duplicate", DuplicateNodes),
                MenuItem::action("Select All Nodes", SelectAllNodes),
                MenuItem::Submenu(Menu {
                    name: "Colour".into(),
                    items: vec![
                        MenuItem::action("None  0", SetColor0),
                        MenuItem::action("Red  1", SetColor1),
                        MenuItem::action("Amber  2", SetColor2),
                        MenuItem::action("Green  3", SetColor3),
                        MenuItem::action("Teal  4", SetColor4),
                        MenuItem::action("Blue  5", SetColor5),
                        MenuItem::action("Violet  6", SetColor6),
                    ],
                }),
                MenuItem::separator(),
                MenuItem::action("Find", OpenFind),
                MenuItem::separator(),
                MenuItem::action("Fold / Unfold", ToggleFold),
                MenuItem::separator(),
                MenuItem::action("New Child  ⇥", MenuNewChild),
                MenuItem::action("New Sibling  ⇧⇥", MenuNewSibling),
                MenuItem::separator(),
                MenuItem::action("Select Siblings", SelectSiblings),
                MenuItem::action("Move Up  ⌥⇧↑", MoveSiblingUp),
                MenuItem::action("Move Down  ⌥⇧↓", MoveSiblingDown),
                MenuItem::action("Promote  ⌥⇧←", Outdent),
                MenuItem::action("Demote  ⌥⇧→", Indent),
                MenuItem::separator(),
                MenuItem::action("Create Right", CreateRight),
                MenuItem::action("Create Left", CreateLeft),
                MenuItem::action("Create Up", CreateUp),
                MenuItem::action("Create Down", CreateDown),
                MenuItem::action("Delete Node", DeleteNode),
                MenuItem::separator(),
                MenuItem::action("Edge Label", ToggleEdgeLabel),
                MenuItem::action("Edit / Done  ↩", MenuCommit),
                MenuItem::separator(),
                MenuItem::action("Deselect / Cancel", Escape),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Fit Map", ZoomFit),
                MenuItem::action("Actual Size", ZoomReset),
                MenuItem::separator(),
                MenuItem::action("Minimap", ToggleMinimap),
                MenuItem::action("Paper / Night", ToggleTheme),
                MenuItem::separator(),
                MenuItem::action("Tidy Layout", TidyLayout),
                MenuItem::separator(),
                MenuItem::action("Next Sibling", FocusNextSibling),
                MenuItem::action("Previous Sibling", FocusPrevSibling),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", MinimizeWindow),
                MenuItem::action("Zoom", ZoomWindow),
                MenuItem::separator(),
                MenuItem::action("Enter Full Screen", ToggleFullScreen),
            ],
        },
        // macOS puts its own search field at the top of whichever menu the app
        // nominates as Help — `mac::use_help_menu` does the nominating — and
        // that field searches every menu title in the bar. Which is the other
        // half of why the titles above are now just their verbs.
        Menu {
            name: "Help".into(),
            items: vec![MenuItem::action("mind67 Shortcuts", ToggleHelp)],
        },
    ]);
    mac::use_help_menu();
}

/// Which map to open, and whether this session owns the document on disk.
struct Startup {
    graph: Graph,
    pose: Option<store::Pose>,
    window: Option<store::WindowRect>,
    theme: Theme,
    /// The document being edited. `None` means this session writes nothing.
    path: Option<std::path::PathBuf>,
    /// Shown in the status line when we deliberately opened read-only.
    warning: Option<String>,
}

fn startup(demo: bool, empty: bool) -> Startup {
    let scratch = |graph: Graph, warning: Option<String>| Startup {
        graph,
        pose: None,
        window: None,
        theme: theme::LIGHT,
        path: None,
        warning,
    };
    // `--demo` / `--empty` are scratch maps: they never overwrite the real one.
    if demo || empty {
        return scratch(if demo { Graph::demo() } else { Graph::new("") }, None);
    }
    let Some(path) = store::startup_path() else {
        return scratch(Graph::demo(), Some("no home directory to save into".into()));
    };
    match store::load_path(&path) {
        store::Load::Opened(doc) => {
            store::remember(&path);
            Startup {
                graph: doc.graph,
                pose: doc.camera,
                window: doc.window,
                theme: Theme::from_dark(doc.dark),
                path: Some(path),
                warning: None,
            }
        }
        // Start with one editable idea; the welcome sheet explains the essentials.
        store::Load::Fresh => {
            store::remember(&path);
            Startup {
                graph: Graph::new("My first idea"),
                pose: None,
                window: None,
                theme: theme::LIGHT,
                path: Some(path),
                warning: None,
            }
        }
        // The saved map is unreadable. Open a scratch session so autosave can
        // never clobber a file the user might still rescue by hand.
        store::Load::Unreadable(msg) => scratch(Graph::new(""), Some(msg)),
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let smoke = has("--smoke");
    let demo = has("--demo") || smoke;
    let empty = has("--empty");

    Application::new().run(move |cx: &mut App| {
        load_code_font(cx);
        apply_keymap(cx, &store::load_keymap());
        pinch::install();

        cx.on_action(|_: &Quit, cx| {
            cx.quit();
        });

        set_menus(cx);

        let Startup {
            graph,
            pose,
            window: saved_window,
            theme,
            path,
            warning,
        } = startup(demo, empty);

        // Reopen where it was left. `usable()` has already rejected anything
        // degenerate, and macOS pulls a window back on screen if the display it
        // was on has since gone away.
        let bounds = match saved_window {
            Some(w) => Bounds {
                origin: gpui::point(px(w.x), px(w.y)),
                size: size(px(w.w), px(w.h)),
            },
            None => Bounds::centered(None, size(px(1020.), px(700.)), cx),
        };
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Let macOS clip the window once. An opaque canvas avoids
                    // the extra glass rim and keeps both palettes predictable.
                    window_background: WindowBackgroundAppearance::Opaque,
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some(dev::app_title().into()),
                        // The system bar is opaque white, which in night mode
                        // put a 33px white band above a near-black canvas —
                        // the most unfinished-looking thing in the app.
                        appears_transparent: true,
                        // Measured from the top of the system titlebar down to
                        // the top of the close button. The strip the view draws
                        // is taller than that band, so this only sets how far
                        // the buttons sit from the window's top edge; `ui`'s
                        // `TITLEBAR_H` decides how much room is left under them.
                        traffic_light_position: Some(gpui::point(px(18.), px(16.))),
                    }),
                    ..Default::default()
                },
                move |window, cx| {
                    cx.new(|cx| {
                        let mut view = MindMapView::new(graph, pose, theme, path, cx);
                        if let Some(msg) = warning {
                            view.open_read_only(msg);
                        }
                        view.focus(window, cx);
                        view
                    })
                },
            )
            .unwrap();

        // The debounced autosave can't run after the event loop stops — flush.
        cx.on_app_quit(move |cx: &mut App| {
            let _ = window.update(cx, |view, _, _| view.save_on_quit());
            async {}
        })
        .detach();

        cx.activate(true);
        if !smoke && !demo && !empty {
            let _ = window.update(cx, |_, window, cx| onboarding::show_once(window, cx));
        }

        if smoke {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(1800))
                    .await;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
}
