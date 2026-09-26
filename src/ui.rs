//! GPUI view: text nodes + curved edges on a free, zoomable canvas.
//! Font: **Lilex** — same family Zed uses for code (GPUI: `.ZedMono` → Lilex).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, FocusHandle, Focusable, FontWeight, Hitbox,
    HitboxBehavior, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PathPromptOptions, Pixels, ScrollWheelEvent, SharedString, Window, canvas, div, font,
    prelude::*, px,
};

use crate::camera::{Camera, Viewport};
use crate::image_bank;
use crate::paint::IMAGE_MIN_PX;
use crate::images;
use crate::graph::{Clip, Direction, Graph, Selection, layout};
use crate::history::{History, Tag};
use crate::paint::{
    CODE_FONT, CULL_MARGIN, FONT_EDGE, FONT_NODE, FONT_ROOT, Frame, LINE_H,
    MIN_READABLE_PX, Minimap, PaintEdge, PaintNode, PAD_X, PAD_Y, index_for_offset, label_lines,
    line_offset_y, mark_radius, paint_map, shape_width,
};
use crate::pinch;
use crate::store::{self, Doc, Pose};
use crate::theme::{Theme, c, ca};
use crate::{
    Backspace, CaretDown, CaretEnd, CaretHome, CaretLeft, CaretRight, CaretUp, CaretWordLeft,
    CaretWordRight, InsertNewline, SelectDown, SelectUp,
    ClearFocusText, Commit, CopyNodes, CopyText, CreateChild, CreateDown, CreateLeft,
    CreateRight, CreateSibling, CreateUp, CutNodes, CutText, DeleteForward, DeleteNode,
    DeleteWordLeft, DuplicateNodes, Escape, FindBackspace, FindNext, FindPrev, FocusNextSibling,
    HelpBackspace,
    ExportMarkdown, ExportOpml, ExportOutline, ExportSvg,
    FocusPrevSibling, ImportMarkdown, ImportOpml, NavDown, NavLeft, NavRight, NavUp, NewDocument,
    NudgeDown, NudgeDownBig, NudgeLeft, NudgeLeftBig, NudgeRight, NudgeRightBig,
    MenuCommit, MenuNewChild, MenuNewSibling, ClearRecents, RevealInFinder, SaveAs, ToggleMinimap,
    About, HideApp, HideOthers, ShowAll, MinimizeWindow, ZoomWindow, ToggleFullScreen, CloseWindow,
    SelectSiblings, MoveSiblingUp, MoveSiblingDown, Outdent, Indent, FocusRoot, FocusLast,
    FindToggleReplace, FindReplaceAll, RevertDocument, DuplicateDocument,
    SetColor0, SetColor1, SetColor2, SetColor3, SetColor4, SetColor5, SetColor6,
    NudgeUp, NudgeUpBig, OpenDocument, OpenFind, OpenRecent, PasteNodes, PasteText, Redo, SaveNow, SelectAll,
    SelectAllNodes, SelectEnd, SelectHome, SelectLeft, SelectRight, SelectWordLeft,
    SelectWordRight, TidyLayout, ToggleEdgeLabel, ToggleFold, ToggleHelp, ToggleTheme, Undo,
    ZoomFit, ZoomIn, ZoomOut, ZoomReset, convert, export_svg, mac, set_menus,
};


/// Padding used when framing the map.
const FIT_PAD: f32 = 56.0;
/// How much of each edge stays clear when navigation pulls a node into view.
const KEEP_INSET: f32 = 0.18;
/// Pointer travel before a press counts as a drag rather than a click.
const DRAG_SLOP: f32 = 3.0;

/// How small and how large a picture may be dragged, in world units. The floor
/// keeps a picture from being shrunk into something that cannot be grabbed
/// again; the ceiling keeps one from being stretched across a map until nothing
/// else can be found. `Graph::sanitize` repairs a loaded map to the same
/// bounds, so a hand-edited file cannot hold a box the gesture would refuse.
use crate::graph::{MAX_IMAGE_BOX as MAX_IMAGE_W, MIN_IMAGE_BOX as MIN_IMAGE_W};
/// World-space distance a node must be pulled from its parent, with no drop
/// target under the pointer, before releasing cuts the connection. Generous on
/// purpose: an ordinary reposition should never sever a branch by accident.
const DISCONNECT_DIST: f32 = 340.0;
/// How long a status message stays up before fading out.
const STATUS_SECS: f32 = 2.6;
/// A release this long after the last pointer movement is a stop, not a flick.
const FLICK_WINDOW: Duration = Duration::from_millis(80);
/// Idle time after the last change before the map is written to disk.
const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(700);
const BLINK: f32 = 0.53;
/// How many grid steps a ⇧+arrow move covers, against one for a bare arrow.
const BIG_NUDGE: f32 = 5.0;
/// The shortcut panel's column and key-column widths. The key column is fixed
/// and the description takes the rest and wraps, so a long line can never spill
/// into the next column the way it used to.
const HELP_COL_W: f32 = 344.0;
const HELP_KEY_W: f32 = 168.0;
/// Height of the strip the window's traffic lights sit in.
const TITLEBAR_H: f32 = 44.0;
/// Time constant for a node easing to a new position. Short: this is feedback
/// about what moved, not a performance — anything slower and tidy feels like
/// it is thinking rather than answering.
const NODE_EASE_TAU: f32 = 0.055;
/// Below this world-space gap a node has arrived.
const NODE_SETTLED: f32 = 0.4;
/// How close to the window edge a held drag starts pulling the canvas along.
const EDGE_PAN_MARGIN: f32 = 48.0;
/// Screen px per second at the very edge; it ramps up from zero.
const EDGE_PAN_SPEED: f32 = 900.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hover {
    Node(u64),
    Edge(u64),
}

/// Where an import's bytes are coming from: the pasteboard, or files dropped on
/// the canvas. Both end in the same place — the store — so they share a path.
enum ImportSource {
    Bytes(Vec<u8>),
    Files(Vec<std::path::PathBuf>),
}

/// The default width a freshly imported picture is given, in world units. Wide
/// enough to see, small enough that dropping a photo does not bury the map.
const DEFAULT_IMAGE_W: f32 = 320.0;

/// The first image on the pasteboard, if there is one.
fn clipboard_image(item: &gpui::ClipboardItem) -> Option<Vec<u8>> {
    item.entries().iter().find_map(|entry| match entry {
        gpui::ClipboardEntry::Image(image) => Some(image.bytes.clone()),
        _ => None,
    })
}

/// Both ends of `range`, as x offsets from the centre of a `full_w`-wide label.
/// Two prefix shapings — the same measurement the caret uses, so the band and
/// the caret can never disagree about where a boundary is.
/// Byte offset nearest a click at `(dx, dy)` from the label's centre, across
/// however many hard-broken lines the label has. Picks the line by `dy`, then
/// the column within it by `dx`.
fn index_at_point(
    window: &mut Window,
    text: &str,
    font: f32,
    weight: FontWeight,
    dx: f32,
    dy: f32,
) -> usize {
    let lines = label_lines(text);
    let n = lines.len();
    if n == 1 {
        return index_for_offset(window, text, font, weight, dx);
    }
    let line_h = font * LINE_H;
    // dy of line i is (i - (n-1)/2)*line_h; invert and round to the nearest row.
    let rel = dy / line_h + (n as f32 - 1.0) * 0.5;
    let i = (rel.round().max(0.0) as usize).min(n - 1);
    let local = index_for_offset(window, lines[i], font, weight, dx);
    let mut start = 0usize;
    for line in lines.iter().take(i) {
        start += line.len() + 1; // + the '\n'
    }
    start + local
}

/// Caret and selection geometry for a label that may hold hard line breaks.
/// Everything is measured from the block's centre, the same origin paint uses:
/// the caret as `(dx, dy)`, and the selection as one `(dy, x0, x1)` band per
/// visual line it touches. A single-line label is just the one-line case.
fn text_geometry(
    window: &mut Window,
    text: &str,
    font: f32,
    weight: FontWeight,
    caret: usize,
    sel: Option<(usize, usize)>,
) -> ((f32, f32), Vec<(f32, f32, f32)>) {
    let lines = label_lines(text);
    let n = lines.len();
    // Byte offset at the start of each visual line.
    let mut starts = Vec::with_capacity(n);
    let mut acc = 0usize;
    for (i, l) in lines.iter().enumerate() {
        starts.push(acc);
        acc += l.len();
        if i + 1 < n {
            acc += 1; // the '\n'
        }
    }
    let widths: Vec<f32> = lines
        .iter()
        .map(|l| shape_width(window, l, font, weight))
        .collect();

    // The caret.
    let caret = caret.min(text.len());
    let cline = text[..caret].bytes().filter(|&b| b == b'\n').count().min(n - 1);
    let cls = starts[cline];
    let cdx = shape_width(window, &text[cls..caret], font, weight) - widths[cline] * 0.5;
    let caret_xy = (cdx, line_offset_y(cline, n, font));

    // The selection, split per line it crosses.
    let mut bands = Vec::new();
    if let Some((s, e)) = sel {
        let (s, e) = (s.min(text.len()), e.min(text.len()));
        if e > s {
            for i in 0..n {
                let ls = starts[i];
                let le = ls + lines[i].len();
                let a = s.max(ls);
                let b = e.min(le);
                // Skip a line the selection does not touch; keep a blank line
                // that sits wholly inside the selection so it still shows.
                let interior = s <= ls && e > le;
                if a > b || (a == b && !interior) {
                    continue;
                }
                let dy = line_offset_y(i, n, font);
                let half = widths[i] * 0.5;
                let x0 = shape_width(window, &text[ls..a], font, weight) - half;
                let mut x1 = shape_width(window, &text[ls..b], font, weight) - half;
                // The selected line break itself reads as a little tail.
                if e > le {
                    x1 += font * 0.3;
                }
                bands.push((dy, x0, x1));
            }
        }
    }
    (caret_xy, bands)
}

/// "s" unless there is exactly one of them.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The find field's state. Matches are recomputed on every keystroke — a map
/// is small enough that an index would be machinery with nothing to buy.
struct Find {
    query: String,
    matches: Vec<u64>,
    at: usize,
    /// The replacement text, and whether the keyboard is currently pointed at it
    /// (⇥ toggles) rather than at the query.
    replace: String,
    replacing: bool,
}

/// What a context-menu row does. A closed set rather than boxed closures so
/// the menu is plain data, built and thrown away per press.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuCmd {
    NewChild,
    NewSibling,
    NewNodeHere,
    EditLabel,
    SelectBranch,
    Fold,
    Duplicate,
    Copy,
    Cut,
    Paste,
    Delete,
    SelectAll,
    Tidy,
    Fit,
}

/// A row, or a rule between groups.
enum MenuRow {
    Item(&'static str, &'static str, MenuCmd),
    Rule,
}

/// An open context menu: where it is, what is in it, and the world point the
/// press landed on — "new node here" means *here*, not wherever focus is.
/// Built and thrown away per press; the selection change that goes with
/// opening it happens at the call site, not in here.
struct ContextMenu {
    at: (f32, f32),
    world: (f32, f32),
    rows: Vec<MenuRow>,
}

/// Row height and menu width in px. Fixed so the menu can be clamped into the
/// window before it is laid out.
const MENU_ROW_H: f32 = 22.0;
const MENU_RULE_H: f32 = 7.0;
const MENU_W: f32 = 240.0;

/// World-space half-width of a node's ring, from the last measurement. Free
/// rather than a method so callers can borrow `shaped` on its own while some
/// other field of the view is held mutably.
/// World-space half-extents of a node's ring, from the same measurement paint
/// uses — so what you can see and what you can press are one rectangle. Free
/// rather than a method so callers can borrow `shaped` on its own while some
/// other field of the view is held mutably.
fn half_of(shaped: &HashMap<u64, Shaped>, zoom: f32, id: u64) -> (f32, f32) {
    shaped
        .get(&id)
        .map(|m| {
            let (hw, hh) = m.half(zoom);
            (hw / zoom, hh / zoom)
        })
        .unwrap_or((layout::NODE_HALF_W, layout::NODE_HALF_H))
}

/// A label measured once, at its base font. Its on-screen size at any zoom is
/// derived by scaling — glyph advances are linear in point size — so a smooth
/// zoom never reshapes text. Only a change to the label itself re-measures.
struct Shaped {
    /// What was shaped — the label, or the placeholder for an empty node.
    text: SharedString,
    /// Base font size (root vs node), the reference the width was taken at.
    base: f32,
    /// Widest line's shaped width, at the base font.
    w_base: f32,
    /// Number of hard-broken lines.
    lines: u16,
    /// An image node's box in world units, which is its extent instead of the
    /// text's. Kept *here* rather than read from the node at each call site
    /// because `half_of` is the one accessor paint, hit-testing, the marquee,
    /// fit and edge trimming all go through, and it never sees a `Node`.
    image: Option<(f32, f32)>,
}

impl Shaped {
    /// On-screen font size at zoom `z`.
    fn font(&self, z: f32) -> f32 {
        self.base * z
    }

    /// On-screen ring half-extents at zoom `z`. Below the readable size the node
    /// is a mark, so the target shrinks to the mark.
    fn half(&self, z: f32) -> (f32, f32) {
        let font = self.base * z;
        if font < MIN_READABLE_PX {
            let r = mark_radius(font);
            return (r, r);
        }
        // A picture is as big as it was drawn, not as big as its label would
        // have been. World units scale with the zoom like everything else.
        if let Some((w, h)) = self.image {
            return (w * z * 0.5, h * z * 0.5);
        }
        let hw = (self.w_base * z * 0.5 + font * PAD_X).max(font * 0.9);
        let hh = font * PAD_Y + (self.lines.max(1) as f32 - 1.0) * font * LINE_H * 0.5;
        (hw, hh)
    }
}

/// An in-flight pointer gesture. Which one a press starts is decided once, in
/// `on_mouse_down`, from what is under the pointer and which mode the map is
/// in — never re-decided mid-drag.
#[derive(Clone, Debug)]
enum Drag {
    /// Repositioning the selection. `id` is the node actually grabbed; the
    /// rest of the selection follows by the same delta.
    Node {
        id: u64,
        /// Cursor-to-node offset in world units, so the node doesn't jump.
        grab: (f32, f32),
        origin: (f32, f32),
        /// What this gesture is moving, fixed at press time. Recomputing it per
        /// event means anything that re-points the cursor mid-drag — an arrow
        /// key, a paste — silently changes what the pointer is carrying, and
        /// the delta then re-applies in full on every motion event.
        ids: Vec<u64>,
        moved: bool,
        /// Undo depth just after this gesture pushed its snapshot, so cancel
        /// can tell whether the top of the stack is still its own to take back.
        depth: usize,
        /// A node the drop would re-hang these under, when the pointer is over
        /// one that is a legal parent for all of them.
        onto: Option<u64>,
        /// Set once the grabbed node has been pulled far enough from its parent
        /// that releasing here would cut it free. Mutually exclusive with `onto`
        /// — a drop is either a re-hang or a disconnect, never both.
        detach: bool,
    },
    /// Resizing a picture by its corner grip.
    ///
    /// Modelled on `Node`: the snapshot is pushed at press time and `depth`
    /// lets a cancel tell whether the top of the undo stack is still its own.
    Resize {
        id: u64,
        /// The box when the grip was grabbed, in world units.
        start: (f32, f32),
        /// Where the pointer was then, in world units, so it tracks without
        /// jumping.
        grab: (f32, f32),
        /// Width over height, held so the picture keeps its proportions.
        aspect: f32,
        moved: bool,
        depth: usize,
    },
    /// Rubber-band over the canvas. `base` is what was selected when the drag
    /// began, so a ⇧-marquee adds to it instead of replacing it.
    Marquee {
        origin: (f32, f32),
        current: (f32, f32),
        base: Vec<u64>,
        /// Where the cursor was before the sweep. Restored when the band ends
        /// up empty, and on cancel: a rectangle that caught nothing should not
        /// quietly relocate where ⌘→ will build the next node.
        focus_before: u64,
    },
    /// Growing a child off a node's ring: press the fold bubble and drag to
    /// empty ground to place a new child there. A press with no drag still just
    /// folds. `to` tracks the pointer for the preview line.
    Grow {
        from: u64,
        origin: (f32, f32),
        to: (f32, f32),
        moved: bool,
    },
    /// Sweeping a text selection through the label being edited.
    Text,
    /// Flying the camera around — middle button only; a plain drag selects.
    Pan {
        last: (f32, f32),
        last_at: Instant,
        /// Screen px/sec, handed to the camera as inertia on release.
        vel: (f32, f32),
        moved: bool,
    },
}

pub struct MindMapView {
    graph: Graph,
    /// Decoded pictures, downscaled and bounded. See [`image_bank`].
    bank: image_bank::Bank,
    /// The directory the image store sits in. Resolved once, because the frame
    /// build asks for it per visible image node.
    images_dir: std::path::PathBuf,
    history: History,
    camera: Camera,
    theme: Theme,
    focus_handle: FocusHandle,
    status: SharedString,
    /// Seconds the toast has left. Messages fade out instead of parking in a
    /// bar across the top of the map.
    status_ttl: f32,

    drag: Option<Drag>,
    hover: Option<Hover>,
    show_help: bool,
    /// What has been typed into the shortcut panel's own search box. Filters the
    /// rows live; empty means show them all.
    help_query: String,
    /// The corner minimap: an overview of the whole map with a viewport box.
    show_minimap: bool,
    /// Last window title handed to the OS, so it is only re-set when it changes.
    shown_title: Option<String>,
    /// The user's keyboard-shortcut overrides — action name → chord.
    keymap: HashMap<String, String>,
    /// The action whose chord is being captured in the shortcut panel, if any.
    /// While it is set, all bindings are cleared so every key reaches the
    /// capture handler instead of firing a command.
    capturing: Option<&'static str>,
    /// The open context menu, if any.
    menu: Option<ContextMenu>,
    /// The find field, when it is open: what has been typed, what it matched,
    /// and which match we are standing on.
    find: Option<Find>,
    /// The last query, kept after the field closes so ⌘G resumes the search
    /// rather than starting a blank one.
    last_query: String,

    cursor_on: bool,
    blink_t: f32,

    /// Per-node shaping, keyed by node id and invalidated when the label or
    /// the painted size changes. Read by paint, hit-testing and fit alike.
    shaped: HashMap<u64, Shaped>,
    /// The hidden set from the last built frame. Hit-testing on the mouse path
    /// reuses it instead of rebuilding it on every pointer event — structure
    /// cannot change between a frame and the next mouse event, since any change
    /// renders first.
    frame_hidden: HashSet<u64>,

    /// The document this session owns. `None` is a scratch session (`--demo`,
    /// `--empty`, or a map that would not open) and writes nothing at all.
    path: Option<PathBuf>,
    dirty_since: Option<Instant>,
    /// Why the last write failed, if it did. Painted as a standing warning
    /// rather than a toast — losing work quietly is the worst thing this app
    /// can do, so the notice has to outlive a fade.
    save_error: Option<SharedString>,
    /// Set when the saved map could not be read. This session is deliberately
    /// not writing anything, and must keep saying so.
    read_only: Option<SharedString>,

    /// The window's own frame, refreshed each render so quitting can record
    /// where the user left it. Reopening somewhere else is a small thing that
    /// makes an app feel like it is not paying attention.
    window_rect: Option<store::WindowRect>,

    /// Where each node is *drawn*, which lags where it *is* while a layout
    /// change eases into place. The graph stays the truth; this is a view of
    /// it, so nothing downstream of the document has to know about motion.
    ///
    /// Only nodes that are mid-flight are in here — a settled node is drawn
    /// straight from the graph, so a still map costs nothing.
    anim: HashMap<u64, (f32, f32)>,

    /// Last pointer position in screen px, so a held drag near the window edge
    /// can keep panning between mouse-move events — the pointer stops moving
    /// long before the map has finished scrolling under it.
    pointer: (f32, f32),
    /// Modifiers as of the last pointer movement, so an edge pan can keep a
    /// drag's axis lock and grid snap alive while the pointer sits still.
    drag_mods: (bool, bool),

    /// Frame the map once the canvas reports its real size. `Camera::new`'s
    /// placeholder viewport is nothing like the window, so fitting in `new`
    /// would frame against the wrong rectangle.
    needs_fit: bool,
    last_bounds: Option<Bounds<Pixels>>,

    /// Pointer is over the titlebar strip, so the traffic lights are wanted.
    titlebar_hover: bool,
}

impl MindMapView {
    pub fn new(
        graph: Graph,
        pose: Option<Pose>,
        theme: Theme,
        path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut camera = Camera::new();
        // No saved pose means "frame the map", but the canvas has not been laid
        // out yet and text has not been shaped — both of which the fit needs.
        // It happens on the first render instead, which is one frame later.
        let needs_fit = match pose {
            Some(p) => {
                camera.set_pose((p.x, p.y), p.zoom);
                false
            }
            None => true,
        };
        let this = Self {
            graph,
            history: History::new(),
            camera,
            theme,
            focus_handle: cx.focus_handle(),
            status: "⌘/ for shortcuts".into(),
            status_ttl: STATUS_SECS,
            drag: None,
            hover: None,
            show_help: false,
            help_query: String::new(),
            show_minimap: false,
            shown_title: None,
            keymap: store::load_keymap(),
            capturing: None,
            menu: None,
            find: None,
            last_query: String::new(),
            cursor_on: true,
            blink_t: 0.0,
            bank: image_bank::Bank::new(),
            images_dir: store::images_root(),
            shaped: HashMap::new(),
            frame_hidden: HashSet::new(),
            path,
            dirty_since: None,
            save_error: None,
            read_only: None,
            anim: HashMap::new(),
            window_rect: None,
            pointer: (0.0, 0.0),
            drag_mods: (false, false),
            needs_fit,
            last_bounds: None,
            titlebar_hover: false,
        };
        this.start_tick(cx);
        this
    }

    /// The saved map could not be read, so this session is not writing. A toast
    /// would fade in under three seconds and leave nothing to distinguish this
    /// from a normal session — an hour's work would then vanish on quit.
    pub fn open_read_only(&mut self, msg: impl Into<SharedString>) {
        let msg = msg.into();
        self.read_only = Some(msg.clone());
        self.status = msg;
        self.status_ttl = STATUS_SECS;
    }

    /// One loop drives the caret blink, the camera easing and the autosave
    /// debounce. It idles at ~11 Hz and only speeds up while the camera moves.
    fn start_tick(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut interval = Duration::from_millis(16);
            let mut last = Instant::now();
            loop {
                cx.background_executor().timer(interval).await;
                let now = Instant::now();
                let dt = (now - last).as_secs_f32().min(0.1);
                last = now;
                let Ok(next) = this.update(cx, |view, cx| view.tick(dt, cx)) else {
                    break;
                };
                interval = next;
            }
        })
        .detach();
    }

    /// While a drag is held near the edge of the window, keep the canvas
    /// moving. Without it the reach of any drag is the viewport, so a node can
    /// never be moved somewhere that is not already on screen and a marquee can
    /// never take in more than one screenful.
    fn edge_pan(&mut self, dt: f32) -> bool {
        // Only once the gesture is genuinely under way. Firing on mouse-down
        // slid the world out from under the drag's slop check, so a click held
        // for a moment near the window edge became a move plus an undo step.
        let under_way = match &self.drag {
            Some(Drag::Node { moved, .. }) => *moved,
            Some(Drag::Marquee { origin, current, .. }) => origin != current,
            _ => false,
        };
        if !under_way {
            return false;
        }
        let Some(b) = self.last_bounds else {
            return false;
        };
        let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        let (px_, py_) = self.pointer;
        // Ramp in over the margin rather than switching on, so the canvas
        // creeps when you are just inside it and runs when you are at the edge.
        let ramp = |p: f32, lo: f32, hi: f32| -> f32 {
            if p < lo + EDGE_PAN_MARGIN {
                -((lo + EDGE_PAN_MARGIN - p) / EDGE_PAN_MARGIN).clamp(0.0, 1.0)
            } else if p > hi - EDGE_PAN_MARGIN {
                ((p - (hi - EDGE_PAN_MARGIN)) / EDGE_PAN_MARGIN).clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let (ax, ay) = (ramp(px_, x0, x0 + w), ramp(py_, y0, y0 + h));
        if ax == 0.0 && ay == 0.0 {
            return false;
        }
        // Negated: pushing the pointer right should bring the world leftwards.
        self.camera
            .pan_by_screen(-ax * EDGE_PAN_SPEED * dt, -ay * EDGE_PAN_SPEED * dt);
        // The pointer has not moved, but the world under it has, so the drag
        // has to be re-applied or the node stops dead while the canvas slides.
        let (wx, wy) = self.camera.to_world(px_, py_);
        match self.drag {
            Some(Drag::Node { .. }) => {
                self.drag_node_to(wx, wy);
            }
            Some(Drag::Marquee { .. }) => self.drag_marquee_to(wx, wy),
            _ => {}
        }
        true
    }

    /// Ease every in-flight node towards where the document says it is.
    /// Returns whether anything is still moving.
    fn step_motion(&mut self, dt: f32) -> bool {
        if self.anim.is_empty() {
            return false;
        }
        let t = 1.0 - (-dt / NODE_EASE_TAU).exp();
        let mut settled: Vec<u64> = Vec::new();
        for (&id, at) in self.anim.iter_mut() {
            let Some(n) = self.graph.nodes.get(&id) else {
                settled.push(id);
                continue;
            };
            let (dx, dy) = (n.x - at.0, n.y - at.1);
            if (dx * dx + dy * dy).sqrt() <= NODE_SETTLED {
                settled.push(id);
                continue;
            }
            at.0 += dx * t;
            at.1 += dy * t;
        }
        for id in settled {
            self.anim.remove(&id);
        }
        !self.anim.is_empty()
    }

    /// Note where everything is now, so the *next* layout change has somewhere
    /// to ease from. Called before anything that repositions nodes wholesale.
    fn capture_positions(&mut self) {
        for (&id, n) in &self.graph.nodes {
            // Keep an in-flight position: a second move within the animation
            // should continue from where the node is drawn, not snap to the
            // target the first one was still travelling to.
            self.anim.entry(id).or_insert((n.x, n.y));
        }
    }

    /// Where a node is drawn: its eased position while in flight, otherwise
    /// exactly where the document puts it.
    fn draw_pos(&self, id: u64, n: &crate::graph::Node) -> (f32, f32) {
        self.anim.get(&id).copied().unwrap_or((n.x, n.y))
    }

    fn tick(&mut self, dt: f32, cx: &mut Context<Self>) -> Duration {
        let mut dirty = false;

        // The traffic lights are asserted every tick rather than only when the
        // hover changes: AppKit puts them back on its own — a resize, a
        // fullscreen transition or a change of key window all bring them back
        // visible — and there is no notification to hang a one-shot on. Six
        // objc sends at 40 Hz is cheaper than the machinery to avoid them.
        mac::set_traffic_lights(self.titlebar_hover);

        if self.step_motion(dt) {
            dirty = true;
        }

        if self.edge_pan(dt) {
            dirty = true;
        }

        let pinched = self.apply_pinches();
        if pinched {
            dirty = true;
        }

        if self.camera.step(dt) {
            dirty = true;
        }

        self.blink_t += dt;
        if self.blink_t >= BLINK {
            self.blink_t = 0.0;
            self.cursor_on = !self.cursor_on;
            // Only a caret needs this frame — the label's, the find box's, or
            // the shortcut panel's search. Skip it when none is blinking.
            if self.graph.is_editing() || self.find.is_some() || self.show_help {
                dirty = true;
            }
        }

        if self.status_ttl > 0.0 {
            self.status_ttl = (self.status_ttl - dt).max(0.0);
            // The toast sits fully opaque, then fades over its last second — so
            // only the fade (and the frame it clears) changes a pixel. Holding
            // it opaque needs no repaint; without this guard every toast drove
            // ~2.6s of full-canvas rebuilds. `say` already painted it visible.
            if self.status_ttl <= 1.0 {
                dirty = true;
            }
        }

        if let Some(since) = self.dirty_since {
            // Never write mid-pinch: a synchronous disk write dropped into a
            // zoom gesture is a visible hitch. The debounce means it only waits
            // for the fingers to lift.
            if !pinched && since.elapsed() >= AUTOSAVE_DEBOUNCE {
                // Clear the flag only on success. Clearing first means a failed
                // write is never retried and nothing on screen ever says so.
                if self.write_to_disk() {
                    self.dirty_since = None;
                } else {
                    self.dirty_since = Some(Instant::now());
                }
                dirty = true;
            }
        }

        if dirty {
            cx.notify();
        }

        if self.camera.is_animating()
            || self.drag.is_some()
            || !self.anim.is_empty()
            || pinch::pending()
        {
            Duration::from_millis(8)
        } else {
            // The idle poll doubles as the ceiling on how long the first pinch
            // of a gesture waits before the loop notices it — the monitor
            // cannot wake us. 16 ms (~one 60 Hz frame) keeps that first step
            // from feeling late; once a gesture is under way `pending()` above
            // drops the loop to 8 ms.
            Duration::from_millis(16)
        }
    }

    /// Apply any trackpad pinches the NSEvent monitor queued. Returns true if
    /// the camera moved.
    ///
    /// All of a frame's magnify events are coalesced into ONE zoom at ONE
    /// anchor. Applying them one at a time — each at its own slightly different
    /// pointer position — is what made the zoom wobble and stutter: the scale
    /// jumped in uneven steps and the centre drifted between events. A gesture's
    /// scale is the product of its steps, anchored where the fingers are now.
    fn apply_pinches(&mut self) -> bool {
        let events = pinch::drain();
        let Some(last) = events.last().copied() else {
            return false;
        };
        self.sync_viewport();
        let factor = events
            .iter()
            .fold(1.0_f32, |acc, p| acc * (1.0 + p.magnification))
            .clamp(0.5, 2.0);
        self.camera.zoom_at(last.x, last.y, factor);
        self.touch();
        true
    }

    pub fn focus(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
    }

    // ---- persistence ---------------------------------------------------

    fn doc(&self) -> Doc {
        let (x, y) = self.camera.center();
        Doc::new(
            self.graph.clone(),
            Some(Pose {
                x,
                y,
                zoom: self.camera.zoom(),
            }),
            self.theme.dark,
            self.window_rect,
        )
    }

    /// Flush a pending autosave. Called on ⌘Q *and* from `Drop`, because
    /// closing the window tears the view down without quitting the app — the
    /// debounce would otherwise never fire and the last edits would be lost.
    pub fn save_on_quit(&mut self) {
        if self.dirty_since.is_some() && self.write_to_disk() {
            self.dirty_since = None;
        }
        // A failure here is the worst case — the view is being torn down, so
        // there is no longer anywhere to paint the warning. stderr is all that
        // is left, and `store::save` has already described what went wrong.
        if let Some(why) = &self.save_error {
            eprintln!("mind-map-rust: {why}");
        }
    }

    /// Returns whether the map is now safely on disk.
    fn write_to_disk(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return true;
        };
        match store::save(&path, &self.doc()) {
            Ok(()) => {
                self.save_error = None;
                true
            }
            Err(msg) => {
                // Sticky, not a toast: this one has to still be on screen when
                // the user decides to quit.
                self.save_error = Some(SharedString::from(msg));
                false
            }
        }
    }

    /// Mark the document changed; the tick loop writes it once things go quiet.
    fn touch(&mut self) {
        if self.path.is_some() {
            self.dirty_since = Some(Instant::now());
        }
    }

    /// File ▸ New — a blank map in its own file.
    fn on_new_document(&mut self, _: &NewDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.adopt_new_document(Graph::new(""), true, cx) {
            // A blank map starts on its root, at a plain 1× so the caret is ready.
            self.camera.set_pose((0.0, 0.0), 1.0);
            self.needs_fit = false;
            self.focus(window, cx);
            self.say("new map");
            cx.notify();
        }
    }

    /// Take `graph` as a brand-new, untitled document in its own file: the one
    /// on screen is flushed first (a pending autosave belongs to the *old*
    /// file), then everything session-scoped is reset around the new map. The
    /// backing store for `New` and every `Import`. Returns false if there was
    /// nowhere on disk to make the file — the caller then leaves the old map be.
    fn adopt_new_document(&mut self, graph: Graph, edit: bool, cx: &mut Context<Self>) -> bool {
        if self.dirty_since.is_some() && self.write_to_disk() {
            self.dirty_since = None;
        }
        let Some(path) = store::new_document_path() else {
            self.say("no home directory to make a new map in");
            cx.notify();
            return false;
        };
        self.graph = graph;
        self.history = History::new();
        self.shaped.clear();
        self.anim.clear();
        self.drag = None;
        self.hover = None;
        self.menu = None;
        self.find = None;
        self.save_error = None;
        self.read_only = None;
        self.path = Some(path.clone());
        if edit {
            self.graph.enter_edit();
        } else {
            self.graph.leave_edit();
        }
        // Write it once now, so the file the recents list points at exists.
        self.dirty_since = None;
        self.write_to_disk();
        store::remember(&path);
        set_menus(cx);
        true
    }

    /// File ▸ Duplicate — a copy of the current map as its own new document, the
    /// original left untouched on disk.
    fn on_duplicate_document(&mut self, _: &DuplicateDocument, window: &mut Window, cx: &mut Context<Self>) {
        if self.adopt_new_document(self.graph.clone(), false, cx) {
            self.needs_fit = true;
            self.focus(window, cx);
            self.say("duplicated map");
            cx.notify();
        }
    }

    /// File ▸ Revert — throw away unsaved changes and re-read the file on disk.
    fn on_revert_document(&mut self, _: &RevertDocument, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            self.say("this map has no file to revert to");
            cx.notify();
            return;
        };
        match store::load_from(&path) {
            Ok(doc) => {
                self.graph = doc.graph;
                self.graph.leave_edit();
                self.history = History::new();
                self.shaped.clear();
                self.anim.clear();
                self.drag = None;
                self.hover = None;
                self.menu = None;
                self.find = None;
                self.dirty_since = None;
                if let Some(p) = doc.camera {
                    self.camera.set_pose((p.x, p.y), p.zoom);
                    self.needs_fit = false;
                }
                self.focus(window, cx);
                self.say("reverted to the saved map");
            }
            Err(_) => self.say("could not re-read the file"),
        }
        cx.notify();
    }

    /// File ▸ Save As… — write the map to a new path the user names, and switch
    /// the session to owning that file from here on.
    fn on_save_as(&mut self, _: &SaveAs, _: &mut Window, cx: &mut Context<Self>) {
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(store::home_path)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let suggested = self
            .path
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "map.json".to_string());
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                view.path = Some(path.clone());
                view.dirty_since = None;
                view.save_error = None;
                if view.write_to_disk() {
                    store::remember(&path);
                    set_menus(cx);
                    view.say(store::recent_label(&path));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// File ▸ Reveal in Finder — show the document's file in Finder.
    fn on_reveal_in_finder(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        match self.path.clone() {
            Some(path) => {
                // Make sure what we point at exists first.
                let _ = self.write_to_disk();
                cx.reveal_path(&path);
            }
            None => {
                self.say("this map has no file yet — Save As first");
                cx.notify();
            }
        }
    }

    /// File ▸ Open Recent ▸ Clear Menu.
    fn on_clear_recents(&mut self, _: &ClearRecents, _: &mut Window, cx: &mut Context<Self>) {
        store::clear_recents();
        // Keep the current document on the list — it is still open — but drop
        // the rest. Re-remembering it rebuilds a one-entry list.
        if let Some(path) = self.path.clone() {
            store::remember(&path);
        }
        set_menus(cx);
        self.say("cleared recent maps");
        cx.notify();
    }

    // ---- colour ---------------------------------------------------------

    fn set_color(&mut self, color: Option<u8>, cx: &mut Context<Self>) {
        // Like the arrows, a colour key acts on the node the cursor is on when
        // nothing is explicitly selected.
        if self.graph.acting_on().is_empty() {
            let f = self.graph.focused_id;
            if self.graph.nodes.contains_key(&f) && self.graph.focused_edge.is_none() {
                self.graph.focus(f);
            } else {
                self.say("nothing selected");
                cx.notify();
                return;
            }
        }
        // Record only when it will change something — no dead undo step. A
        // colour never changes a label's size, so the shape cache stands.
        let will_change = self
            .graph
            .acting_on()
            .iter()
            .any(|id| self.graph.nodes.get(id).is_some_and(|n| n.color != color));
        if !will_change {
            cx.notify();
            return;
        }
        self.edit(Tag::Structural);
        self.graph.set_color_of_selection(color);
        match color {
            None => self.say("colour cleared"),
            Some(n) => self.say(format!("colour {n}")),
        }
        cx.notify();
    }
    fn on_set_color0(&mut self, _: &SetColor0, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(None, cx);
    }
    fn on_set_color1(&mut self, _: &SetColor1, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(1), cx);
    }
    fn on_set_color2(&mut self, _: &SetColor2, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(2), cx);
    }
    fn on_set_color3(&mut self, _: &SetColor3, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(3), cx);
    }
    fn on_set_color4(&mut self, _: &SetColor4, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(4), cx);
    }
    fn on_set_color5(&mut self, _: &SetColor5, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(5), cx);
    }
    fn on_set_color6(&mut self, _: &SetColor6, _: &mut Window, cx: &mut Context<Self>) {
        self.set_color(Some(6), cx);
    }

    // ---- minimap --------------------------------------------------------

    fn on_toggle_minimap(&mut self, _: &ToggleMinimap, _: &mut Window, cx: &mut Context<Self>) {
        self.show_minimap = !self.show_minimap;
        self.say(if self.show_minimap {
            "minimap on"
        } else {
            "minimap off"
        });
        cx.notify();
    }

    // ---- window / app shell ---------------------------------------------

    fn on_about(&mut self, _: &About, _: &mut Window, _: &mut Context<Self>) {
        crate::mac::about_panel();
    }

    fn on_hide_app(&mut self, _: &HideApp, _: &mut Window, _: &mut Context<Self>) {
        crate::mac::hide_app();
    }

    fn on_hide_others(&mut self, _: &HideOthers, _: &mut Window, _: &mut Context<Self>) {
        crate::mac::hide_others();
    }

    fn on_show_all(&mut self, _: &ShowAll, _: &mut Window, _: &mut Context<Self>) {
        crate::mac::show_all();
    }

    fn on_minimize_window(&mut self, _: &MinimizeWindow, window: &mut Window, _: &mut Context<Self>) {
        window.minimize_window();
    }

    fn on_zoom_window(&mut self, _: &ZoomWindow, window: &mut Window, _: &mut Context<Self>) {
        window.zoom_window();
    }

    fn on_toggle_fullscreen(
        &mut self,
        _: &ToggleFullScreen,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.toggle_fullscreen();
    }

    fn on_close_window(&mut self, _: &CloseWindow, window: &mut Window, _: &mut Context<Self>) {
        // Flush first: the app-quit hook that normally lands the last write does
        // not fire when a single window is closed rather than the app quit.
        self.save_on_quit();
        window.remove_window();
    }

    // ---- import / export ------------------------------------------------

    /// Write `contents` to a path the user names, then reveal it. Shared by
    /// every Export row; `suggested` is the default filename.
    fn export_to_file(&mut self, contents: String, suggested: String, cx: &mut Context<Self>) {
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .or_else(store::home_path)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let picked = cx.prompt_for_new_path(&dir, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else {
                return;
            };
            let wrote = std::fs::write(&path, contents);
            let _ = this.update(cx, |view, cx| {
                match wrote {
                    Ok(()) => {
                        view.say(format!("exported {}", store::recent_label(&path)));
                        // Surface it — an export you can't find is half an export.
                        cx.reveal_path(&path);
                    }
                    Err(e) => view.say(format!("export failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The document's base name, for suggesting an export filename.
    fn doc_stem(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "map".to_string())
    }

    fn on_export_markdown(&mut self, _: &ExportMarkdown, _: &mut Window, cx: &mut Context<Self>) {
        let s = convert::to_markdown(&self.graph);
        self.export_to_file(s, format!("{}.md", self.doc_stem()), cx);
    }
    fn on_export_opml(&mut self, _: &ExportOpml, _: &mut Window, cx: &mut Context<Self>) {
        let s = convert::to_opml(&self.graph);
        self.export_to_file(s, format!("{}.opml", self.doc_stem()), cx);
    }
    fn on_export_outline(&mut self, _: &ExportOutline, _: &mut Window, cx: &mut Context<Self>) {
        let s = convert::to_outline(&self.graph);
        self.export_to_file(s, format!("{}.txt", self.doc_stem()), cx);
    }
    fn on_export_svg(&mut self, _: &ExportSvg, _: &mut Window, cx: &mut Context<Self>) {
        let s = export_svg::to_svg(&self.graph);
        self.export_to_file(s, format!("{}.svg", self.doc_stem()), cx);
    }

    /// Import Markdown or OPML as a new untitled map. `parse` turns the file
    /// text into a graph; a picked file that will not parse into more than the
    /// root is still adopted — the user asked for it.
    fn import_with(
        &mut self,
        label: &'static str,
        parse: fn(&str) -> Graph,
        cx: &mut Context<Self>,
    ) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                let _ = this.update(cx, |view, cx| {
                    view.say("could not read that file");
                    cx.notify();
                });
                return;
            };
            let mut graph = parse(&text);
            graph.sanitize();
            let _ = this.update(cx, |view, cx| {
                if view.adopt_new_document(graph, false, cx) {
                    view.needs_fit = true;
                    view.say(format!("imported {label}"));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_import_markdown(&mut self, _: &ImportMarkdown, _: &mut Window, cx: &mut Context<Self>) {
        self.import_with("Markdown", convert::from_markdown, cx);
    }
    fn on_import_opml(&mut self, _: &ImportOpml, _: &mut Window, cx: &mut Context<Self>) {
        self.import_with("OPML", convert::from_opml, cx);
    }

    /// Ask for a map to open. The picker is modal to the system, not to us, so
    /// the answer comes back on a channel and the map keeps running meanwhile.
    fn on_open_document(&mut self, _: &OpenDocument, _: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |view, cx| view.open_document(path, cx));
        })
        .detach();
    }

    // The three menu rows whose key macOS will not draw for us; see the note on
    // `MenuNewChild`. They are the same verb, reached from the other direction.
    fn on_menu_new_child(&mut self, _: &MenuNewChild, w: &mut Window, cx: &mut Context<Self>) {
        self.on_create_child(&CreateChild, w, cx);
    }
    fn on_menu_new_sibling(&mut self, _: &MenuNewSibling, w: &mut Window, cx: &mut Context<Self>) {
        self.on_create_sibling(&CreateSibling, w, cx);
    }
    fn on_menu_commit(&mut self, _: &MenuCommit, w: &mut Window, cx: &mut Context<Self>) {
        self.on_commit(&Commit, w, cx);
    }

    /// A row of the Open Recent submenu. The list is re-read rather than
    /// remembered, so a slot always means what the menu was showing.
    fn on_open_recent(&mut self, ev: &OpenRecent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = store::recents().into_iter().nth(ev.slot) else {
            self.say("that one is no longer in the list");
            return;
        };
        self.open_document(path, cx);
    }

    /// Switch documents. The map on screen is written out first: a debounced
    /// autosave that has not fired yet belongs to the *old* file, and opening
    /// another one is exactly the moment it would otherwise be dropped.
    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_deref() == Some(path.as_path()) {
            self.say("already open");
            return;
        }
        if self.dirty_since.is_some() && self.write_to_disk() {
            self.dirty_since = None;
        }

        let doc = match store::load_path(&path) {
            store::Load::Opened(doc) => doc,
            store::Load::Fresh => {
                self.say(format!("nothing at {}", path.display()));
                return;
            }
            // Refuse rather than open empty-and-writable: this is the one path
            // where a file that is only *probably* a map could be overwritten.
            store::Load::Unreadable(msg) => {
                self.say(msg);
                return;
            }
        };

        self.graph = doc.graph;
        self.theme = Theme::from_dark(doc.dark);
        self.history = History::new();
        self.shaped.clear();
        self.anim.clear();
        self.drag = None;
        self.hover = None;
        self.menu = None;
        self.find = None;
        self.save_error = None;
        self.read_only = None;
        self.dirty_since = None;
        self.path = Some(path.clone());
        match doc.camera {
            Some(p) => {
                self.camera.set_pose((p.x, p.y), p.zoom);
                self.needs_fit = false;
            }
            // No saved pose: frame it, once the canvas has told us its size.
            None => self.needs_fit = true,
        }

        store::remember(&path);
        // The list the menu is built from has just changed, so the menu has to
        // be built again — this is the only submenu made of data.
        set_menus(cx);
        self.say(store::recent_label(&path));
        cx.notify();
    }

    /// Snapshot for undo, then mark dirty. Call before mutating the graph.
    fn edit(&mut self, tag: Tag) {
        self.history.record(&self.graph, tag);
        self.touch();
    }

    // ---- measurement ---------------------------------------------------
    //
    // Shaping is the expensive part of a frame and the answer to two separate
    // questions — where to draw a rosette, and how big its click target is —
    // so it is measured once, in screen units, and both readers divide by the
    // zoom they were asked about.

    fn base_font(&self, id: u64) -> f32 {
        if id == self.graph.root_id {
            FONT_ROOT
        } else {
            FONT_NODE
        }
    }

    fn weight_for(&self, base: f32) -> FontWeight {
        if base == FONT_ROOT {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        }
    }

    /// What a node's label renders as: its text, or a placeholder when empty.
    fn display_for(text: &str, is_focus: bool) -> &str {
        if !text.is_empty() {
            text
        } else if is_focus {
            " "
        } else {
            "·"
        }
    }

    /// Shape `id` at its *base* font unless the cache already holds that exact
    /// label. Zoom is deliberately absent: the width is taken once and scaled
    /// per frame (see [`Shaped::half`]), so panning and zooming never reshape.
    fn remeasure(&mut self, window: &mut Window, id: u64, editing: bool) {
        let Some(node) = self.graph.nodes.get(&id) else {
            return;
        };
        let is_focus = id == self.graph.focused_id && self.graph.focused_edge.is_none();
        let display = Self::display_for(&node.text, is_focus && editing);
        let base = self.base_font(id);
        let image = node.image.as_ref().map(|i| (i.w, i.h));
        if let Some(m) = self.shaped.get(&id) {
            // The image box is part of the key: a resize changes neither the
            // label nor the font, so a key of (text, base) alone would leave
            // hit-testing, the marquee and edge trimming using the old extent
            // for the rest of the session.
            if m.base == base && m.text.as_ref() == display && m.image == image {
                return;
            }
        }
        // A label is as wide as its widest line; height comes from the line
        // count. Measured at the base font, once.
        let weight = self.weight_for(base);
        let w_base = label_lines(display)
            .iter()
            .map(|l| shape_width(window, l, base, weight))
            .fold(0.0_f32, f32::max);
        let lines = label_lines(display).len().min(u16::MAX as usize) as u16;
        self.shaped.insert(
            id,
            Shaped {
                text: SharedString::from(display.to_owned()),
                base,
                w_base,
                lines,
                image,
            },
        );
    }

    /// Shape every node. Only needed before a fit, which has to know about nodes
    /// the last frame culled.
    fn measure_all(&mut self, window: &mut Window) {
        let ids: Vec<u64> = self.graph.nodes.keys().copied().collect();
        let editing = self.graph.is_editing();
        for id in ids {
            self.remeasure(window, id, editing);
        }
    }

    /// World-space half-width of a node's ring, from the last measurement.
    fn half_world(&self, id: u64) -> (f32, f32) {
        half_of(&self.shaped, self.camera.zoom(), id)
    }

    /// Hit-test against the rings that were actually drawn.
    fn hit_node(&self, wx: f32, wy: f32) -> Option<u64> {
        self.graph.hit_test_masked(wx, wy, &HashSet::new(), &self.frame_hidden, |id, _| {
            self.half_world(id)
        })
    }

    /// Reduce the graph to exactly what the canvas will draw, culled to the
    /// viewport. The canvas used to receive a full clone of the graph on every
    /// repaint — including every caret blink — and re-derive all of this.
    fn build_frame(&mut self, window: &mut Window, app: &mut App) -> Frame {
        let _t0 = crate::perf::enabled().then(std::time::Instant::now);
        let cam = self.camera;
        let z = cam.zoom();
        let (vx0, vy0, vx1, vy1) = cam.visible_world();
        let m = CULL_MARGIN / z;
        let (vx0, vy0, vx1, vy1) = (vx0 - m, vy0 - m, vx1 + m, vy1 + m);
        // Culled by the node's *extent*, not only its centre. A label is small
        // enough that the margin covers it, but a picture is as big as it was
        // dragged: testing the centre alone makes an image wider than twice the
        // margin vanish outright while half of it is still on screen — and
        // `hit_node` does not use the frame, so it would stay clickable while
        // invisible.
        let visible = |x: f32, y: f32, hw: f32, hh: f32| {
            x + hw >= vx0 && x - hw <= vx1 && y + hh >= vy0 && y - hh <= vy1
        };

        let root_id = self.graph.root_id;
        let focused_id = self.graph.focused_id;
        let focus_is_node = self.graph.focused_edge.is_none();
        let editing = self.graph.is_editing();
        let caret = self.graph.caret();
        let selection = self.graph.selected_range();
        let hover = self.hover;
        let drop_target = match self.drag {
            Some(Drag::Node { onto, .. }) => onto,
            _ => None,
        };
        let total_nodes = self.graph.nodes.len();

        // Which nodes need geometry at all: the ones on screen, plus the far
        // end of any channel that touches the screen — a half-visible edge
        // still has to be trimmed correctly at the end you can see.
        // One children index feeds both the hidden set and the child counts,
        // instead of each rebuilding its own — one O(n) pass per frame, not two.
        let kids = self.graph.children_index();
        let hidden = self.graph.hidden_set_from(&kids);
        let child_counts: HashMap<u64, usize> =
            kids.iter().map(|(&p, cs)| (p, cs.len())).collect();
        // Keep it for the mouse path, which would otherwise rebuild it per event.
        self.frame_hidden.clone_from(&hidden);
        let mut needed: HashSet<u64> = HashSet::with_capacity(64);
        let mut on_screen: HashSet<u64> = HashSet::with_capacity(64);
        for (&id, n) in &self.graph.nodes {
            // Folded away: not drawn, and not a reason to draw anything else.
            if hidden.contains(&id) {
                continue;
            }
            let (dx, dy) = self.draw_pos(id, n);
            // A picture's half-extent is known from the node alone — no
            // shaping, so this stays as cheap as the centre test it replaces.
            let (ehw, ehh) = n
                .image
                .as_ref()
                .map_or((0.0, 0.0), |i| (i.w * 0.5, i.h * 0.5));
            if visible(dx, dy, ehw, ehh) {
                on_screen.insert(id);
                needed.insert(id);
            }
        }
        for e in self.graph.edges.values() {
            if hidden.contains(&e.from) || hidden.contains(&e.to) {
                continue;
            }
            if on_screen.contains(&e.from) != on_screen.contains(&e.to) {
                needed.insert(e.from);
                needed.insert(e.to);
            }
        }

        // Find state, resolved once: which nodes are hits, and which hit the
        // view is parked on, so matches highlight and the rest dim.
        let (find_active, find_current, find_set) = match &self.find {
            Some(f) if !f.query.is_empty() => (
                true,
                f.matches.get(f.at).copied(),
                f.matches.iter().copied().collect::<HashSet<u64>>(),
            ),
            _ => (false, None, HashSet::new()),
        };

        let mut geom: HashMap<u64, (f32, f32, f32, f32)> = HashMap::with_capacity(needed.len());
        let mut nodes = Vec::with_capacity(on_screen.len());
        let mut ids: Vec<u64> = needed.iter().copied().collect();
        ids.sort_unstable();
        for id in ids {
            self.remeasure(window, id, editing);
            let Some(m) = self.shaped.get(&id) else { continue };
            // Screen size is derived from the base measurement and the current
            // zoom — no reshaping as the camera moves.
            let (hw, hh) = m.half(z);
            let (text, font) = (m.text.clone(), m.font(z));
            let Some(n) = self.graph.nodes.get(&id) else {
                continue;
            };
            let (wx, wy) = self.draw_pos(id, n);
            let text_empty = n.text.is_empty();
            let node_color = n.color;
            let is_root = id == root_id;
            let is_focus = id == focused_id && focus_is_node;
            let (cx, cy) = cam.to_screen(wx, wy);
            geom.insert(id, (cx, cy, hw, hh));

            if !on_screen.contains(&id) {
                continue;
            }
            // Only the node being edited needs caret and selection geometry;
            // for everything else it is wasted shaping. A band already says
            // where the text is, so the caret is hidden while one is up.
            // A picture is resolved only when it is big enough on screen to be
            // worth a texture. `paint_image` uploads the whole decoded bitmap
            // into the sprite atlas whatever size it is drawn at, so a fitted
            // map full of photos would otherwise make every one of them
            // resident to draw a smudge. The font-size threshold that gates
            // text cannot do this job: it is 1.5px, and at a fitted zoom a 15pt
            // label is 2.25px, so it never fires.
            let has_image = n.image.is_some();
            let mut missing = false;
            let image = (has_image && hw * 2.0 >= IMAGE_MIN_PX)
                .then(|| {
                    let name = n.image.as_ref().map(|i| i.name.clone())?;
                    let path = images::path(&self.images_dir, &name);
                    match self.bank.get(&path, window, app) {
                        image_bank::Resolved::Ready(data) => Some(data),
                        image_bank::Resolved::Missing => {
                            missing = true;
                            None
                        }
                        image_bank::Resolved::Loading => None,
                    }
                })
                .flatten();

            let weight = self.weight_for(self.base_font(id));
            let (caret_pt, sel) = if is_focus && editing {
                let (cxy, bands) = text_geometry(window, &text, font, weight, caret, selection);
                (bands.is_empty().then_some(cxy), bands)
            } else {
                (None, Vec::new())
            };
            nodes.push(PaintNode {
                id,
                text,
                image,
                has_image,
                image_missing: missing,
                cx,
                cy,
                hw,
                hh,
                font,
                is_root,
                is_focus,
                // Selection and cursor are independent now: the node the cursor
                // is on is very often also the whole selection, and excluding
                // it here left a plain click painting nothing at all.
                is_selected: self.graph.is_node_selected(id),
                is_drop_target: drop_target == Some(id),
                fold: child_counts.get(&id).is_some_and(|&c| c > 0).then(|| {
                    self.graph
                        .is_collapsed(id)
                        .then(|| self.graph.hidden_count(id))
                }),
                is_hover: hover == Some(Hover::Node(id)),
                empty: text_empty,
                color: node_color,
                dim: find_active && !find_set.contains(&id),
                find_hit: if find_active && find_set.contains(&id) {
                    Some(find_current == Some(id))
                } else {
                    None
                },
                caret: caret_pt,
                sel,
            });
        }
        // Drop cache entries for nodes that are gone, so a long session that
        // deletes a lot of subtrees does not hold their text forever.
        if self.shaped.len() > self.graph.nodes.len() * 2 + 64 {
            let live: HashSet<u64> = self.graph.nodes.keys().copied().collect();
            self.shaped.retain(|id, _| live.contains(id));
        }

        // Depth only matters for channels we are about to draw, so resolve it
        // for the visible set in one memoized pass rather than per edge.
        let depth_of = self.graph.depths_for(&needed);
        let focused_edge = self.graph.focused_edge;
        // The node whose parent link is about to be cut, if a disconnect drag is
        // in flight — its incoming edge paints as breaking.
        let severing = match self.drag {
            Some(Drag::Node { id, detach: true, .. }) => Some(id),
            _ => None,
        };
        let mut edges = Vec::new();
        for (&eid, e) in &self.graph.edges {
            if hidden.contains(&e.from) || hidden.contains(&e.to) {
                continue;
            }
            let (Some(a), Some(b)) = (geom.get(&e.from), geom.get(&e.to)) else {
                continue;
            };
            // Keep an edge if either end is on screen; a long channel crossing
            // the view with both ends outside is rare enough to drop.
            if !on_screen.contains(&e.from) && !on_screen.contains(&e.to) {
                continue;
            }
            let depth = depth_of.get(&e.to).copied().unwrap_or(0) as f32;
            let is_focus = focused_edge == Some(eid);
            let size = FONT_EDGE * z;
            let (caret_pt, sel) = if is_focus && editing {
                let (cxy, bands) =
                    text_geometry(window, &e.label, size, FontWeight::NORMAL, caret, selection);
                (bands.is_empty().then_some(cxy), bands)
            } else {
                (None, Vec::new())
            };
            edges.push(PaintEdge {
                label: SharedString::from(e.label.clone()),
                id: eid,
                a: (a.0, a.1),
                b: (b.0, b.1),
                ah: (a.2, a.3),
                bh: (b.2, b.3),
                depth,
                is_focus,
                is_hover: hover == Some(Hover::Edge(eid)),
                is_severing: severing == Some(e.to),
                caret: caret_pt,
                sel,
            });
        }
        // Stable paint order, but sorting only what survived the cull.
        edges.sort_unstable_by_key(|e| e.id);

        if let Some(t0) = _t0 {
            crate::perf::record_build(t0.elapsed().as_micros() as u64);
        }
        Frame {
            nodes,
            edges,
            cam,
            theme: self.theme,
            editing: self.graph.is_editing(),
            cursor_on: self.cursor_on && self.graph.is_editing(),
            grow: match self.drag {
                Some(Drag::Grow { from, to, moved: true, .. }) => self
                    .graph
                    .nodes
                    .get(&from)
                    .map(|n| (cam.to_screen(n.x, n.y), cam.to_screen(to.0, to.1))),
                _ => None,
            },
            marquee: match &self.drag {
                Some(Drag::Marquee { origin, current, .. }) => {
                    let a = cam.to_screen(origin.0, origin.1);
                    let b = cam.to_screen(current.0, current.1);
                    Some((a.0, a.1, b.0, b.1))
                }
                _ => None,
            },
            minimap: self.show_minimap.then(|| {
                // The whole map, not just what is on screen — the minimap's
                // whole point is to show what the viewport is missing.
                let (mut minx, mut miny) = (f32::MAX, f32::MAX);
                let (mut maxx, mut maxy) = (f32::MIN, f32::MIN);
                let mut dots = Vec::with_capacity(self.graph.nodes.len());
                for n in self.graph.nodes.values() {
                    if !(n.x.is_finite() && n.y.is_finite()) {
                        continue;
                    }
                    minx = minx.min(n.x);
                    miny = miny.min(n.y);
                    maxx = maxx.max(n.x);
                    maxy = maxy.max(n.y);
                    dots.push((n.x, n.y, n.color));
                }
                if dots.is_empty() {
                    minx = 0.0;
                    miny = 0.0;
                    maxx = 1.0;
                    maxy = 1.0;
                }
                let view = cam.visible_world();
                Minimap {
                    world: (minx, miny, maxx, maxy),
                    dots,
                    view,
                }
            }),
            total_nodes,
        }
    }

    fn hit_edge(&self, wx: f32, wy: f32) -> Option<u64> {
        // Keep the target a constant size on screen as the camera pulls back.
        let r = (layout::EDGE_HIT_R).max(16.0 / self.camera.zoom());
        self.graph.hit_test_edge_r(wx, wy, r)
    }

    // ---- status --------------------------------------------------------

    /// Feedback for a selection change. Selecting an edge used to nag "edge
    /// label · type · Esc for the node"; the hover caret on the line now says
    /// the same thing wordlessly, so this stays quiet. Kept as the single seam
    /// every selection change still routes through.
    fn label_status(&mut self) {}

    fn say(&mut self, msg: impl Into<SharedString>) {
        self.status = msg.into();
        self.status_ttl = STATUS_SECS;
    }

    // ---- camera --------------------------------------------------------

    fn fit(&mut self, window: &mut Window) {
        let bounds = self.map_bounds(window);
        self.camera.fly_to_fit(bounds, FIT_PAD);
    }

    /// The startup fit, once the canvas has told us how big it really is.
    /// Snaps rather than animates — the window has only just opened.
    fn fit_now(&mut self, window: &mut Window) {
        let bounds = self.map_bounds(window);
        self.camera.snap_to_fit(bounds, FIT_PAD);
    }

    /// Extent of the map in world units, using the widths from the last
    /// measurement. A fit can be asked for before anything has been painted at
    /// this zoom, so make sure every node has one first.
    fn map_bounds(&mut self, window: &mut Window) -> (f32, f32, f32, f32) {
        self.measure_all(window);
        self.graph.bounds_with(|id, _| self.half_world(id))
    }

    /// Keep the focused node comfortably on screen after a keyboard move.
    fn follow_focus(&mut self) {
        let n = self.graph.focused();
        let (x, y) = (n.x, n.y);
        self.camera.ensure_visible(x, y, KEEP_INSET);
    }

    // ---- actions: create / navigate ------------------------------------

    fn create(&mut self, dir: Direction, cx: &mut Context<Self>) {
        self.edit(Tag::Structural);
        self.graph.clear_edge_focus();
        self.graph.create_from_focused(dir, "");
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.follow_focus();
        self.say(match dir {
            Direction::Right => "created → · type to name it · ← parent",
            Direction::Left => "created ← · type to name it",
            Direction::Up => "created ↑ · type to name it · ← parent",
            Direction::Down => "created ↓ · type to name it · ← parent",
        });
        cx.notify();
    }

    fn on_create_right(&mut self, _: &CreateRight, _: &mut Window, cx: &mut Context<Self>) {
        self.create(Direction::Right, cx);
    }
    fn on_create_left(&mut self, _: &CreateLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.create(Direction::Left, cx);
    }
    fn on_create_up(&mut self, _: &CreateUp, _: &mut Window, cx: &mut Context<Self>) {
        self.create(Direction::Up, cx);
    }
    fn on_create_down(&mut self, _: &CreateDown, _: &mut Window, cx: &mut Context<Self>) {
        self.create(Direction::Down, cx);
    }

    /// ⇥ — a child of what you are on. The mind-map convention, and the verb
    /// this app made you reach for ⌘→ to get.
    fn on_create_child(&mut self, _: &CreateChild, _: &mut Window, cx: &mut Context<Self>) {
        // ⇥ is Browse-only, but the Edit menu dispatches straight past the
        // keymap — so commit the label and go, rather than doing nothing at
        // all and saying nothing about it.
        self.graph.leave_edit();
        self.create(Direction::Right, cx);
    }

    /// ⌘. — fold or unfold the branch under the cursor.
    fn on_toggle_fold(&mut self, _: &ToggleFold, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        let id = self.graph.focused_id;
        if self.graph.children_of(id).is_empty() {
            self.say("nothing under this one to fold");
            cx.notify();
            return;
        }
        self.toggle_fold_of(id);
        cx.notify();
    }

    /// Fold or unfold `id` and say what happened. The bubble and ⌘. are two
    /// doors onto this, not two implementations of it.
    fn toggle_fold_of(&mut self, id: u64) {
        self.edit(Tag::Structural);
        self.graph.toggle_collapsed(id);
        let n = self.graph.hidden_count(id);
        self.say(if self.graph.is_collapsed(id) {
            format!("folded · {n} hidden")
        } else {
            "unfolded".to_string()
        });
    }

    /// Put a new free node at a world point and drop into naming it.
    fn new_node_at(&mut self, wx: f32, wy: f32) {
        // Snapshot only if a node is actually going to appear.
        let mut next = self.graph.clone();
        match next.create_free_node(wx, wy) {
            Some(_) => {
                self.edit(Tag::Structural);
                self.graph = next;
                self.cursor_on = true;
                self.blink_t = 0.0;
                self.say("new node · type to name it");
            }
            None => self.say("out of node ids"),
        }
    }

    /// ⇧⇥ — a node beside what you are on, under the same parent.
    fn on_create_sibling(&mut self, _: &CreateSibling, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.cancel_drag();
        self.edit(Tag::Structural);
        self.graph.clear_edge_focus_only();
        match self.graph.create_sibling("") {
            Some(_) => {
                self.cursor_on = true;
                self.blink_t = 0.0;
                self.follow_focus();
                self.say("new sibling · type to name it");
            }
            None => self.say("out of node ids"),
        }
        cx.notify();
    }

    // ---- actions: restructure -------------------------------------------

    fn on_select_siblings(&mut self, _: &SelectSiblings, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.graph.select_siblings_of(self.graph.focused_id);
        self.say_selection();
        cx.notify();
    }

    /// Trade places with the sibling above / below. Pre-checked so a node with
    /// no neighbour that way records no undo step.
    fn reorder_sibling(&mut self, up: bool, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.cancel_drag();
        let id = self.graph.focused_id;
        if !self.graph.can_move_sibling(id, up) {
            self.say(if up { "already first" } else { "already last" });
            cx.notify();
            return;
        }
        self.edit(Tag::Structural);
        if self.graph.move_sibling(id, up) {
            self.follow_focus();
            self.say("reordered · ⌘Z to undo");
        }
        cx.notify();
    }

    fn on_move_sibling_up(&mut self, _: &MoveSiblingUp, _: &mut Window, cx: &mut Context<Self>) {
        self.reorder_sibling(true, cx);
    }

    fn on_move_sibling_down(&mut self, _: &MoveSiblingDown, _: &mut Window, cx: &mut Context<Self>) {
        self.reorder_sibling(false, cx);
    }

    fn on_outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.cancel_drag();
        let id = self.graph.focused_id;
        if !self.graph.can_outdent(id) {
            self.say("already at the top level");
            cx.notify();
            return;
        }
        self.edit(Tag::Structural);
        if self.graph.outdent(id) {
            self.graph.settle_after_drop(id);
            self.follow_focus();
            self.say("promoted · ⌘Z to undo");
        }
        cx.notify();
    }

    fn on_indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.cancel_drag();
        let id = self.graph.focused_id;
        if !self.graph.can_indent(id) {
            self.say("no sibling above to nest under");
            cx.notify();
            return;
        }
        self.edit(Tag::Structural);
        if self.graph.indent(id) {
            self.graph.settle_after_drop(id);
            self.follow_focus();
            self.say("demoted · ⌘Z to undo");
        }
        cx.notify();
    }

    fn on_focus_root(&mut self, _: &FocusRoot, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        self.graph.focus(self.graph.root_id);
        self.follow_focus();
        self.say_selection();
        cx.notify();
    }

    fn on_focus_last(&mut self, _: &FocusLast, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        let hidden = self.graph.hidden_set();
        let last = self
            .graph
            .nodes
            .iter()
            .filter(|(id, _)| !hidden.contains(id))
            .max_by(|a, b| {
                a.1.y
                    .partial_cmp(&b.1.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(&id, _)| id);
        if let Some(id) = last {
            self.graph.focus(id);
            self.follow_focus();
            self.say_selection();
        }
        cx.notify();
    }

    fn navigate(&mut self, dir: Direction, cx: &mut Context<Self>) {
        if self.graph.navigate(dir) {
            self.history.seal();
            self.cursor_on = true;
            self.blink_t = 0.0;
            self.follow_focus();
            self.label_status();
            cx.notify();
        }
    }

    // Arrows mean the map in Browse and the caret in Edit. The keymap decides
    // which, so these four only ever have to do one thing; see `bind_keys`.
    fn on_nav_left(&mut self, _: &NavLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Direction::Left, cx);
    }
    fn on_nav_right(&mut self, _: &NavRight, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Direction::Right, cx);
    }
    fn on_nav_up(&mut self, _: &NavUp, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Direction::Up, cx);
    }
    fn on_nav_down(&mut self, _: &NavDown, _: &mut Window, cx: &mut Context<Self>) {
        self.navigate(Direction::Down, cx);
    }

    /// Esc is one ladder, always climbing outwards from wherever you are
    /// deepest: the menu, the find field, the shortcut panel, the gesture in
    /// your hand, the text you are inside, the selection you are holding, the
    /// edge you are on — and only with nothing left to let go of, up the tree.
    fn on_escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        if self.close_menu() {
            cx.notify();
            return;
        }
        if self.close_find() {
            self.label_status();
            cx.notify();
            return;
        }
        if self.show_help {
            self.show_help = false;
            cx.notify();
            return;
        }
        if self.abort_drag() {
            self.say("cancelled");
            cx.notify();
            return;
        }
        if self.graph.is_editing() {
            self.graph.leave_edit();
            self.history.seal();
            self.label_status();
            cx.notify();
            return;
        }
        if self.graph.deselect_all() {
            self.say("nothing selected");
            cx.notify();
            return;
        }
        if self.graph.focused_edge.is_some() {
            self.graph.clear_edge_focus();
            self.history.seal();
            self.cursor_on = true;
            self.label_status();
            cx.notify();
            return;
        }
        if self.graph.focus_parent() {
            self.history.seal();
            self.cursor_on = true;
            self.follow_focus();
            self.label_status();
        } else {
            self.say("already at root");
        }
        cx.notify();
    }

    /// Put a drag back where it started and drop it. Returns false if there
    /// was nothing in flight.
    fn abort_drag(&mut self) -> bool {
        match self.drag.take() {
            Some(Drag::Node { moved: true, depth, .. }) => {
                // The pre-drag layout is the top of the undo stack. Take it
                // back rather than undoing to it, so a cancelled drag is not
                // left sitting on the redo stack waiting to be reapplied — but
                // only while it really is still on top. Anything that pushed a
                // snapshot mid-drag (a keystroke, say) owns it instead, and
                // popping that would revert the wrong edit.
                if self.history.depth() == depth {
                    if let Some(g) = self.history.discard_last() {
                        self.graph = g;
                        self.touch();
                    }
                }
                true
            }
            Some(Drag::Resize { id, start, moved, depth, .. }) => {
                if moved {
                    // Put the box back, then take the snapshot off the stack if
                    // it is still ours — and seal either way. Without the seal
                    // `history.last` stays `Tag::Move(id)`, so the next resize
                    // or move of the same node coalesces into a gesture the
                    // user cancelled, and one ⌘Z reverts both.
                    if let Some(node) = self.graph.nodes.get_mut(&id) {
                        if let Some(name) = node.image.as_ref().map(|i| i.name.clone()) {
                            node.image = Some(std::sync::Arc::new(crate::graph::NodeImage {
                                name,
                                w: start.0,
                                h: start.1,
                            }));
                        }
                    }
                    if self.history.depth() == depth {
                        self.history.discard_last();
                    }
                    self.history.seal();
                    self.touch();
                }
                true
            }
            Some(Drag::Marquee { base, focus_before, .. }) => {
                self.graph.set_node_selection(base);
                self.graph.set_cursor(focus_before);
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn on_select_all_nodes(&mut self, _: &SelectAllNodes, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            return self.on_select_all(&SelectAll, window, cx);
        }
        let all: Vec<u64> = self.graph.nodes.keys().copied().collect();
        self.graph.set_node_selection(all);
        self.say_selection();
        cx.notify();
    }

    fn on_focus_prev_sibling(&mut self, _: &FocusPrevSibling, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            return;
        }
        if self.graph.focus_sibling(-1) {
            self.history.seal();
            self.cursor_on = true;
            self.follow_focus();
            self.label_status();
            cx.notify();
        }
    }

    fn on_focus_next_sibling(&mut self, _: &FocusNextSibling, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            return;
        }
        if self.graph.focus_sibling(1) {
            self.history.seal();
            self.cursor_on = true;
            self.follow_focus();
            self.label_status();
            cx.notify();
        }
    }

    // ---- actions: reposition -------------------------------------------

    /// End any in-flight pointer gesture. Actions that replace or restructure
    /// the graph must call this first: a drag holds ids and a pre-drag snapshot
    /// that no longer describe the graph once undo/redo/tidy has run, and its
    /// later mouse-moves would mutate with no history entry behind them.
    fn cancel_drag(&mut self) {
        if self.drag.take().is_some() {
            self.history.seal();
        }
    }

    fn nudge(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        self.cancel_drag();
        // Arrows act on the current node: with nothing explicitly selected, grab
        // the one the cursor is on and move that, rather than dead-ending on a
        // "nothing selected" toast.
        if self.graph.acting_on().is_empty() {
            let f = self.graph.focused_id;
            if self.graph.nodes.contains_key(&f) && self.graph.focused_edge.is_none() {
                self.graph.focus(f);
            } else {
                cx.notify();
                return;
            }
        }
        self.capture_positions();
        self.edit(self.move_tag());
        // Movement is its own feedback; only the multi-node case, which is easy
        // to misjudge, says how much it took.
        if self.graph.move_selection_by(dx, dy) {
            self.follow_focus();
            let count = self.graph.acting_on().len();
            if count > 1 {
                self.say(format!("moved {count} nodes"));
            }
            cx.notify();
        }
    }

    fn on_nudge_left(&mut self, _: &NudgeLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(-layout::SNAP, 0.0, cx);
    }
    fn on_nudge_right(&mut self, _: &NudgeRight, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(layout::SNAP, 0.0, cx);
    }
    fn on_nudge_up(&mut self, _: &NudgeUp, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(0.0, -layout::SNAP, cx);
    }
    fn on_nudge_down(&mut self, _: &NudgeDown, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(0.0, layout::SNAP, cx);
    }

    // ⇧ + arrow — the same move, a longer stride. Five grid steps is far enough
    // to cross a gap between branches in a keypress or two without overshooting.
    fn on_nudge_left_big(&mut self, _: &NudgeLeftBig, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(-layout::SNAP * BIG_NUDGE, 0.0, cx);
    }
    fn on_nudge_right_big(&mut self, _: &NudgeRightBig, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(layout::SNAP * BIG_NUDGE, 0.0, cx);
    }
    fn on_nudge_up_big(&mut self, _: &NudgeUpBig, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(0.0, -layout::SNAP * BIG_NUDGE, cx);
    }
    fn on_nudge_down_big(&mut self, _: &NudgeDownBig, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge(0.0, layout::SNAP * BIG_NUDGE, cx);
    }

    fn on_tidy(&mut self, _: &TidyLayout, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_drag();
        // Tidying an already-tidy map changes nothing, and a snapshot for it
        // would cost a wasted ⌘Z and the whole redo stack.
        let mut tidied = self.graph.clone();
        tidied.tidy();
        if self.graph.positions_match(&tidied) {
            self.fit(window);
            self.say("already tidy");
            cx.notify();
            return;
        }
        self.capture_positions();
        self.edit(Tag::Structural);
        self.graph = tidied;
        self.fit(window);
        self.say("tidied · ⌘Z to undo");
        cx.notify();
    }

    fn on_delete(&mut self, _: &DeleteNode, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_drag();
        // An edge is what's selected: cut the connection, not a node. The child
        // and its whole subtree float free — nothing is destroyed. Guard the
        // undo step on the cut actually being possible.
        if let Some(eid) = self.graph.focused_edge {
            let child = self.graph.edges.get(&eid).map(|e| e.to);
            let cuttable = child.is_some_and(|c| {
                self.graph.nodes.get(&c).is_some_and(|n| n.parent.is_some())
            });
            if let (Some(child), true) = (child, cuttable) {
                self.edit(Tag::Structural);
                self.graph.detach(child);
                self.graph.clear_edge_focus();
                self.graph.focus(child);
                self.follow_focus();
                self.say("connection cut · ⌘Z to undo");
            }
            cx.notify();
            return;
        }
        // Check before recording: a refused delete must not push a no-op undo
        // step, and must not throw away a redo stack the user can still use.
        let targets = self.graph.acting_on();
        if targets.is_empty() {
            self.say("nothing selected");
            cx.notify();
            return;
        }
        if targets.iter().all(|&id| id == self.graph.root_id) {
            self.say("the root stays");
            cx.notify();
            return;
        }
        self.edit(Tag::Structural);
        let count = self.graph.delete_selection_of_nodes();
        if count > 0 {
            self.say(if count > 1 {
                format!("deleted {count} nodes · ⌘Z to undo")
            } else {
                "deleted · ⌘Z to undo".to_string()
            });
            self.follow_focus();
        }
        cx.notify();
    }

    // ---- actions: history ----------------------------------------------

    fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_drag();
        self.capture_positions();
        match self.history.undo(&self.graph) {
            Some(g) => {
                self.graph = g;
                self.touch();
                self.follow_focus();
                self.say("undo");
            }
            None => self.say("nothing to undo"),
        }
        cx.notify();
    }

    fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel_drag();
        self.capture_positions();
        match self.history.redo(&self.graph) {
            Some(g) => {
                self.graph = g;
                self.touch();
                self.follow_focus();
                self.say("redo");
            }
            None => self.say("nothing to redo"),
        }
        cx.notify();
    }

    // ---- actions: view --------------------------------------------------

    fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.camera.zoom_by(1.25);
        self.touch();
        cx.notify();
    }
    fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.camera.zoom_by(1.0 / 1.25);
        self.touch();
        cx.notify();
    }
    fn on_zoom_fit(&mut self, _: &ZoomFit, window: &mut Window, cx: &mut Context<Self>) {
        self.fit(window);
        self.touch();
        self.say("fit to map");
        cx.notify();
    }
    fn on_zoom_reset(&mut self, _: &ZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.graph.focused();
        let (x, y) = (n.x, n.y);
        self.camera.fly_to((x, y), 1.0);
        self.touch();
        self.say("100% · centered on focus");
        cx.notify();
    }
    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.theme = self.theme.toggled();
        self.touch();
        self.say(if self.theme.dark { "night" } else { "paper" });
        cx.notify();
    }
    fn on_toggle_help(&mut self, _: &ToggleHelp, _: &mut Window, cx: &mut Context<Self>) {
        // Leaving the panel mid-capture puts the keymap back first.
        if self.capturing.take().is_some() {
            self.reapply_keymap(cx);
        }
        self.show_help = !self.show_help;
        // Each visit starts from the whole list, not wherever the last search
        // left off — the panel is a reference, not a session.
        self.help_query.clear();
        self.cursor_on = true;
        self.blink_t = 0.0;
        cx.notify();
    }
    fn on_help_backspace(&mut self, _: &HelpBackspace, _: &mut Window, cx: &mut Context<Self>) {
        self.help_query.pop();
        self.cursor_on = true;
        self.blink_t = 0.0;
        cx.notify();
    }

    // ---- customising shortcuts ------------------------------------------

    /// Start listening for the next chord to bind to `action`. Every binding is
    /// cleared for the duration, so the keys the user presses reach the capture
    /// handler instead of firing whatever they are currently bound to.
    fn begin_capture(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.capturing = Some(action);
        cx.clear_key_bindings();
        self.say("press a chord — Esc to cancel");
        cx.notify();
    }

    /// Put the keymap back the way the overrides say — after a capture ends,
    /// whether it committed a new chord or was cancelled.
    fn reapply_keymap(&mut self, cx: &mut Context<Self>) {
        crate::apply_keymap(cx, &self.keymap);
        // Menu accelerators are read from the keymap when the bar is built.
        set_menus(cx);
    }

    /// A key arrived while a chord was being captured. Esc cancels; a bare
    /// modifier is ignored; anything else becomes the action's new chord.
    fn capture_chord(&mut self, ev: &Keystroke, cx: &mut Context<Self>) {
        let Some(action) = self.capturing else { return };
        let key = ev.key.as_str();
        if key == "escape" {
            self.capturing = None;
            self.reapply_keymap(cx);
            self.say("kept the old shortcut");
            cx.notify();
            return;
        }
        // A press of only a modifier has no key of its own — wait for the rest.
        if key.is_empty() {
            return;
        }
        // A shortcut must carry ⌘, ⌃ or ⌥. Binding a command to a bare key —
        // Esc, ⏎, ⇥, a letter — would shadow that key everywhere the command is
        // live (you could no longer type the letter, or leave a mode), so refuse
        // it and keep listening. ⇧ alone does not count as a modifier here.
        if !(ev.modifiers.platform || ev.modifiers.control || ev.modifiers.alt) {
            self.say("add ⌘, ⌥ or ⌃ — a bare key can't be a shortcut");
            cx.notify();
            return;
        }
        let mut parts: Vec<&str> = Vec::new();
        if ev.modifiers.platform {
            parts.push("cmd");
        }
        if ev.modifiers.control {
            parts.push("ctrl");
        }
        if ev.modifiers.alt {
            parts.push("alt");
        }
        if ev.modifiers.shift {
            parts.push("shift");
        }
        parts.push(key);
        let chord = parts.join("-");

        // A chord held by another *rebindable* verb would fire two commands at
        // once, so a rebind steals it: every other verb that answers to this
        // chord right now — whether by an override or by its default — is set
        // unbound (empty), which `bind_rebindable` then skips. A clash with a
        // fixed structural key is left to the user.
        self.keymap.insert(action.to_string(), chord.clone());
        let clashing: Vec<&'static str> = crate::REBINDABLE
            .iter()
            .map(|(n, _, _)| *n)
            .filter(|n| *n != action && crate::chord_for(n, &self.keymap) == chord)
            .collect();
        for n in clashing {
            self.keymap.insert(n.to_string(), String::new());
        }
        store::save_keymap(&self.keymap);
        self.capturing = None;
        self.reapply_keymap(cx);
        self.say(format!("bound to {chord}"));
        cx.notify();
    }

    /// Drop a custom chord and fall back to the default.
    fn reset_shortcut(&mut self, action: &'static str, cx: &mut Context<Self>) {
        if self.keymap.remove(action).is_some() {
            store::save_keymap(&self.keymap);
            self.reapply_keymap(cx);
            self.say("shortcut reset");
            cx.notify();
        }
    }

    /// The editable list at the top of the shortcut panel: every rebindable verb
    /// with its current chord, a click to capture a new one, and a reset for the
    /// ones that carry a custom chord.
    fn build_customise(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let th = self.theme;
        let capturing = self.capturing;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .mb_3()
            .pb_3()
            .border_b_1()
            .border_color(ca(th.ink, 0.1))
            .child(
                div()
                    .text_xs()
                    .text_color(c(th.accent))
                    .child("customise · click a chord, then press keys"),
            )
            .children(crate::REBINDABLE.iter().enumerate().map(|(i, entry)| {
                let name: &'static str = entry.0;
                let label: &'static str = entry.1;
                let capturing_this = capturing == Some(name);
                let chord = if capturing_this {
                    "press a chord…".to_string()
                } else {
                    let c = crate::chord_for(name, &self.keymap);
                    // An empty override means the chord was handed to another
                    // verb; show it as unbound rather than blank.
                    if c.is_empty() { "unbound".to_string() } else { c }
                };
                let is_custom = self.keymap.contains_key(name);
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .text_xs()
                    .child(
                        div()
                            .w(px(HELP_KEY_W))
                            .flex_shrink_0()
                            .text_color(c(th.ink))
                            .child(SharedString::from(label)),
                    )
                    .child(
                        div()
                            .id(("rebind", i))
                            .px_2()
                            .py(px(1.))
                            .min_w(px(96.))
                            .rounded_md()
                            .border_1()
                            .border_color(if capturing_this {
                                ca(th.accent, 0.8)
                            } else {
                                ca(th.ink, 0.2)
                            })
                            .text_color(if capturing_this {
                                c(th.accent)
                            } else {
                                c(th.ink_soft)
                            })
                            .hover(|s| s.bg(ca(th.accent, 0.1)))
                            .child(SharedString::from(chord))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.begin_capture(name, cx)),
                            ),
                    )
                    .child(if is_custom {
                        div()
                            .id(("reset", i))
                            .px_1()
                            .text_color(ca(th.ink, 0.5))
                            .hover(|s| s.text_color(c(th.ink)))
                            .child("reset")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.reset_shortcut(name, cx)),
                            )
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
            }))
            .into_any_element()
    }
    fn on_save_now(&mut self, _: &SaveNow, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(why) = self.read_only.clone() {
            self.say(why);
        } else if self.path.is_none() {
            self.say("scratch map · not saved (run without --demo/--empty)");
        } else if self.write_to_disk() {
            self.dirty_since = None;
            self.say("saved");
        } else {
            let why = self.save_error.clone().unwrap_or_else(|| "save failed".into());
            self.say(why);
        }
        cx.notify();
    }

    // ---- actions: edit --------------------------------------------------

    fn on_toggle_edge_label(&mut self, _: &ToggleEdgeLabel, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.toggle_edge_edit_on_focused() {
            self.history.seal();
            self.cursor_on = true;
            self.label_status();
        } else {
            self.say("no edge into this node");
        }
        cx.notify();
    }


    /// Move the caret, and blink it back on so the jump is visible.
    fn caret_moved(&mut self, moved: bool, cx: &mut Context<Self>) {
        if moved {
            self.history.seal();
        }
        self.cursor_on = true;
        self.blink_t = 0.0;
        cx.notify();
    }

    fn on_caret_left(&mut self, _: &CaretLeft, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_left(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_right(&mut self, _: &CaretRight, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_right(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_word_left(&mut self, _: &CaretWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_word_left(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_word_right(&mut self, _: &CaretWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_word_right(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_home(&mut self, _: &CaretHome, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_home(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_end(&mut self, _: &CaretEnd, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_end(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_up(&mut self, _: &CaretUp, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_up(false);
        self.caret_moved(m, cx);
    }
    fn on_caret_down(&mut self, _: &CaretDown, _: &mut Window, cx: &mut Context<Self>) {
        let m = self.graph.caret_down(false);
        self.caret_moved(m, cx);
    }

    /// ⇧↩ — a hard line break inside the label, the same insertion path as any
    /// other character so undo and the caret follow it exactly.
    fn on_insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
        if !self.graph.is_editing() {
            return;
        }
        self.edit(self.text_tag());
        self.graph.insert_char('\n');
        self.shaped.clear();
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    // ---- actions: selection ---------------------------------------------
    //
    // ⇧ + a motion only means "select" while editing text. In Browse the same
    // chords have to stay free: ⇧ there is the subtree modifier, and ⇧⇥ walks
    // siblings. So each of these is a no-op outside Edit rather than a mode
    // switch — a key that quietly changes mode is how you lose your place.

    fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_left(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_right(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_word_left(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_word_right(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_home(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_end(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_up(true);
            self.caret_moved(m, cx);
        }
    }
    fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            let m = self.graph.caret_down(true);
            self.caret_moved(m, cx);
        }
    }

    /// ⌘A over a label. Browse binds the same key to "select every node", and
    /// the keymap decides which — see `bind_keys`.
    fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if !self.graph.is_editing() {
            return;
        }
        let m = self.graph.select_all();
        self.caret_moved(m, cx);
    }

    // ---- actions: clipboard ---------------------------------------------

    /// What the *text* clipboard acts on while editing: the selection, or the
    /// whole label when there isn't one. Browse binds these keys to the node
    /// clipboard instead.
    fn clipboard_text(&self) -> Option<String> {
        match self.graph.selected_text() {
            Some(s) => Some(s.to_owned()),
            None => {
                let all = self.graph.active_text();
                (!all.is_empty()).then(|| all.to_owned())
            }
        }
    }

    fn on_copy(&mut self, _: &CopyText, _: &mut Window, cx: &mut Context<Self>) {
        let whole = self.graph.selected_text().is_none();
        match self.clipboard_text() {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.say(if whole { "copied the whole label" } else { "copied" });
            }
            None => self.say("nothing to copy"),
        }
        cx.notify();
    }

    fn on_cut(&mut self, _: &CutText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.clipboard_text() else {
            self.say("nothing to cut");
            cx.notify();
            return;
        };
        // Cut rewrites the text, so like typing it drops into Edit first.
        if !self.graph.is_editing() {
            self.graph.enter_edit();
        }
        self.edit(Tag::Structural);
        if !self.graph.delete_selection() {
            self.graph.set_focused_text(String::new());
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.say("cut · ⌘Z to undo");
        cx.notify();
    }

    fn on_paste(&mut self, _: &PasteText, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            self.say("clipboard is empty");
            cx.notify();
            return;
        };
        // A label is one line until backlog A8, and pasting a paragraph into
        // one would otherwise smuggle in newlines the renderer cannot show.
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.is_empty() {
            self.say("clipboard has no text");
            cx.notify();
            return;
        }
        if !self.graph.is_editing() {
            self.graph.enter_edit();
        }
        self.edit(Tag::Structural);
        self.graph.insert_str(&flat);
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    fn on_delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        // Nothing ahead of the caret: no edit, so no snapshot and no discarded
        // redo stack. ⌦ at the end of a label used to cost a dead ⌘Z.
        if !self.graph.is_editing() || !self.graph.can_delete_forward() {
            return;
        }
        self.edit(self.text_tag());
        self.graph.delete_forward();
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    fn on_delete_word(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        // ⌥⌫ at offset 0 removes nothing, and its tag never coalesces, so each
        // repeat used to be its own dead undo step.
        if !self.graph.is_editing() || !self.graph.can_delete_word_left() {
            return;
        }
        self.edit(Tag::Structural);
        self.graph.delete_word_left();
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    /// ↩ — the one key that crosses the two modes: it opens editing from
    /// Browse and commits from Edit.
    fn on_commit(&mut self, _: &Commit, _: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            self.graph.leave_edit();
            self.history.seal();
        } else {
            self.graph.enter_edit();
        }
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        // Nothing behind the caret: no edit, so no snapshot and no discarded
        // redo stack. ⌫ at offset 0 used to cost a dead ⌘Z.
        if !self.graph.is_editing() || !self.graph.can_backspace() {
            return;
        }
        self.edit(self.text_tag());
        self.graph.backspace();
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

    fn on_clear(&mut self, _: &ClearFocusText, _: &mut Window, cx: &mut Context<Self>) {
        // A no-op clear would still push a snapshot and wipe the redo stack.
        if !self.graph.is_editing() || self.graph.active_text().is_empty() {
            return;
        }
        self.edit(Tag::Structural);
        self.graph.set_focused_text(String::new());
        self.cursor_on = true;
        self.label_status();
        cx.notify();
    }

    /// Undo tag for a move, keyed on *what would move* rather than on the
    /// focused node. Keying on focus alone let two nudges either side of a
    /// selection change coalesce into one step, so a move of the whole map
    /// could not be undone without also undoing the single-node move before it.
    fn move_tag(&self) -> Tag {
        let mut h: u64 = 0xcbf29ce484222325;
        for id in self.graph.acting_on() {
            h ^= id;
            h = h.wrapping_mul(0x100000001b3);
        }
        Tag::Move(h)
    }

    /// Undo tag for typing. Node and edge ids come from one counter at runtime,
    /// but a hand-edited file can key a node and an edge the same — without the
    /// discriminant, one ⌘Z would then revert an edit to both.
    fn text_tag(&self) -> Tag {
        match self.graph.selection() {
            Selection::Edge(id) => Tag::EdgeText(id),
            Selection::Node(id) => Tag::Text(id),
        }
    }

    // ---- actions: the node clipboard -------------------------------------
    //
    // ⌘C in Browse copies *nodes*; in Edit it copies text. The keymap picks,
    // so neither handler has to guess which noun the user meant.

    fn on_copy_nodes(&mut self, _: &CopyNodes, window: &mut Window, cx: &mut Context<Self>) {
        // The keymap only binds this in Browse, but a menu item dispatches
        // straight into the listener with no key context consulted — so Edit ▸
        // Copy would otherwise copy *nodes* while you are typing in a label.
        if self.graph.is_editing() {
            return self.on_copy(&CopyText, window, cx);
        }
        match self.graph.copy_subtrees(&self.graph.acting_on()) {
            Some(clip) => {
                let n = clip.len();
                cx.write_to_clipboard(ClipboardItem::new_string(clip.to_text()));
                self.say(format!("copied {n} node{}", plural(n)));
            }
            None => self.say("nothing selected"),
        }
        cx.notify();
    }

    fn on_cut_nodes(&mut self, _: &CutNodes, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            return self.on_cut(&CutText, window, cx);
        }
        let targets = self.graph.acting_on();
        if targets.is_empty() {
            self.say("nothing selected");
            cx.notify();
            return;
        }
        if targets.iter().all(|&id| id == self.graph.root_id) {
            self.say("the root stays");
            cx.notify();
            return;
        }
        let Some(clip) = self.graph.copy_subtrees(&targets) else {
            cx.notify();
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(clip.to_text()));
        self.edit(Tag::Structural);
        let n = self.graph.delete_selection_of_nodes();
        self.say(format!("cut {n} node{} · ⌘Z to undo", plural(n)));
        self.follow_focus();
        cx.notify();
    }

    /// The middle of what is currently on screen, in world units — where a new
    /// picture goes when nothing said otherwise.
    fn viewport_centre_world(&self) -> (f32, f32) {
        let (x0, y0, x1, y1) = self.camera.visible_world();
        ((x0 + x1) * 0.5, (y0 + y1) * 0.5)
    }

    /// Put a picture on the canvas: import it into the store, then make a node
    /// that shows it.
    ///
    /// The import runs on the background executor — hashing and writing twenty
    /// megabytes is not something a frame waits for — and the node is created
    /// when it lands.
    fn import_images(&mut self, source: ImportSource, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.images_dir.clone();
        cx.spawn_in(window, async move |this, cx| {
            let imported = cx
                .background_executor()
                .spawn(async move {
                    match source {
                        ImportSource::Bytes(bytes) => vec![images::import_bytes(&dir, &bytes)],
                        ImportSource::Files(paths) => paths
                            .iter()
                            .map(|path| images::import_file(&dir, path))
                            .collect(),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.place_images(imported, window.scale_factor(), cx);
            })
            .ok();
        })
        .detach();
    }

    /// Create one node per imported picture, stepping to the right so a
    /// multi-file drop does not stack them all on one spot.
    fn place_images(
        &mut self,
        imported: Vec<std::io::Result<images::Import>>,
        scale: f32,
        cx: &mut Context<Self>,
    ) {
        let scale = if scale > 0.0 { scale } else { 2.0 };
        // The import landed some arbitrary time after it was asked for, and it
        // is about to restructure the graph. A drag in flight holds ids and a
        // pre-drag snapshot that will no longer describe it.
        self.cancel_drag();

        // Where the view is *now*. Reading it before the await would put the
        // node wherever the canvas happened to be seconds ago — hashing a phone
        // photo, or shelling out to `sips` for a HEIC, is not instant — and the
        // toast would announce an image nobody can see.
        let mut made = 0;
        let mut failed = 0;
        let (mut x, y) = self.viewport_centre_world();
        let mut prev_w = 0.0_f32;
        let mut first: Option<u64> = None;
        for result in imported {
            let Ok(import) = result else {
                failed += 1;
                continue;
            };
            // Its own size in world units — pixels over the display's scale
            // factor, not a hard 2, which is right on every Retina screen and
            // wrong by half on a 1x monitor — then fitted to a box on its
            // *longest* edge. Bounding the width alone is what lets a 1200x8000
            // page screenshot arrive as a node seventy times taller than text.
            let (iw, ih) = (
                import.w.max(1) as f32 / scale,
                import.h.max(1) as f32 / scale,
            );
            let longest = iw.max(ih);
            let scale = (DEFAULT_IMAGE_W / longest).min(1.0).max(MIN_IMAGE_W / longest);
            let (w, h) = (iw * scale, ih * scale);
            if made == 0 {
                self.edit(Tag::Structural);
            }
            let Some(id) = self.graph.create_free_node(x, y) else {
                break;
            };
            if let Some(node) = self.graph.nodes.get_mut(&id) {
                node.image = Some(std::sync::Arc::new(crate::graph::NodeImage {
                    name: import.name.as_str().into(),
                    w,
                    h,
                }));
            }
            // `create_free_node` drops into Edit so a new node can be named. A
            // picture has no label to type, and `paint_nodes` draws no caret on
            // one — so staying in Edit would leave the keyboard pointed at an
            // invisible empty string, with ⌫ and every node command inert.
            self.graph.leave_edit();
            // Centre-to-centre, so a wider picture does not land on top of the
            // one before it.
            x += prev_w * 0.5 + w * 0.5 + 24.0;
            prev_w = w;
            first.get_or_insert(id);
            made += 1;
        }
        if made > 0 {
            self.history.seal();
            self.touch();
            // Bring the view to what was just made, in case the canvas moved
            // while the import ran.
            if let Some(id) = first {
                self.graph.set_cursor(id);
                self.follow_focus();
            }
            self.say(if made == 1 {
                "image added".to_string()
            } else {
                format!("{made} images added")
            });
        }
        if failed > 0 {
            self.say(format!(
                "{failed} file{} could not be read as an image",
                if failed == 1 { "" } else { "s" }
            ));
        }
        cx.notify();
    }

    /// Files dropped on the canvas. Images become nodes at the drop point;
    /// everything else is not something this app can hold.
    fn drop_files(
        &mut self,
        paths: &gpui::ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let files: Vec<std::path::PathBuf> = paths
            .paths()
            .iter()
            .filter(|p| images::is_image_file(p))
            .cloned()
            .collect();
        if files.is_empty() {
            self.say("only images can be dropped on the map");
            cx.notify();
            return;
        }
        // GPUI's `on_drop` hands over the payload but not where it was
        // released, and a pointer position remembered from the last mouse-move
        // is stale by then — a drag from the Finder sends none. `place_images`
        // uses the middle of the view, which is at least where the reader is
        // looking.
        self.import_images(ImportSource::Files(files), window, cx);
    }

    fn on_paste_nodes(&mut self, _: &PasteNodes, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_editing() {
            return self.on_paste(&PasteText, window, cx);
        }
        // A picture on the pasteboard — a screenshot, most of the time —
        // becomes a node showing it. Note that macOS offers plain text in
        // preference to an image when both are present, so something copied
        // out of a browser arrives as its text; dropping the file covers that.
        let item = cx.read_from_clipboard();
        if let Some(bytes) = item.as_ref().and_then(clipboard_image) {
            self.import_images(ImportSource::Bytes(bytes), window, cx);
            return;
        }
        let text = item.and_then(|i| i.text());
        if let Some(clip) = text.as_deref().and_then(Clip::from_text) {
            self.paste_clip(&clip, cx);
            return;
        }
        // Not our own clip: treat whatever text is there as an outline and grow
        // it under the current node — pasting notes copied from anywhere makes
        // nodes, instead of the old "no nodes on the clipboard" dead end.
        let outline = text.unwrap_or_default();
        if outline.trim().is_empty() {
            self.say("nothing to paste");
            cx.notify();
            return;
        }
        let under = self.graph.focused_id;
        let mut next = self.graph.clone();
        match next.paste_outline(under, &outline) {
            Some(first) => {
                self.edit(Tag::Structural);
                self.graph = next;
                self.graph.leave_edit();
                self.graph.focus(first);
                self.follow_focus();
                self.say("pasted outline · ⌘Z to undo");
            }
            None => self.say("nothing to paste"),
        }
        cx.notify();
    }

    fn on_duplicate_nodes(&mut self, _: &DuplicateNodes, _: &mut Window, cx: &mut Context<Self>) {
        self.graph.leave_edit();
        let Some(clip) = self.graph.copy_subtrees(&self.graph.acting_on()) else {
            self.say("nothing selected");
            cx.notify();
            return;
        };
        self.paste_clip(&clip, cx);
    }

    /// Land a clip below-right of what it came from, hanging off the node the
    /// cursor is on. Offsetting matters: pasted on top of the original it looks
    /// like nothing happened.
    fn paste_clip(&mut self, clip: &Clip, cx: &mut Context<Self>) {
        // Clear of the original in both axes: pasted on top of what it came
        // from, a copy reads as a rendering glitch rather than a new branch.
        let anchor = self.graph.focused();
        let (x, y) = (
            anchor.x + layout::TIDY_COL,
            anchor.y + layout::TIDY_ROW,
        );
        let parent = Some(self.graph.focused_id);
        self.edit(Tag::Structural);
        let fresh = self.graph.paste(clip, x, y, parent);
        if fresh.is_empty() {
            self.say("out of node ids");
        } else {
            self.say(format!("pasted {} node{} · ⌘Z to undo", fresh.len(), plural(fresh.len())));
            self.follow_focus();
        }
        cx.notify();
    }

    // ---- pointer --------------------------------------------------------

    fn sync_viewport(&mut self) {
        if let Some(b) = self.last_bounds {
            self.camera.set_viewport(Viewport::new(
                b.origin.x.into(),
                b.origin.y.into(),
                b.size.width.into(),
                b.size.height.into(),
            ));
        }
    }

    /// Put the caret where a click landed, in whatever label is active. Both
    /// call sites are already guarded so the thing clicked *is* the active
    /// selection, which is why one measurement serves node and edge alike.
    fn place_caret_at(&mut self, sx: f32, sy: f32, window: &mut Window) {
        let Some((text, cx, cy, font, weight)) = self.active_label_metrics() else {
            return;
        };
        if text.is_empty() {
            self.graph.set_caret(0);
            return;
        }
        if font < MIN_READABLE_PX {
            // Painted as a mark — there are no letters to aim between.
            self.graph.caret_end(false);
            return;
        }
        let at = index_at_point(window, &text, font, weight, sx - cx, sy - cy);
        self.graph.set_caret(at);
    }

    /// Drag the far end of the text selection to wherever the pointer is now.
    /// Which text that is comes from the graph, so this works the same for a
    /// node label and an edge label.
    fn extend_text_selection(&mut self, sx: f32, sy: f32, window: &mut Window) -> bool {
        let Some((text, cx, cy, font, weight)) = self.active_label_metrics() else {
            return false;
        };
        if font < MIN_READABLE_PX || text.is_empty() {
            return false;
        }
        let at = index_at_point(window, &text, font, weight, sx - cx, sy - cy);
        self.graph.extend_caret_to(at)
    }

    /// Text, screen-x of its centre, painted size and weight for whatever is
    /// being edited — the four things every caret measurement needs.
    fn active_label_metrics(&self) -> Option<(String, f32, f32, f32, FontWeight)> {
        let z = self.camera.zoom();
        match self.graph.selection() {
            Selection::Edge(eid) => {
                let label = self.graph.edges.get(&eid)?.label.clone();
                let (wx, wy) = self.graph.edge_mid(eid)?;
                let (cx, cy) = self.camera.to_screen(wx, wy);
                Some((label, cx, cy, FONT_EDGE * z, FontWeight::NORMAL))
            }
            Selection::Node(id) => {
                let node = self.graph.nodes.get(&id)?;
                let (cx, cy) = self.camera.to_screen(node.x, node.y);
                let base = self.base_font(id);
                Some((node.text.clone(), cx, cy, base * z, self.weight_for(base)))
            }
        }
    }

    /// The caret is already placed; a repeat click widens it into a selection.
    /// Twice takes the word under the pointer, three times the whole label.
    fn widen_to_click(&mut self, clicks: usize) {
        match clicks {
            2 => {
                let at = self.graph.caret();
                self.graph.select_word_at(at);
            }
            n if n >= 3 => {
                self.graph.select_all();
            }
            _ => {}
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_viewport();
        self.camera.stop();
        // Touching the map ends the search — otherwise a click that puts a
        // blinking caret in a label still sends every keystroke to the query.
        self.close_find();
        // Interaction beats animation: land everything where it is actually
        // going, so a press hits what the pointer is over rather than where a
        // node happened to be drawn a frame ago.
        self.anim.clear();
        let (sx, sy) = (f32::from(ev.position.x), f32::from(ev.position.y));
        let (wx, wy) = self.camera.to_world(sx, sy);

        // ⌃-click is a right-click on macOS, and gpui reports it as a left
        // press with the control modifier — both have to open the menu or
        // trackpad users never see it.
        if ev.button == MouseButton::Right
            || (ev.button == MouseButton::Left && ev.modifiers.control)
        {
            self.open_context_menu(sx, sy, cx);
            return;
        }
        // Any other press dismisses an open menu and does nothing else, so a
        // click aimed at closing it cannot also move something.
        if self.close_menu() {
            cx.notify();
            return;
        }

        // The camera is on the middle button and on trackpad swipes. A plain
        // drag has to stay free for selecting — that is the gesture a pointer
        // is for on a canvas, and a trackpad already pans without one.
        if ev.button == MouseButton::Middle {
            self.drag = Some(Drag::Pan {
                last: (sx, sy),
                last_at: Instant::now(),
                vel: (0.0, 0.0),
                moved: false,
            });
            cx.notify();
            return;
        }
        if ev.button != MouseButton::Left {
            return;
        }

        // The resize grip sits on the picture's own corner, so it is tested
        // before the node for the same reason the fold bubble is: a test that
        // ran afterwards would never see it.
        if let Some(id) = self.hit_resize_grip(sx, sy) {
            self.graph.leave_edit();
            let image = self.graph.nodes.get(&id).and_then(|n| n.image.clone());
            let (w, h) = image.as_ref().map(|i| (i.w, i.h)).unwrap_or((1.0, 1.0));
            self.drag = Some(Drag::Resize {
                id,
                start: (w, h),
                grab: (wx, wy),
                aspect: image.as_ref().map_or(1.0, |i| i.aspect()),
                moved: false,
                depth: self.history.depth(),
            });
            cx.notify();
            return;
        }

        // The fold bubble sits outside the ring, so it has to be tested before
        // the node — otherwise it is only reachable where the two overlap.
        if let Some(id) = self.hit_fold_bubble(sx, sy) {
            self.graph.leave_edit();
            // Press-and-release folds; press-and-drag grows a child where you let
            // go. Deciding on release keeps the fold click as fast as it was.
            self.drag = Some(Drag::Grow {
                from: id,
                origin: (wx, wy),
                to: (wx, wy),
                moved: false,
            });
            cx.notify();
            return;
        }

        // Nodes take priority over edges, edges over empty space.
        if let Some(id) = self.hit_node(wx, wy) {
            let origin = {
                let n = &self.graph.nodes[&id];
                (n.x, n.y)
            };
            // ⇧ adds and removes; ⌘ takes the whole branch. Both are about the
            // set of nodes, so neither may fall through into a drag — a press
            // that changes the selection and *also* moves something is how you
            // nudge a node by accident every time you extend a selection.
            if ev.modifiers.shift {
                self.graph.leave_edit();
                self.graph.toggle_node_selected(id);
                self.history.seal();
                self.say_selection();
                cx.notify();
                return;
            }
            if ev.modifiers.platform {
                self.graph.leave_edit();
                self.graph.select_subtree_of(id);
                self.history.seal();
                self.say_selection();
                cx.notify();
                return;
            }
            // A click on the one node you are already on reaches past the ring
            // for the text, opening Edit with the caret where you aimed. Browse
            // stays the resting state, so selecting never silently changes what
            // the arrows mean.
            // "Already on it" means the cursor is here and this node is the
            // whole selection — not that the selection is empty, which it never
            // is right after a click.
            // A picture has no label, and `paint_nodes` draws neither caret nor
            // text for one — so entering Edit on it would point the keyboard at
            // an invisible string, where ⌫ eats characters nobody can see
            // instead of deleting the node. `place_images` already refuses to
            // leave a fresh import in Edit; this is the same rule on the click
            // path, which is how the very first click after a paste got in.
            let has_image = self
                .graph
                .nodes
                .get(&id)
                .is_some_and(|n| n.image.is_some());
            let on_it = !has_image
                && id == self.graph.focused_id
                && self.graph.focused_edge.is_none()
                && self.graph.acting_on() == [id];
            if on_it {
                if !self.graph.is_editing() {
                    self.graph.enter_edit();
                }
                self.place_caret_at(sx, sy, window);
                self.widen_to_click(ev.click_count);
                self.blink_t = 0.0;
                self.cursor_on = true;
                self.history.seal();
                // Dragging on from here sweeps the text, not the node. Moving
                // it means leaving Edit first, which Esc or a click away does.
                self.drag = Some(Drag::Text);
                cx.notify();
                return;
            }
            self.graph.leave_edit();
            if !self.graph.is_node_selected(id) {
                self.graph.focus(id);
            }
            self.history.seal();
            self.drag = Some(Drag::Node {
                id,
                grab: (wx - origin.0, wy - origin.1),
                origin,
                ids: self.graph.acting_on(),
                moved: false,
                depth: 0,
                onto: None,
                detach: false,
            });
            self.cursor_on = true;
            self.label_status();
            cx.notify();
            return;
        }
        if let Some(eid) = self.hit_edge(wx, wy) {
            if self.graph.focused_edge == Some(eid) {
                if !self.graph.is_editing() {
                    self.graph.enter_edit();
                }
                self.place_caret_at(sx, sy, window);
                self.widen_to_click(ev.click_count);
                self.blink_t = 0.0;
                self.drag = Some(Drag::Text);
            } else {
                self.graph.leave_edit();
                self.graph.focus_edge(eid);
            }
            self.history.seal();
            self.cursor_on = true;
            self.label_status();
            cx.notify();
            return;
        }
        // Empty ground. Pressing here lets go of everything — that is what
        // clicking away means on any canvas — and the drag that may follow
        // rubber-bands a new selection. ⇧ keeps what was already held.
        self.graph.leave_edit();
        self.graph.clear_edge_focus_only();
        let base = if ev.modifiers.shift {
            self.graph.acting_on()
        } else {
            self.graph.deselect_all();
            Vec::new()
        };
        if ev.click_count >= 2 {
            // Double-click on empty ground makes a node there. Without it the
            // pointer cannot create anything at all — every other way in is a
            // keyboard shortcut.
            self.drag = None;
            self.new_node_at(wx, wy);
            cx.notify();
            return;
        }
        self.drag = Some(Drag::Marquee {
            origin: (wx, wy),
            current: (wx, wy),
            base,
            focus_before: self.graph.focused_id,
        });
        cx.notify();
    }

    // ---- find ------------------------------------------------------------

    fn on_open_find(&mut self, _: &OpenFind, _: &mut Window, cx: &mut Context<Self>) {
        self.open_find(cx);
    }

    fn open_find(&mut self, cx: &mut Context<Self>) {
        // Searching behind an opaque overlay you cannot type into is not a
        // thing anyone asked for.
        self.show_help = false;
        self.graph.leave_edit();
        self.close_menu();
        // Reopening keeps the last query, so ⌘F ⌘F is "search again" — and so
        // is ⌘G after Esc.
        let query = self
            .find
            .as_ref()
            .map(|f| f.query.clone())
            .unwrap_or_else(|| self.last_query.clone());
        let matches = self.graph.find(&query);
        self.find = Some(Find {
            query,
            matches,
            at: 0,
            replace: String::new(),
            replacing: false,
        });
        self.say("find · type to search · ⇥ replace · ↩ next · Esc done");
        cx.notify();
    }

    fn close_find(&mut self) -> bool {
        match self.find.take() {
            Some(f) => {
                self.last_query = f.query;
                true
            }
            None => false,
        }
    }

    /// Recompute matches after the query changed, and go to the first one so
    /// the map moves as you type rather than only when you press ↩.
    fn refresh_find(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        f.matches = self.graph.find(&f.query);
        f.at = 0;
        if f.matches.is_empty() {
            let q = f.query.clone();
            if !q.is_empty() {
                self.say(format!("no node matches \"{q}\""));
            }
        } else {
            self.go_to_match();
        }
        cx.notify();
    }

    fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_mut() else { return };
        if f.matches.is_empty() {
            cx.notify();
            return;
        }
        let n = f.matches.len() as isize;
        f.at = (((f.at as isize + delta) % n + n) % n) as usize;
        self.go_to_match();
        cx.notify();
    }

    /// Select the current match and bring it on screen — unfolding whatever was
    /// hiding it, because "found it" has to mean you can see it.
    fn go_to_match(&mut self) {
        let Some(f) = self.find.as_ref() else { return };
        let Some(&id) = f.matches.get(f.at) else { return };
        let (at, total) = (f.at + 1, f.matches.len());
        if self.graph.reveal(id) {
            self.touch();
        }
        self.graph.focus(id);
        let n = self.graph.focused();
        let (x, y) = (n.x, n.y);
        self.camera.fly_to((x, y), self.camera.zoom().max(0.6));
        self.say(format!("{at} of {total}"));
    }

    /// ⌘G steps on even with the field closed, so a search survives dismissing
    /// it — that is the whole point of the chord.
    fn on_find_next(&mut self, _: &FindNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.open_find(cx);
            return;
        }
        self.find_step(1, cx);
    }

    fn on_find_prev(&mut self, _: &FindPrev, _: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            self.open_find(cx);
            return;
        }
        self.find_step(-1, cx);
    }

    fn on_find_backspace(&mut self, _: &FindBackspace, _: &mut Window, cx: &mut Context<Self>) {
        let mut trimmed_query = false;
        if let Some(f) = self.find.as_mut() {
            if f.replacing {
                f.replace.pop();
            } else {
                f.query.pop();
                trimmed_query = true;
            }
        }
        if trimmed_query {
            self.refresh_find(cx);
        } else {
            cx.notify();
        }
    }

    /// ⇥ inside the find bar swaps the keyboard between the query and the
    /// replacement field.
    fn on_find_toggle_replace(&mut self, _: &FindToggleReplace, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.find.as_mut() {
            f.replacing = !f.replacing;
            self.cursor_on = true;
            self.blink_t = 0.0;
            cx.notify();
        }
    }

    /// ⌘↩ inside the find bar swaps the query for the replacement across every
    /// matching label, in one undo step.
    fn on_find_replace_all(&mut self, _: &FindReplaceAll, _: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.find.as_ref() else { return };
        let (query, replace) = (f.query.clone(), f.replace.clone());
        if query.is_empty() {
            self.say("nothing to replace");
            cx.notify();
            return;
        }
        // Replace on a copy first: only a real change earns the undo step, so a
        // query that trims to no match (or a replace-with-itself) never wipes the
        // redo stack.
        let mut next = self.graph.clone();
        let n = next.replace_all(&query, &replace);
        if n > 0 {
            self.edit(Tag::Structural);
            self.graph = next;
            self.refresh_find(cx);
            self.say(format!("replaced in {n} label{} · ⌘Z to undo", if n == 1 { "" } else { "s" }));
        } else {
            self.say("no matches to replace");
        }
        cx.notify();
    }

    // ---- context menu ----------------------------------------------------

    /// Open the menu for whatever was right-clicked. What is under the pointer
    /// decides the verbs, so the menu is also the app explaining itself — the
    /// first thing anyone tries after a click does not do what they wanted.
    fn open_context_menu(&mut self, sx: f32, sy: f32, cx: &mut Context<Self>) {
        self.sync_viewport();
        let (wx, wy) = self.camera.to_world(sx, sy);
        let rows = if let Some(id) = self.hit_node(wx, wy) {
            // Right-clicking outside the selection picks that node first, the
            // way every list does — acting on something you cannot see selected
            // is how you delete the wrong branch.
            if !self.graph.is_node_selected(id) {
                self.graph.leave_edit();
                self.graph.focus(id);
            }
            let many = self.graph.acting_on().len() > 1;
            let mut rows = vec![
                MenuRow::Item("new child", "⇥", MenuCmd::NewChild),
                MenuRow::Item("new sibling", "⇧⇥", MenuCmd::NewSibling),
                MenuRow::Rule,
            ];
            if !many {
                rows.push(MenuRow::Item("edit label", "↩", MenuCmd::EditLabel));
                rows.push(MenuRow::Item("select branch", "⌘ click", MenuCmd::SelectBranch));
                if !self.graph.children_of(id).is_empty() {
                    rows.push(MenuRow::Item(
                        if self.graph.is_collapsed(id) { "Unfold" } else { "Fold branch" },
                        "⌘.",
                        MenuCmd::Fold,
                    ));
                }
                rows.push(MenuRow::Rule);
            }
            rows.extend([
                MenuRow::Item("duplicate", "⌘D", MenuCmd::Duplicate),
                MenuRow::Item("copy", "⌘C", MenuCmd::Copy),
                MenuRow::Item("cut", "⌘X", MenuCmd::Cut),
                MenuRow::Rule,
                MenuRow::Item(if many { "Delete selection" } else { "Delete" }, "⌫", MenuCmd::Delete),
            ]);
            rows
        } else if let Some(eid) = self.hit_edge(wx, wy) {
            self.graph.leave_edit();
            self.graph.focus_edge(eid);
            vec![MenuRow::Item("edit label", "⌘L", MenuCmd::EditLabel)]
        } else {
            self.graph.leave_edit();
            self.graph.deselect_all();
            vec![
                MenuRow::Item("new node here", "double-click", MenuCmd::NewNodeHere),
                MenuRow::Item("paste", "⌘V", MenuCmd::Paste),
                MenuRow::Rule,
                MenuRow::Item("select all", "⌘A", MenuCmd::SelectAll),
                MenuRow::Item("tidy layout", "⌘R", MenuCmd::Tidy),
                MenuRow::Item("fit map", "⌘0", MenuCmd::Fit),
            ]
        };
        // Keep it inside the window: a menu whose rows fall off the bottom is
        // one you cannot reach the important half of.
        let h = rows
            .iter()
            .map(|r| match r {
                MenuRow::Item(..) => MENU_ROW_H,
                MenuRow::Rule => MENU_RULE_H,
            })
            .sum::<f32>()
            + 8.0;
        let (mut x, mut y) = (sx, sy);
        if let Some(b) = self.last_bounds {
            let (bx, by) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (bw, bh) = (f32::from(b.size.width), f32::from(b.size.height));
            x = x.min(bx + bw - MENU_W - 8.0).max(bx + 4.0);
            y = y.min(by + bh - h - 8.0).max(by + 4.0);
        }
        self.drag = None;
        self.menu = Some(ContextMenu {
            at: (x, y),
            world: (wx, wy),
            rows,
        });
        cx.notify();
    }

    fn close_menu(&mut self) -> bool {
        self.menu.take().is_some()
    }

    /// Run a row. Every one of these is reachable another way; the menu is a
    /// second door onto the same handlers, not a second implementation.
    fn run_menu(&mut self, cmd: MenuCmd, window: &mut Window, cx: &mut Context<Self>) {
        let world = self.menu.as_ref().map(|m| m.world).unwrap_or((0.0, 0.0));
        self.close_menu();
        match cmd {
            MenuCmd::NewChild => self.on_create_child(&CreateChild, window, cx),
            MenuCmd::NewSibling => self.on_create_sibling(&CreateSibling, window, cx),
            MenuCmd::NewNodeHere => {
                self.new_node_at(world.0, world.1);
                cx.notify();
            }
            MenuCmd::EditLabel => {
                // Not `on_commit` — that is a toggle, and on a node already
                // being edited it would close the label the row offers to open.
                self.graph.enter_edit();
                self.cursor_on = true;
                self.blink_t = 0.0;
                self.label_status();
                cx.notify();
            }
            MenuCmd::SelectBranch => {
                let id = self.graph.focused_id;
                self.graph.leave_edit();
                self.graph.select_subtree_of(id);
                self.history.seal();
                self.say_selection();
                cx.notify();
            }
            MenuCmd::Fold => self.on_toggle_fold(&ToggleFold, window, cx),
            MenuCmd::Duplicate => self.on_duplicate_nodes(&DuplicateNodes, window, cx),
            MenuCmd::Copy => self.on_copy_nodes(&CopyNodes, window, cx),
            MenuCmd::Cut => self.on_cut_nodes(&CutNodes, window, cx),
            MenuCmd::Paste => self.on_paste_nodes(&PasteNodes, window, cx),
            MenuCmd::Delete => self.on_delete(&DeleteNode, window, cx),
            MenuCmd::SelectAll => self.on_select_all_nodes(&SelectAllNodes, window, cx),
            MenuCmd::Tidy => self.on_tidy(&TidyLayout, window, cx),
            MenuCmd::Fit => self.on_zoom_fit(&ZoomFit, window, cx),
        }
    }

    /// Say how big the selection is, since a count is the one thing the rings
    /// cannot show at a glance.
    fn say_selection(&mut self) {
        let n = self.graph.selected_nodes().len();
        if n > 1 {
            self.say(format!("{n} selected · ⌫ deletes · drag moves"));
        } else {
            self.label_status();
        }
    }

    /// Track a resize. The snapshot is pushed once, on the first motion past
    /// the slop, so a click that grazes the grip records nothing.
    fn resize_drag_to(&mut self, wx: f32, wy: f32) {
        let z = self.camera.zoom();
        let Some(Drag::Resize {
            id,
            start,
            grab,
            aspect,
            moved,
            ..
        }) = self.drag
        else {
            return;
        };
        let travelled = ((wx - grab.0).powi(2) + (wy - grab.1).powi(2)).sqrt() * z;
        if !moved && travelled <= DRAG_SLOP {
            return;
        }
        if !moved {
            // Keyed on the node so it cannot coalesce with a move of anything
            // else. It does not coalesce with a second resize of the *same*
            // picture either, because both release paths seal unconditionally —
            // which is what we want: one gesture, one undo entry.
            self.history.record(&self.graph, Tag::Move(id));
            if let Some(Drag::Resize {
                moved: m, depth: d, ..
            }) = &mut self.drag
            {
                *m = true;
                *d = self.history.depth();
            }
        }
        // The grip sits at the bottom-right corner, so the box follows the
        // pointer outward from the node's centre. `grab` is subtracted from the
        // corner it was pressed on: the grip's reach is a dozen-odd pixels, so
        // a press anywhere but dead centre would otherwise snap the box by that
        // much the instant the drag started. `Drag::Node` is careful about the
        // same thing. Height follows the width, so a picture can never be
        // squashed out of proportion.
        let node_x = self.graph.nodes.get(&id).map(|n| n.x).unwrap_or(wx);
        let offset = grab.0 - (node_x + start.0 * 0.5);
        // No `abs()`: pulling the grip left past the node's centre must bottom
        // out at the floor, not mirror and start growing again.
        let half_w = wx - offset - node_x;
        // Bounded on the *longer* edge. Clamping the width alone lets a tall
        // screenshot — aspect 0.1 — reach forty thousand units of height at the
        // width ceiling, which is the very thing the ceiling is there to stop.
        let aspect = aspect.clamp(0.02, 50.0);
        let want = (half_w * 2.0).max(MIN_IMAGE_W);
        let longest = want.max(want / aspect);
        let scale = (MAX_IMAGE_W / longest).min(1.0);
        let w = (want * scale).max(MIN_IMAGE_W);
        let h = w / aspect;
        if let Some(node) = self.graph.nodes.get_mut(&id) {
            if let Some(name) = node.image.as_ref().map(|i| i.name.clone()) {
                node.image = Some(std::sync::Arc::new(crate::graph::NodeImage {
                    name,
                    w,
                    h,
                }));
            }
        }
    }

    /// Track the pointer for a grow-from-ring gesture and flip it to "moved"
    /// once it clears the slop, so the preview line shows and the release grows
    /// a child instead of folding.
    fn grow_drag_to(&mut self, wx: f32, wy: f32) {
        let z = self.camera.zoom();
        let mut moved_now = false;
        if let Some(Drag::Grow { origin, to, moved, .. }) = &mut self.drag {
            *to = (wx, wy);
            if ((wx - origin.0).powi(2) + (wy - origin.1).powi(2)).sqrt() * z > DRAG_SLOP {
                *moved = true;
            }
            moved_now = *moved;
        }
        if moved_now {
            self.say("release to grow a child here");
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_viewport();
        let (sx, sy) = (f32::from(ev.position.x), f32::from(ev.position.y));
        self.pointer = (sx, sy);
        let (wx, wy) = self.camera.to_world(sx, sy);

        // Sweeping text and rubber-banding both need `&mut self` helpers, so
        // they run before the `match` takes a borrow on the drag state.
        if matches!(self.drag, Some(Drag::Text)) {
            if self.extend_text_selection(sx, sy, window) {
                self.cursor_on = true;
                self.blink_t = 0.0;
            }
            cx.notify();
            return;
        }
        if matches!(self.drag, Some(Drag::Marquee { .. })) {
            self.drag_marquee_to(wx, wy);
            cx.notify();
            return;
        }

        // Modifiers are remembered so the tick loop can keep a drag going,
        // with the same axis lock and snapping, while the pointer sits still
        // against the edge of the window.
        self.drag_mods = (ev.modifiers.shift, ev.modifiers.alt);

        match self.drag {
            Some(Drag::Node { .. }) => {
                if self.drag_node_to(wx, wy) {
                    cx.notify();
                }
            }
            Some(Drag::Grow { .. }) => {
                self.grow_drag_to(wx, wy);
                cx.notify();
            }
            Some(Drag::Resize { .. }) => {
                self.resize_drag_to(wx, wy);
                cx.notify();
            }
            Some(Drag::Marquee { .. }) | Some(Drag::Text) => {
                // Handled above, before the drag state is borrowed.
            }
            Some(Drag::Pan {
                ref mut last,
                ref mut last_at,
                ref mut vel,
                ref mut moved,
            }) => {
                let (dx, dy) = (sx - last.0, sy - last.1);
                if !*moved && (dx * dx + dy * dy).sqrt() < DRAG_SLOP {
                    return;
                }
                *moved = true;
                let dt = last_at.elapsed().as_secs_f32().max(1e-3);
                // Smooth the instantaneous speed so one jittery sample can't
                // launch the camera on release.
                vel.0 = vel.0 * 0.7 + (dx / dt) * 0.3;
                vel.1 = vel.1 * 0.7 + (dy / dt) * 0.3;
                *last = (sx, sy);
                *last_at = Instant::now();
                self.camera.pan_by_screen(dx, dy);
                cx.notify();
            }
            None => {
                let before = self.hover;
                self.refresh_hover(sx, sy);
                if self.hover != before {
                    cx.notify();
                }
            }
        }
    }

    /// Put the dragged selection where world point `(wx, wy)` says. Returns
    /// whether anything changed. Shared by mouse-move and by the tick loop's
    /// edge pan, so a drag held against the window edge keeps behaving exactly
    /// as it does in the middle of it — same slop, same axis lock, same snap.
    fn drag_node_to(&mut self, wx: f32, wy: f32) -> bool {
        let Some(Drag::Node { id, grab, origin, ref ids, moved, .. }) = self.drag else {
            return false;
        };
        let ids = ids.clone();
        let (shift, alt) = self.drag_mods;
        let mut target = (wx - grab.0, wy - grab.1);
        // Slop is measured against the raw pointer. Snapping can shift the
        // landing spot by half a grid cell on the very first event, which
        // would turn an ⌥-click into an unintended move.
        let travelled =
            ((target.0 - origin.0).powi(2) + (target.1 - origin.1).powi(2)).sqrt()
                * self.camera.zoom();
        if !moved && travelled < DRAG_SLOP {
            return false;
        }
        if shift {
            // Axis lock to whichever way the drag has gone furthest.
            // Unambiguous: ⇧ only picks nodes at press time, and this drag has
            // already started.
            if (target.0 - origin.0).abs() >= (target.1 - origin.1).abs() {
                target.1 = origin.1;
            } else {
                target.0 = origin.0;
            }
        }
        if alt {
            target = (Graph::snap(target.0), Graph::snap(target.1));
        }
        if !moved {
            if let Some(Drag::Node { moved, .. }) = &mut self.drag {
                *moved = true;
            }
            // Snapshot the pre-drag layout exactly once per gesture.
            self.history.record(&self.graph, self.move_tag());
            let depth = self.history.depth();
            if let Some(Drag::Node { depth: d, .. }) = &mut self.drag {
                *d = depth;
            }
        }
        // The grabbed node goes exactly where the pointer says; the rest of
        // the selection follows by the same delta.
        let cur = self
            .graph
            .nodes
            .get(&id)
            .map(|n| (n.x, n.y))
            .unwrap_or(origin);
        self.graph
            .move_nodes_by(&ids, target.0 - cur.0, target.1 - cur.1);
        // What is under the pointer must not lag it. Anything else the move
        // displaced can still ease.
        for id in &ids {
            self.anim.remove(id);
        }

        // Dropping onto another node re-hangs the branch there. The candidate
        // is whatever ring the pointer is inside that is a legal parent for
        // every root being carried — legality is checked here, while it can
        // still be shown, rather than only on release.
        let roots = self.graph.reparent_roots(&ids);
        // Skip everything being carried — including the descendants that came
        // along, which are also sitting under the pointer.
        let mut carried: std::collections::HashSet<u64> = ids.iter().copied().collect();
        for &r in &roots {
            carried.extend(self.graph.subtree_ids(r));
        }
        let (shaped, z, hidden) = (&self.shaped, self.camera.zoom(), &self.frame_hidden);
        let onto = self
            .graph
            .hit_test_masked(wx, wy, &carried, hidden, |id, _| half_of(shaped, z, id))
            .filter(|&t| roots.iter().all(|&r| self.graph.can_reparent(r, t)));
        // With no re-hang target, a grabbed node pulled far from its parent is
        // being torn off. Judge it on the grabbed node alone — predictable, and
        // it is the one the pointer is on.
        let detach = onto.is_none()
            && self
                .graph
                .nodes
                .get(&id)
                .and_then(|n| n.parent.map(|p| (n.x, n.y, p)))
                .and_then(|(nx, ny, pid)| self.graph.nodes.get(&pid).map(|p| (nx, ny, p.x, p.y)))
                .is_some_and(|(nx, ny, px, py)| {
                    ((nx - px).powi(2) + (ny - py).powi(2)).sqrt() > DISCONNECT_DIST
                });
        if let Some(Drag::Node { onto: slot, detach: d, .. }) = &mut self.drag {
            *slot = onto;
            *d = detach;
        }
        self.touch();
        let n = ids.len();
        if detach {
            self.say("release to disconnect");
        } else if let Some(t) = onto {
            let name = self
                .graph
                .nodes
                .get(&t)
                .map(|node| {
                    if node.text.is_empty() {
                        "that node".to_string()
                    } else {
                        format!("\"{}\"", node.text)
                    }
                })
                .unwrap_or_default();
            self.say(format!("drop to hang under {name}"));
        } else if n > 1 {
            self.say(format!("moving {n} nodes"));
        } else {
            self.say(format!("{:.0}, {:.0}", target.0, target.1));
        }
        true
    }

    /// Extend the rubber band to `(wx, wy)` and reselect what it covers.
    fn drag_marquee_to(&mut self, wx: f32, wy: f32) {
        let Some(Drag::Marquee { origin, base, focus_before, .. }) = &self.drag else {
            return;
        };
        let (origin, base, focus_before) = (*origin, base.clone(), *focus_before);
        if let Some(Drag::Marquee { current, .. }) = &mut self.drag {
            *current = (wx, wy);
        }
        // Split borrows: the closure takes `shaped` and the zoom directly, not
        // `&self`, which the drag state is already holding.
        let (shaped, z, hidden) = (&self.shaped, self.camera.zoom(), &self.frame_hidden);
        let hits = self.graph.nodes_in_rect_masked(origin.0, origin.1, wx, wy, hidden, |id, _| {
            half_of(shaped, z, id)
        });
        self.graph.set_node_selection(base.into_iter().chain(hits));
        if self.graph.acting_on().is_empty() {
            self.graph.set_cursor(focus_before);
        }
        self.say_selection();
    }

    /// Which node's fold bubble the screen point `(sx, sy)` is inside, if any.
    /// Measured against exactly the geometry `paint_nodes` uses.
    /// The resize grip under the pointer, in screen coordinates.
    ///
    /// Screen space, and tested before the node itself, exactly as the fold
    /// bubble is: the grip sits on the picture's own corner, so a hit test that
    /// ran after the node would never see it.
    fn hit_resize_grip(&self, sx: f32, sy: f32) -> Option<u64> {
        let z = self.camera.zoom();
        let mut best: Option<(u64, f32)> = None;
        for (&id, n) in &self.graph.nodes {
            if n.image.is_none() || self.frame_hidden.contains(&id) {
                continue;
            }
            // Only where a grip is actually drawn.
            // Exactly the condition `paint_nodes` draws it under. Testing
            // `focused_id` without the edge check left an invisible hot zone on
            // the last-focused picture while an edge label had the keyboard.
            let shown = matches!(self.hover, Some(Hover::Node(h)) if h == id)
                || self.graph.selected_nodes().contains(&id)
                || (self.graph.focused_id == id && self.graph.focused_edge.is_none());
            if !shown {
                continue;
            }
            let (hw, hh) = self.half_world(id);
            let (wx, wy) = self.draw_pos(id, n);
            let (cx, cy) = self.camera.to_screen(wx, wy);
            let (gx, gy, r) = crate::paint::resize_grip(cx, cy, hw * z, hh * z, z);
            let d = ((sx - gx).powi(2) + (sy - gy).powi(2)).sqrt();
            let reach = (r + 4.0).max(9.0);
            if d <= reach && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((id, d));
            }
        }
        best.map(|(id, _)| id)
    }

    fn hit_fold_bubble(&self, sx: f32, sy: f32) -> Option<u64> {
        let z = self.camera.zoom();
        let hidden = self.graph.hidden_set();
        // Nearest bubble wins, not whichever the hash yields first — two that
        // overlap must not resolve differently from one frame to the next.
        let mut best: Option<(u64, f32)> = None;
        let child_counts = self.graph.child_counts();
        for (&id, n) in &self.graph.nodes {
            if hidden.contains(&id) || !child_counts.contains_key(&id) {
                continue;
            }
            // `paint_nodes` returns from the image branch before it draws a fold
            // bubble, so a picture with children had one that was invisible and
            // still took the click. Fold it from the keyboard instead.
            if n.image.is_some() {
                continue;
            }
            let font = self.base_font(id) * z;
            if font < MIN_READABLE_PX {
                continue;
            }
            let (wx, wy) = self.draw_pos(id, n);
            let (cx, cy) = self.camera.to_screen(wx, wy);
            let hw = self.shaped.get(&id).map(|m| m.half(z).0).unwrap_or(0.0);
            let (bx, by, r) = crate::paint::fold_bubble(cx, cy, hw, font);
            // A generous target: the bubble is small on purpose, and a control
            // you have to aim at is one people stop using.
            let reach = (r + 4.0).max(9.0);
            let d2 = (sx - bx).powi(2) + (sy - by).powi(2);
            if d2 <= reach * reach && best.is_none_or(|(_, b)| d2 < b) {
                best = Some((id, d2));
            }
        }
        best.map(|(id, _)| id)
    }

    /// Recompute what the pointer is over. Hover is otherwise only tracked
    /// while no drag is in flight, so it needs refreshing when one ends.
    fn refresh_hover(&mut self, sx: f32, sy: f32) {
        let (wx, wy) = self.camera.to_world(sx, sy);
        self.hover = self
            .hit_node(wx, wy)
            .map(Hover::Node)
            .or_else(|| self.hit_edge(wx, wy).map(Hover::Edge));
    }

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (sx, sy) = (f32::from(ev.position.x), f32::from(ev.position.y));
        match self.drag.take() {
            Some(Drag::Pan {
                vel,
                moved,
                last_at,
                ..
            }) => {
                // Only a *flick* coasts. `vel` is sampled in mouse-move, so a
                // pointer that stopped before release still holds the speed it
                // had while moving — releasing on that would fling the camera.
                let stale = last_at.elapsed() > FLICK_WINDOW;
                if moved && !stale {
                    // Screen px/sec → world units/sec, so a flick keeps flying.
                    let z = self.camera.zoom();
                    self.camera.set_velocity(-vel.0 / z, -vel.1 / z);
                }
                if moved {
                    self.touch();
                }
            }
            // A resize that never moved past the slop pushed no snapshot and
            // changed nothing, so there is nothing to seal or take back.
            Some(Drag::Resize { moved, .. }) => {
                if moved {
                    self.history.seal();
                    self.touch();
                }
            }
            Some(Drag::Node { moved, onto, detach, ref ids, .. }) => {
                let ids = ids.clone();
                if let Some(target) = onto.filter(|_| moved) {
                    // The pre-drag snapshot is already on the stack from the
                    // first motion event, so the re-hang undoes with the move
                    // that carried it there — one gesture, one ⌘Z.
                    self.capture_positions();
                    let roots = self.graph.reparent_roots(&ids);
                    let mut n = 0;
                    for r in roots {
                        if self.graph.reparent(r, target) {
                            self.graph.settle_after_drop(r);
                            n += 1;
                        }
                    }
                    if n > 0 {
                        self.say(format!(
                            "re-hung {n} branch{} · ⌘Z to undo",
                            if n == 1 { "" } else { "es" }
                        ));
                    }
                } else if moved && detach {
                    // Pulled clear of its parent with nowhere to land: cut it
                    // free. Same snapshot as the move, so one ⌘Z takes it back.
                    self.capture_positions();
                    let roots = self.graph.reparent_roots(&ids);
                    let mut n = 0;
                    for r in roots {
                        if self.graph.detach(r) {
                            n += 1;
                        }
                    }
                    if n > 0 {
                        self.say(format!(
                            "disconnected {n} branch{} · ⌘Z to undo",
                            if n == 1 { "" } else { "es" }
                        ));
                    }
                } else if moved {
                    self.label_status();
                }
                if moved {
                    self.history.seal();
                    self.touch();
                }
                // Hover is only tracked when no drag is in flight, so it is
                // whatever it was when the drag began — stale by definition.
                self.refresh_hover(sx, sy);
            }
            Some(Drag::Marquee { .. }) => {
                self.history.seal();
                self.say_selection();
                self.refresh_hover(sx, sy);
            }
            Some(Drag::Text) => {
                self.history.seal();
            }
            Some(Drag::Grow { from, to, moved, .. }) => {
                if moved {
                    // Dragged off the ring: a child lands where you let go, ready
                    // to name.
                    let mut next = self.graph.clone();
                    if let Some(id) = next.create_child_at(from, to.0, to.1, "") {
                        self.edit(Tag::Structural);
                        self.graph = next;
                        self.graph.focus(id);
                        self.cursor_on = true;
                        self.blink_t = 0.0;
                        self.follow_focus();
                        self.say("new child · type to name it");
                    } else {
                        self.say("out of node ids");
                    }
                } else {
                    // A plain click on the ring still just folds.
                    self.toggle_fold_of(from);
                }
                self.refresh_hover(sx, sy);
            }
            None => return,
        }
        cx.notify();
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.sync_viewport();
        let delta = ev.delta.pixel_delta(px(24.0));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        let (sx, sy) = (f32::from(ev.position.x), f32::from(ev.position.y));

        // ⌘ + wheel zooms about the pointer; a bare wheel flies the canvas.
        // Not ⌃+wheel: that is macOS's system-wide accessibility zoom, and
        // answering it here means both zooms fire at once.
        // A trackpad pinch arrives separately, via `pinch` — gpui's mac layer
        // never translates NSEventTypeMagnify into a scroll event.
        if ev.modifiers.platform {
            // Clamped per event: a notched mouse wheel reports a whole line at
            // once and would otherwise double the zoom in a single click. Eased
            // rather than snapped, so a wheel notch glides to its new scale
            // while the point under the cursor holds still.
            let factor = (dy * 0.01).exp().clamp(0.8, 1.25);
            self.camera.zoom_toward(sx, sy, factor);
        } else {
            self.camera.pan_by_screen(dx, dy);
        }
        self.touch();
        cx.notify();
    }

    fn on_key_input(&mut self, ev: &Keystroke, cx: &mut Context<Self>) {
        // Command/control chords are shortcuts, never text — without this ⌘R
        // would tidy the map *and* type an "r".
        if ev.modifiers.platform || ev.modifiers.control || ev.modifiers.function {
            return;
        }
        let Some(ch) = ev.key_char.as_ref() else {
            return;
        };
        if ch.chars().count() != 1 {
            return;
        }
        let c = ch.chars().next().unwrap();
        if c.is_control() {
            return;
        }
        // The shortcut panel searches itself: typing filters its rows.
        if self.show_help {
            self.help_query.push(c);
            self.cursor_on = true;
            self.blink_t = 0.0;
            cx.notify();
            return;
        }
        // While find is open, typing builds the query — or the replacement text
        // when ⇥ has switched to that field.
        if self.find.is_some() {
            let mut into_query = true;
            if let Some(f) = self.find.as_mut() {
                if f.replacing {
                    f.replace.push(c);
                    into_query = false;
                } else {
                    f.query.push(c);
                }
            }
            self.cursor_on = true;
            self.blink_t = 0.0;
            if into_query {
                self.refresh_find(cx);
            } else {
                cx.notify();
            }
            return;
        }
        // No caret, no text change. Editing is entered deliberately — ↩ or a
        // click into the label — so a stray key on the map never rewrites a
        // node you only meant to have selected.
        if !self.graph.is_editing() {
            return;
        }
        self.edit(self.text_tag());
        self.graph.insert_char(c);
        self.cursor_on = true;
        self.blink_t = 0.0;
        self.label_status();
        cx.notify();
    }

}

impl Drop for MindMapView {
    fn drop(&mut self) {
        // Closing the window drops the view without quitting the app.
        self.save_on_quit();
    }
}

impl Focusable for MindMapView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MindMapView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Where the window is now, for the next launch. Read here rather than
        // on quit: by the time `Drop` runs the window may already be gone.
        let f = window.bounds();
        let rect = store::WindowRect {
            x: f.origin.x.into(),
            y: f.origin.y.into(),
            w: f.size.width.into(),
            h: f.size.height.into(),
        };
        let moved = self
            .window_rect
            .is_none_or(|w| (w.x, w.y, w.w, w.h) != (rect.x, rect.y, rect.w, rect.h));
        self.window_rect = Some(rect);
        // Moving or resizing the window *is* a change to the document, and
        // nothing else marks it dirty — without this the new frame is only ever
        // saved when something else happens to need saving, so a session spent
        // only resizing writes nothing at all.
        if moved {
            self.touch();
        }

        // The window carries the document's name, so the title bar, the window
        // menu and Mission Control all say which map this is. Only re-set when
        // it changes — set_window_title crosses to AppKit every call.
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Untitled".to_string());
        // A leading dot marks unsaved changes — the standard traffic-light dot
        // is hidden here, so the title carries the edited state instead. It
        // clears the moment the debounced autosave lands.
        let title = if self.dirty_since.is_some() {
            format!("• {name}")
        } else {
            name
        };
        if self.shown_title.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.shown_title = Some(title);
        }

        // The first frame is drawn against a placeholder viewport; as soon as
        // the canvas reports its real bounds, frame the map against those.
        if self.needs_fit && self.last_bounds.is_some() {
            self.needs_fit = false;
            self.sync_viewport();
            self.fit_now(window);
        }

        // Open the frame for the image bank, and let it make room if the last
        // one left too much decoded. Here — before anything asks it for a
        // picture — is *after* the previous frame was painted, and it never
        // touches anything the previous two frames drew: dropping an image
        // hands its slot back to a sprite atlas that recycles slots at once.
        self.bank.begin_frame(window);

        let th = self.theme;
        let frame = self.build_frame(window, cx);
        let focus = self.focus_handle.clone();
        // Fade out over the last second rather than blinking away.
        let toast = (self.status_ttl > 0.0)
            .then(|| (self.status.clone(), self.status_ttl.min(1.0)));
        let debug = crate::perf::readout();
        // A failed write outranks a read-only session: it is the one that
        // means work already done is not on disk.
        let warning = self
            .save_error
            .clone()
            .or_else(|| self.read_only.clone());
        let show_help = self.show_help;
        let caret_on = self.cursor_on;
        let help_query = self.help_query.clone();
        let find = self.find.as_ref().map(|f| {
            let count = if f.query.trim().is_empty() {
                String::new()
            } else if f.matches.is_empty() {
                "no matches".to_string()
            } else {
                format!("{} of {}", f.at + 1, f.matches.len())
            };
            (f.query.clone(), count, f.replace.clone(), f.replacing)
        });
        // The menu is rebuilt from state each frame; only its geometry and the
        // command per row need to survive into the element tree.
        let menu = self.menu.as_ref().map(|m| {
            (
                m.at,
                m.rows
                    .iter()
                    .map(|r| match r {
                        MenuRow::Item(label, hint, cmd) => Some((*label, *hint, *cmd)),
                        MenuRow::Rule => None,
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let hovering = self.hover.is_some();
        let panning = matches!(self.drag, Some(Drag::Pan { .. }));
        // Over the text you are actually editing, the pointer is a text tool —
        // that is the one place a click means "caret here" rather than "focus".
        let over_text = self.graph.is_editing()
            && match self.hover {
                Some(Hover::Node(id)) => {
                    id == self.graph.focused_id && self.graph.focused_edge.is_none()
                }
                Some(Hover::Edge(eid)) => self.graph.focused_edge == Some(eid),
                None => false,
            };

        // The keymap reads this to decide what a key means; see `bind_keys`.
        let mut key_context = gpui::KeyContext::new_with_defaults();
        key_context.add("MindMap");
        key_context.set(
            "mode",
            if show_help {
                "help"
            } else if self.find.is_some() {
                "find"
            } else if self.graph.is_editing() {
                "edit"
            } else {
                "browse"
            },
        );

        div()
            .id("mind-map-root")
            // Paint through the titlebar in both palettes; the native window
            // supplies the only outer corner mask.
            .bg(c(th.bg))
            .key_context(key_context)
            .track_focus(&focus)
            .size_full()
            .flex()
            .flex_col()
            .text_color(c(th.ink))
            .font(font(CODE_FONT))
            .on_action(cx.listener(Self::on_escape))
            .on_action(cx.listener(Self::on_toggle_help))
            .on_action(cx.listener(Self::on_help_backspace))
            // Find is reachable from anywhere, including over the shortcut
            // panel — so its listeners live outside the modal guard, and
            // opening it takes the panel down rather than searching behind it.
            .on_action(cx.listener(Self::on_open_find))
            .on_action(cx.listener(Self::on_find_next))
            .on_action(cx.listener(Self::on_find_prev))
            // The shortcut panel is modal. Menu items dispatch straight into
            // these listeners without consulting a key context, so the only
            // way to make the overlay actually block the map is not to be
            // listening while it is up.
            .when(!show_help, |el| {
                el
                    .on_action(cx.listener(Self::on_create_right))
                    .on_action(cx.listener(Self::on_create_left))
                    .on_action(cx.listener(Self::on_create_up))
                            .on_action(cx.listener(Self::on_create_down))
                    .on_action(cx.listener(Self::on_create_child))
                    .on_action(cx.listener(Self::on_create_sibling))
                    .on_action(cx.listener(Self::on_toggle_fold))
                    .on_action(cx.listener(Self::on_find_backspace))
                    .on_action(cx.listener(Self::on_nav_left))
                    .on_action(cx.listener(Self::on_nav_right))
                    .on_action(cx.listener(Self::on_nav_up))
                    .on_action(cx.listener(Self::on_nav_down))
                    .on_action(cx.listener(Self::on_select_all_nodes))
                    .on_action(cx.listener(Self::on_focus_prev_sibling))
                    .on_action(cx.listener(Self::on_focus_next_sibling))
                    .on_action(cx.listener(Self::on_nudge_left))
                    .on_action(cx.listener(Self::on_nudge_right))
                    .on_action(cx.listener(Self::on_nudge_up))
                    .on_action(cx.listener(Self::on_nudge_down))
                    .on_action(cx.listener(Self::on_nudge_left_big))
                    .on_action(cx.listener(Self::on_nudge_right_big))
                    .on_action(cx.listener(Self::on_nudge_up_big))
                    .on_action(cx.listener(Self::on_nudge_down_big))
                    .on_action(cx.listener(Self::on_tidy))
                    .on_action(cx.listener(Self::on_delete))
                    .on_action(cx.listener(Self::on_undo))
                    .on_action(cx.listener(Self::on_redo))
                    .on_action(cx.listener(Self::on_zoom_in))
                    .on_action(cx.listener(Self::on_zoom_out))
                    .on_action(cx.listener(Self::on_zoom_fit))
                    .on_action(cx.listener(Self::on_zoom_reset))
                    .on_action(cx.listener(Self::on_toggle_theme))
                    .on_action(cx.listener(Self::on_save_now))
                    .on_action(cx.listener(Self::on_new_document))
                    .on_action(cx.listener(Self::on_save_as))
                    .on_action(cx.listener(Self::on_reveal_in_finder))
                    .on_action(cx.listener(Self::on_clear_recents))
                    .on_action(cx.listener(Self::on_export_markdown))
                    .on_action(cx.listener(Self::on_export_opml))
                    .on_action(cx.listener(Self::on_export_outline))
                    .on_action(cx.listener(Self::on_export_svg))
                    .on_action(cx.listener(Self::on_import_markdown))
                    .on_action(cx.listener(Self::on_import_opml))
                    .on_action(cx.listener(Self::on_toggle_minimap))
                    .on_action(cx.listener(Self::on_about))
                    .on_action(cx.listener(Self::on_hide_app))
                    .on_action(cx.listener(Self::on_hide_others))
                    .on_action(cx.listener(Self::on_show_all))
                    .on_action(cx.listener(Self::on_minimize_window))
                    .on_action(cx.listener(Self::on_zoom_window))
                    .on_action(cx.listener(Self::on_toggle_fullscreen))
                    .on_action(cx.listener(Self::on_close_window))
                    .on_action(cx.listener(Self::on_select_siblings))
                    .on_action(cx.listener(Self::on_move_sibling_up))
                    .on_action(cx.listener(Self::on_move_sibling_down))
                    .on_action(cx.listener(Self::on_outdent))
                    .on_action(cx.listener(Self::on_indent))
                    .on_action(cx.listener(Self::on_focus_root))
                    .on_action(cx.listener(Self::on_focus_last))
                    .on_action(cx.listener(Self::on_find_toggle_replace))
                    .on_action(cx.listener(Self::on_find_replace_all))
                    .on_action(cx.listener(Self::on_duplicate_document))
                    .on_action(cx.listener(Self::on_revert_document))
                    .on_action(cx.listener(Self::on_set_color0))
                    .on_action(cx.listener(Self::on_set_color1))
                    .on_action(cx.listener(Self::on_set_color2))
                    .on_action(cx.listener(Self::on_set_color3))
                    .on_action(cx.listener(Self::on_set_color4))
                    .on_action(cx.listener(Self::on_set_color5))
                    .on_action(cx.listener(Self::on_set_color6))
                    .on_action(cx.listener(Self::on_open_document))
                    .on_action(cx.listener(Self::on_open_recent))
                    .on_action(cx.listener(Self::on_menu_new_child))
                    .on_action(cx.listener(Self::on_menu_new_sibling))
                    .on_action(cx.listener(Self::on_menu_commit))
                    .on_action(cx.listener(Self::on_toggle_edge_label))
                            .on_action(cx.listener(Self::on_backspace))
                    .on_action(cx.listener(Self::on_delete_forward))
                    .on_action(cx.listener(Self::on_delete_word))
                    .on_action(cx.listener(Self::on_clear))
                    .on_action(cx.listener(Self::on_commit))
                    .on_action(cx.listener(Self::on_caret_left))
                    .on_action(cx.listener(Self::on_caret_right))
                    .on_action(cx.listener(Self::on_caret_word_left))
                    .on_action(cx.listener(Self::on_caret_word_right))
                    .on_action(cx.listener(Self::on_caret_home))
                    .on_action(cx.listener(Self::on_caret_end))
                    .on_action(cx.listener(Self::on_caret_up))
                    .on_action(cx.listener(Self::on_caret_down))
                    .on_action(cx.listener(Self::on_insert_newline))
                    .on_action(cx.listener(Self::on_select_up))
                    .on_action(cx.listener(Self::on_select_down))
                    .on_action(cx.listener(Self::on_select_left))
                    .on_action(cx.listener(Self::on_select_right))
                    .on_action(cx.listener(Self::on_select_word_left))
                    .on_action(cx.listener(Self::on_select_word_right))
                    .on_action(cx.listener(Self::on_select_home))
                    .on_action(cx.listener(Self::on_select_end))
                    .on_action(cx.listener(Self::on_select_all))
                    .on_action(cx.listener(Self::on_cut))
                    .on_action(cx.listener(Self::on_copy))
                    .on_action(cx.listener(Self::on_paste))
                    .on_action(cx.listener(Self::on_copy_nodes))
                    .on_action(cx.listener(Self::on_cut_nodes))
                    .on_action(cx.listener(Self::on_paste_nodes))
                    .on_action(cx.listener(Self::on_duplicate_nodes))
            })
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if this.capturing.is_some() {
                    this.capture_chord(&ev.keystroke, cx);
                } else {
                    this.on_key_input(&ev.keystroke, cx);
                }
            }))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            // The middle button pans, and gpui filters these listeners strictly
            // by button — without these two the pan never ends and the camera
            // follows the mouse for the rest of the session.
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            // The original compact titlebar: traffic-light hit area only, with
            // no independent surface, rule, decoration, or status capsule.
            .child(
                div()
                    .id("mind-map-titlebar")
                    .h(px(TITLEBAR_H))
                    .flex_shrink_0()
                    .w_full()
                    .on_hover(cx.listener(|this, over: &bool, _, cx| {
                        if this.titlebar_hover != *over {
                            this.titlebar_hover = *over;
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .id("mind-map-canvas")
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    // Images dropped on the canvas become nodes where they
                    // landed. Anything else is ignored: this app has no notion
                    // of attaching a file to a map.
                    .on_drop::<gpui::ExternalPaths>(cx.listener(
                        |this, paths: &gpui::ExternalPaths, window, cx| {
                            this.drop_files(paths, window, cx);
                        },
                    ))
                    // Registered separately: the handler's middle-button branch
                    // is unreachable unless the button is listened for.
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
                    .on_scroll_wheel(cx.listener(Self::on_scroll))
                    .child(
                        canvas(
                            {
                                let entity = cx.entity();
                                move |bounds, window, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.last_bounds = Some(bounds);
                                        this.camera.set_viewport(Viewport::new(
                                            bounds.origin.x.into(),
                                            bounds.origin.y.into(),
                                            bounds.size.width.into(),
                                            bounds.size.height.into(),
                                        ));
                                        // Now that the canvas has a size, the
                                        // pending fit can be resolved — ask for
                                        // the frame that will do it.
                                        if this.needs_fit {
                                            cx.notify();
                                        }
                                    });
                                    window.insert_hitbox(bounds, HitboxBehavior::Normal)
                                }
                            },
                            move |bounds, hitbox: Hitbox, window, cx| {
                                window.set_cursor_style(
                                    if panning {
                                        CursorStyle::ClosedHand
                                    } else if over_text {
                                        CursorStyle::IBeam
                                    } else if hovering {
                                        CursorStyle::PointingHand
                                    } else {
                                        // Empty ground rubber-bands now, so an
                                        // open hand would promise the wrong
                                        // gesture. Panning is the trackpad's.
                                        CursorStyle::Crosshair
                                    },
                                    &hitbox,
                                );
                                paint_map(&frame, bounds, window, cx);
                            },
                        )
                        .size_full(),
                    )
                    .children(toast.map(|(msg, alpha)| {
                        div()
                            .absolute()
                            .bottom_5()
                            .left_5()
                            .text_xs()
                            .text_color(ca(th.ink_soft, alpha))
                            .child(msg)
                    }))
                    .children(warning.map(|msg| {
                        // The one piece of standing chrome. It earns the space:
                        // everything else here is recoverable, and this is not.
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .px_5()
                            .py_1p5()
                            .text_xs()
                            .bg(ca(th.accent, 0.10))
                            .text_color(c(th.accent))
                            .child(msg)
                    }))
                    .children(debug.map(|d| {
                        div()
                            .absolute()
                            .top_5()
                            .right_5()
                            .text_xs()
                            .text_color(c(th.accent))
                            .child(SharedString::from(d))
                    }))
                    .children(find.map(|(query, count, replace, replacing)| {
                        // Top-centre, out of the way of the toast and the perf
                        // readout, and clearly a field rather than a message.
                        div()
                            .absolute()
                            .top_5()
                            .left_0()
                            .w_full()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .flex()
                                    .gap_3()
                                    .items_center()
                                    .px_3()
                                    .py_1p5()
                                    .min_w(px(260.))
                                    .bg(c(th.bg))
                                    .border_1()
                                    .border_color(ca(th.accent, 0.55))
                                    .rounded_md()
                                    .text_xs()
                                    .child(div().text_color(c(th.ink_quiet)).child("find"))
                                    .child(field_value(&query, "type to search", caret_on && !replacing, th))
                                    .child(div().text_color(c(th.ink_quiet)).child("→"))
                                    .child(field_value(&replace, "replace (⇥)", caret_on && replacing, th))
                                    .child(
                                        div()
                                            .text_color(ca(th.ink_soft, 1.0))
                                            .child(SharedString::from(count)),
                                    ),
                            )
                    }))
                    .children(menu.map(|m| context_menu(m, th, cx)))
                    .when(show_help, |el| {
                        let customise = self.build_customise(cx);
                        el.child(help_overlay(th, &help_query, caret_on, customise))
                    }),
            )
    }
}

// ---- chrome elements ----------------------------------------------------

const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "two modes",
        &[
            ("browse", "← → ↑ ↓ move the selection"),
            ("edit", "← → ↑ ↓ walk the caret"),
            ("↩", "browse → edit"),
            ("↩ or Esc", "edit → browse"),
            ("click what you are on", "edit, caret where you clicked"),
            ("drag in the text", "select it"),
        ],
    ),
    (
        "navigate (browse)",
        &[
            ("⌥← / ⌥→", "parent / a child"),
            ("⌥↑ / ⌥↓", "sibling up / down"),
            ("Esc", "cancel · leave · deselect · parent"),
        ],
    ),
    (
        "pick nodes (browse)",
        &[
            ("click empty ground", "let go of everything"),
            ("right-click / ⌃click", "a menu for what is under it"),
            ("drag empty ground", "rubber-band a selection"),
            ("⇧ drag", "add to the selection"),
            ("⇧ click a node", "add or remove one"),
            ("⌘ click a node", "add its whole branch"),
            ("⌘A", "select every node"),
            ("Esc", "deselect"),
        ],
    ),
    (
        "grow",
        &[
            ("⇥  or  ⌘↩", "new child"),
            ("⇧⇥  or  ⌘⇧↩", "new sibling"),
            ("⌘→ ⌘← ⌘↑ ⌘↓", "create a child that way"),
            ("double-click empty", "a new floating node there"),
            ("⌫ or ⌦", "delete the selection"),
            ("⌘⇧⌫", "the same, from either mode"),
            ("⌘C ⌘X ⌘V", "copy / cut / paste nodes"),
            ("⌘D", "duplicate the selection"),
            ("⌘.  or the bubble", "fold / unfold a branch"),
            ("⌘R", "tidy the whole tree"),
        ],
    ),
    (
        "write (edit)",
        &[
            ("← →", "caret by character"),
            ("⌥← ⌥→", "caret by word"),
            ("↑ ↓ / home end", "caret to start / end"),
            ("⌫ / ⌦", "delete back / forward"),
            ("⌥⌫ / ⌘⌫", "delete word / clear"),
            ("click the text", "caret there"),
            ("⌘L or click a line's +", "edit edge label"),
        ],
    ),
    (
        "select (edit)",
        &[
            ("⇧← ⇧→", "extend by character"),
            ("⌥⇧← ⌥⇧→", "extend by word"),
            ("⇧↑ ⇧↓ / ⇧home ⇧end", "extend to start / end"),
            ("⌘A", "select the whole label"),
            ("double-click", "select a word"),
            ("triple-click", "select the label"),
            ("⌘X ⌘C ⌘V", "cut / copy / paste text"),
        ],
    ),
    (
        "move",
        &[
            ("← → ↑ ↓", "move the selection one step"),
            ("⇧ + arrow", "move it a bigger stride"),
            ("drag a node", "move it, or the whole selection"),
            ("drop it on another", "hang the branch under it"),
            ("drag past the edge", "the canvas follows"),
            ("⇧ mid-drag", "lock to one axis"),
            ("⌥ mid-drag", "snap to the grid"),
            ("⌘⌥ ← → ↑ ↓", "nudge one grid step"),
        ],
    ),
    (
        "fly",
        &[
            ("two-finger swipe", "pan"),
            ("middle-drag", "pan (flick to coast)"),
            ("pinch / ⌘scroll", "zoom at pointer"),
            ("⌘+ / ⌘-", "zoom in / out"),
            ("⌘0 / ⌘1", "fit map / 100% on focus"),
        ],
    ),
    (
        "find",
        &[
            ("⌘F", "open the field"),
            ("typing", "search as you type"),
            ("↩ / ⇧↩", "next / previous match"),
            ("⌘G / ⌘⇧G", "the same, field closed"),
            ("Esc", "done"),
        ],
    ),
    (
        "session",
        &[
            ("⌘Z / ⌘⇧Z", "undo / redo"),
            ("⌘N", "new map"),
            ("⌘O", "open a map"),
            ("File ▸ Open Recent", "the last eight"),
            ("⌘S", "save now (autosaves anyway)"),
            ("⌘T", "paper / night"),
            ("⌘/", "this panel"),
            ("⌘Q", "quit"),
        ],
    ),
];

/// The context menu. Rows dispatch back through `run_menu`, so every entry is
/// the same code path as its keyboard equivalent.
fn context_menu(
    (at, rows): ((f32, f32), Vec<Option<(&'static str, &'static str, MenuCmd)>>),
    th: Theme,
    cx: &mut Context<MindMapView>,
) -> impl IntoElement {
    div()
        .absolute()
        .left(px(at.0))
        .top(px(at.1))
        .w(px(MENU_W))
        .py_1()
        .flex()
        .flex_col()
        // Opaque, and it swallows the press that would otherwise reach the map.
        .bg(c(th.bg))
        .border_1()
        .border_color(ca(th.ink, 0.40))
        .rounded_md()
        .occlude()
        .children(rows.into_iter().enumerate().map(|(i, row)| match row {
            // Boxed: a rule and a row are different element types, and a
            // `match` has to settle on one.
            None => div()
                .h(px(MENU_RULE_H))
                .flex()
                .items_center()
                .child(div().w_full().h(px(1.)).bg(ca(th.ink, 0.18)))
                .into_any_element(),
            Some((label, hint, cmd)) => div()
                .id(("menu-row", i))
                .h(px(MENU_ROW_H))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .text_color(c(th.ink))
                .hover(|s| s.bg(ca(th.accent, 0.12)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.run_menu(cmd, window, cx)),
                )
                .child(div().child(SharedString::from(label)))
                .child(
                    div()
                        .text_color(c(th.ink_quiet))
                        .child(SharedString::from(hint)),
                )
                .into_any_element(),
        }))
}

/// A one-line text field's contents: the typed text (or a ghosted placeholder
/// when empty) with a caret blinking at the end. One shape for the find box and
/// the shortcut panel's search alike, so a field always looks like a field you
/// can type into.
fn field_value(text: &str, placeholder: &str, caret_on: bool, th: Theme) -> impl IntoElement {
    let caret = div()
        .w(px(1.5))
        .h(px(15.))
        .flex_shrink_0()
        .when(caret_on, |d| d.bg(c(th.accent)));
    let body = div().flex_1().flex().items_center().min_w_0();
    if text.is_empty() {
        body.child(caret).child(
            div()
                .ml(px(4.))
                .text_color(ca(th.ink, 0.35))
                .child(SharedString::from(placeholder.to_string())),
        )
    } else {
        body.child(
            div()
                .text_color(c(th.ink))
                .child(SharedString::from(text.to_string())),
        )
        .child(caret.ml(px(1.)))
    }
}

/// Case-insensitive substring match against a shortcut's key, its description,
/// or its section title — so "zoom", "⌘", and "fly" all find the zoom rows.
fn help_matches(query: &str, title: &str, key: &str, what: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    title.to_lowercase().contains(&q)
        || key.to_lowercase().contains(&q)
        || what.to_lowercase().contains(&q)
}

fn help_overlay(
    th: Theme,
    query: &str,
    caret_on: bool,
    customise: gpui::AnyElement,
) -> impl IntoElement {
    // Keep only the sections with a surviving row. A title match keeps the
    // whole section; otherwise the rows are filtered one by one.
    let sections: Vec<(&'static str, Vec<&'static (&'static str, &'static str)>)> = HELP
        .iter()
        .filter_map(|(title, rows)| {
            let kept: Vec<_> = rows
                .iter()
                .filter(|(key, what)| help_matches(query, title, key, what))
                .collect();
            if kept.is_empty() {
                None
            } else {
                Some((*title, kept))
            }
        })
        .collect();
    let total: usize = sections.iter().map(|(_, r)| r.len()).sum();

    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        // Clicks and scrolls stop here. Without this they reach the canvas
        // underneath, so scrolling the shortcut list also pans the map.
        .occlude()
        .flex()
        .flex_col()
        .gap_4()
        .px_5()
        .py_5()
        // Opaque: a translucent scrim leaves the map ghosting through the text.
        .bg(c(th.bg))
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .flex_shrink_0()
                .gap_4()
                .pb_2()
                .mb_1()
                .border_b_1()
                .border_color(ca(th.ink, 0.14))
                .text_base()
                .text_color(c(th.ink))
                .child(div().flex_shrink_0().child("shortcuts"))
                // The search box, sized like the find field and carrying its own
                // caret, so ⌘F muscle memory reads it as a place to type.
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .max_w(px(360.))
                        .px_3()
                        .py_1()
                        .text_xs()
                        .bg(c(th.bg))
                        .border_1()
                        .border_color(ca(th.accent, 0.45))
                        .rounded_md()
                        .child(field_value(query, "search shortcuts", caret_on, th)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(c(th.ink_quiet))
                        .child("⌘/ or Esc to close"),
                ),
        )
        .child(
            // Scrolls rather than clipping when the window is short.
            div()
                .id("help-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                // The editor sits above the reference, and steps aside while the
                // search is narrowing the reference down.
                .when(query.trim().is_empty(), |el| el.child(customise))
                .when(total == 0, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(ca(th.ink, 0.45))
                            .child("no shortcut matches that"),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_x_8()
                        .gap_y_4()
                        .children(sections.into_iter().map(|(title, rows)| {
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .w(px(HELP_COL_W))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(c(th.accent))
                                        .child(SharedString::from(title)),
                                )
                                .children(rows.into_iter().map(|(key, what)| {
                                    div()
                                        .flex()
                                        .gap_2()
                                        .text_xs()
                                        .child(
                                            div()
                                                .w(px(HELP_KEY_W))
                                                .flex_shrink_0()
                                                .text_color(c(th.ink))
                                                .child(SharedString::from(*key)),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_color(c(th.ink_soft))
                                                .child(SharedString::from(*what)),
                                        )
                                }))
                        })),
                ),
        )
}
