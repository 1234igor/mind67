//! Rendering: turning a frame's worth of geometry into paths and glyphs.
//!
//! Nothing here knows about input, gestures or the document — `ui` reduces the
//! graph to a [`Frame`] and this module draws it.

use gpui::{App, Bounds, FontWeight, PathBuilder, Pixels, Point, SharedString, TextRun, Window,
    point, px};

use crate::camera::Camera;
use crate::markdown;
use crate::theme::{Theme, c, ca};

/// Zed buffer font. GPUI aliases `.ZedMono` / `Zed Plex Mono` → `Lilex`.
/// We load shipped `Lilex-Regular.ttf` at startup; system install works too.
pub const CODE_FONT: &str = "Lilex";

/// Node text size in **world** units — painted at `size * zoom`.
pub const FONT_NODE: f32 = 15.0;
pub const FONT_ROOT: f32 = 17.0;
pub const FONT_EDGE: f32 = 12.0;
/// The fold control's radius as a fraction of the (screen-space) node font, so
/// it stays the same visual size as the label's text at every zoom. At the
/// default 15px node font this is ~5px, matching the old fixed control.
pub const FOLD_BUBBLE_K: f32 = 0.34;
/// Below this on-screen size a node collapses to a mark. Kept very small on
/// purpose: the labels stay *text* right down to a smudge as you zoom out —
/// only genuinely sub-pixel sizes, where nothing could be read anyway, fall
/// back to a mark (which also keeps shaping off the hot path at extreme zoom).
pub const MIN_READABLE_PX: f32 = 1.5;

/// How wide a picture has to be on screen, in pixels, before it is worth
/// keeping a texture for.
///
/// This is a *drawn size* gate, and it has to be: [`MIN_READABLE_PX`] measures
/// a font, and at a fitted zoom a 15pt label is 2.25px, so it never fires.
/// `paint_image` uploads the entire decoded bitmap into the sprite atlas
/// however small the destination is, so without this a fitted map with two
/// hundred photos would try to make two hundred full textures resident to draw
/// two hundred smudges.
pub const IMAGE_MIN_PX: f32 = 28.0;
/// Line box height as a multiple of the font size. Text is drawn from the top
/// of that box, so centring a label means offsetting by half of it.
pub const LINE_H: f32 = 1.2;

/// One node, already reduced to what paint needs. Built once per frame from
/// the graph so the canvas never has to clone (or even see) the whole map.
pub struct PaintNode {
    pub id: u64,
    pub text: SharedString,
    /// Screen centre and ring half-extents.
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
    pub font: f32,
    pub is_root: bool,
    /// The keyboard cursor is here. Drawn quietly when it is not also selected:
    /// it says where the arrows start, not what an operation would touch.
    pub is_focus: bool,
    pub is_hover: bool,
    /// In the selection — what delete, drag and nudge would act on. This is the
    /// one that gets the accent.
    pub is_selected: bool,
    /// Releasing here would hang the dragged branch under this node.
    pub is_drop_target: bool,
    /// This node has children. `Some(n)` means it is folded and hiding `n` of
    /// them; `Some(0)` never happens. `None` means nothing to fold.
    pub fold: Option<Option<usize>>,
    pub empty: bool,
    /// Colour tag 1..=6, or `None` for plain ink.
    pub color: Option<u8>,
    /// The picture to draw instead of the label, if this node has one and it is
    /// both resident and big enough on screen to be worth a texture. Resolved
    /// in `build_frame`, so the paint path never decodes and never blocks.
    pub image: Option<std::sync::Arc<gpui::RenderImage>>,
    /// This node has a picture, whatever `image` says. A node whose image is
    /// still decoding — or too small to bother with — draws a plate rather than
    /// its file name.
    pub has_image: bool,
    /// The picture it names is not there. A different plate from the one a slow
    /// decode gets: a fault the reader has to be told about is not the same as
    /// an image that is taking its time.
    pub image_missing: bool,
    /// A find is running and this node is not a match: paint it dim.
    pub dim: bool,
    /// This node is a find match — `Some(true)` for the current one the view is
    /// parked on, `Some(false)` for the rest. `None` when no find is running.
    pub find_hit: Option<bool>,
    /// Caret `(dx, dy)` from the label's centre. Only the focused item has one,
    /// and only when nothing is selected — the band says it better.
    pub caret: Option<(f32, f32)>,
    /// Selected bands, one per visual line: `(dy, x0, x1)` from the centre.
    pub sel: Vec<(f32, f32, f32)>,
}

pub struct PaintEdge {
    pub label: SharedString,
    pub id: u64,
    /// Screen centres of the two endpoints.
    pub a: (f32, f32),
    pub b: (f32, f32),
    /// Ring half-extents at each end, for trimming the channel.
    pub ah: (f32, f32),
    pub bh: (f32, f32),
    pub depth: f32,
    pub is_focus: bool,
    pub is_hover: bool,
    /// The drag holding this edge's child has pulled far enough that releasing
    /// will cut it: draw the channel breaking, dashed and faint.
    pub is_severing: bool,
    pub caret: Option<(f32, f32)>,
    pub sel: Vec<(f32, f32, f32)>,
}

/// Everything the canvas needs, snapshotted at render time.
pub struct Frame {
    pub nodes: Vec<PaintNode>,
    pub edges: Vec<PaintEdge>,
    pub cam: Camera,
    pub theme: Theme,
    pub editing: bool,
    pub cursor_on: bool,
    /// A grow-from-ring gesture in flight: `(from_screen, pointer_screen)` for
    /// the preview line and the ghost ring at its end.
    pub grow: Option<((f32, f32), (f32, f32))>,
    /// Rubber band in screen coords, while one is being dragged.
    pub marquee: Option<(f32, f32, f32, f32)>,
    /// The corner minimap, when it is switched on.
    pub minimap: Option<Minimap>,
    /// Nodes in the map, painted or not — the culling denominator.
    pub total_nodes: usize,
}

/// An overview of the whole map — every node, not only the visible ones — and
/// the rectangle the camera is currently looking through.
pub struct Minimap {
    /// World-space bounds of all nodes: (min_x, min_y, max_x, max_y).
    pub world: (f32, f32, f32, f32),
    /// Every node's world position and colour tag.
    pub dots: Vec<(f32, f32, Option<u8>)>,
    /// The world rect the camera is showing: (min_x, min_y, max_x, max_y).
    pub view: (f32, f32, f32, f32),
}

/// Breathing room between the glyphs and the invisible box around them — what
/// edges are trimmed to and what a click has to land inside. Nothing is drawn
/// at this distance any more; it is routing and hit-testing, not decoration.
pub const PAD_X: f32 = 0.48;
pub const PAD_Y: f32 = 0.86;
/// How far outside the viewport something still counts as visible, in screen
/// px. Covers rules and labels that hang off a culled centre.
pub const CULL_MARGIN: f32 = 240.0;
/// One dash length for every dashed thing on the plate.
const DASH: f32 = 5.0;

/// Collects segments that share a colour and width into one path. GPUI
/// tessellates per `build()`, and a rosette is two dozen ticks — batching them
/// turns a per-tick cost into a per-node one.
struct Strokes {
    builder: Option<PathBuilder>,
    count: u64,
}

impl Strokes {
    fn new(width: f32) -> Self {
        Self {
            builder: Some(PathBuilder::stroke(px(width.max(0.4)))),
            count: 0,
        }
    }

    fn line(&mut self, a: (f32, f32), b: (f32, f32)) {
        if let Some(bd) = self.builder.as_mut() {
            bd.move_to(point(px(a.0), px(a.1)));
            bd.line_to(point(px(b.0), px(b.1)));
            self.count += 1;
        }
    }

    fn polygon(&mut self, pts: &[(f32, f32)]) {
        if pts.len() < 2 {
            return;
        }
        if let Some(bd) = self.builder.as_mut() {
            let screen: Vec<Point<Pixels>> =
                pts.iter().map(|(x, y)| point(px(*x), px(*y))).collect();
            bd.add_polygon(&screen, true);
            self.count += 1;
        }
    }

    /// Emits one path. Returns 1 if anything was painted, for the path counter.
    fn flush(mut self, window: &mut Window, color: impl Into<gpui::Background>) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let Some(builder) = self.builder.take() else {
            return 0;
        };
        match builder.build() {
            Ok(path) => {
                window.paint_path(path, color);
                1
            }
            Err(_) => 0,
        }
    }
}

pub fn paint_map(f: &Frame, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let timer = crate::perf::Frame::start();
    let mut paths = 0;
    // The band's wash goes *under* the map — painted last it veiled every node
    // it covered. Its dashed outline still reads fine from below.
    paths += paint_marquee(f, window);
    paths += paint_edges(f, window, cx);
    paths += paint_nodes(f, window, cx);
    if let Some((a, b)) = f.grow {
        // Grow-from-ring preview: a line to the pointer and a ghost ring where
        // the new child will land.
        let mut line = Strokes::new(1.2);
        line.line(a, b);
        paths += line.flush(window, ca(f.theme.accent, 0.5));
        let mut ring = Strokes::new(1.0);
        ring.polygon(&circle(b.0, b.1, 6.0));
        paths += ring.flush(window, ca(f.theme.accent, 0.65));
    }
    paths += paint_minimap(f, bounds, window);
    timer.end(paths, f.nodes.len() as u64, f.total_nodes as u64);
}

/// Edges are drawn as *channels*: a hairline that stops short of each label and
/// breaks open around its own.
fn paint_edges(f: &Frame, window: &mut Window, cx: &mut App) -> u64 {
    let z = f.cam.zoom();
    let mut paths = 0;
    for e in &f.edges {
        // A connection being torn off: a dashed, faded break, no label, no
        // hover caret. It reads as "let go here and this comes apart".
        if e.is_severing {
            let s0 = exit_point(e.a, e.b, e.ah.0, e.ah.1);
            let s1 = exit_point(e.b, e.a, e.bh.0, e.bh.1);
            let mut b = PathBuilder::stroke(px(1.4)).dash_array(&[px(DASH), px(DASH)]);
            b.move_to(point(px(s0.0), px(s0.1)));
            b.line_to(point(px(s1.0), px(s1.1)));
            if let Ok(path) = b.build() {
                window.paint_path(path, ca(f.theme.ink_faint, 0.9));
                paths += 1;
            }
            continue;
        }
        let color = if e.is_focus {
            c(f.theme.edge_accent)
        } else if e.is_hover {
            c(f.theme.hover)
        } else {
            c(f.theme.line)
        };
        // Thinner the deeper you go — reads as a hierarchy without any chrome.
        let base = (1.25 - e.depth * 0.14).max(0.7);
        let width = (base * z).clamp(0.6, 2.4) * if e.is_focus { 1.25 } else { 1.0 };

        // The channel runs ring-to-ring, not centre-to-centre.
        let s0 = exit_point(e.a, e.b, e.ah.0, e.ah.1);
        let s1 = exit_point(e.b, e.a, e.bh.0, e.bh.1);
        let (dx, dy) = (s1.0 - s0.0, s1.1 - s0.1);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1.0 {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len);

        // The straight midpoint is where `Graph::edge_mid` hit-tests.
        let mid = ((e.a.0 + e.b.0) * 0.5, (e.a.1 + e.b.1) * 0.5);
        let size = FONT_EDGE * z;
        let readable = size >= MIN_READABLE_PX;
        let has_label = !e.label.is_empty();
        // Hovering a bare line offers a place to type: open the same gap a label
        // would and drop a faint caret into it, so the invitation is the very
        // mark you'd get if you started typing.
        let type_hint = readable && e.is_hover && !has_label && !e.is_focus;

        let mut ink = Strokes::new(width);

        // Open a gap for the label so the line never crosses the letters.
        let gap = if readable && (has_label || e.is_focus || type_hint) {
            let w = if has_label {
                shape_width(window, &e.label, size, FontWeight::NORMAL)
            } else {
                size
            };
            w * 0.5 + size * 0.45
        } else {
            0.0
        };
        let half = len * 0.5;

        // One line, ring to ring — no doubled trunks, no depth-coded weight in
        // the count of strokes. A label opens a gap so the line never crosses
        // its own letters; otherwise it runs straight through.
        if gap > 0.0 && gap < half - 1.0 {
            let g0 = (mid.0 - ux * gap, mid.1 - uy * gap);
            let g1 = (mid.0 + ux * gap, mid.1 + uy * gap);
            ink.line(s0, g0);
            ink.line(g1, s1);
        } else {
            ink.line(s0, s1);
        }
        paths += ink.flush(window, color);

        if type_hint {
            paths += paint_caret(window, mid, (0.0, 0.0), size, ca(f.theme.ink_soft, 0.5));
        }

        if readable && (has_label || e.is_focus) {
            let label: &str = if has_label { &e.label } else { " " };
            let label_color = if e.is_focus {
                c(f.theme.edge_accent)
            } else {
                c(f.theme.ink_soft)
            };
            for &(dy, x0, x1) in &e.sel {
                paint_selection(
                    window,
                    (mid.0, mid.1 + dy),
                    (x0, x1),
                    size,
                    ca(f.theme.edge_accent, 0.14),
                );
            }
            paint_label(window, cx, label, mid, label_color.into(), size, FontWeight::NORMAL, &f.theme);
            if let (Some(caret), true) = (e.caret, f.editing && f.cursor_on) {
                paths += paint_caret(window, mid, caret, size, c(f.theme.edge_accent));
            }
        }
    }
    paths
}

/// A node **is** its label. Nothing is drawn around it: state is said by a rule
/// underneath, the way a draughtsman underscores a callout, so an ordinary node
/// costs exactly the ink of its own letters.
fn paint_nodes(f: &Frame, window: &mut Window, cx: &mut App) -> u64 {
    let z = f.cam.zoom();
    let mut paths = 0;
    for n in &f.nodes {
        // Too small to read: leave a mark so the shape of the map survives.
        if n.font < MIN_READABLE_PX {
            let r = mark_radius(n.font);
            let color = if n.is_focus {
                c(f.theme.accent)
            } else {
                c(f.theme.ink_faint)
            };
            let mut mark = Strokes::new(r);
            mark.line((n.cx - r, n.cy), (n.cx + r, n.cy));
            paths += mark.flush(window, color);
            continue;
        }

        // A picture stands in for the label entirely.
        if n.has_image {
            // A find dims every node it did not match; a picture has to dim too,
            // or a search leaves the photographs as the loudest thing on a page
            // of greyed-out text.
            let wash = if n.dim { 0.35 } else { 1.0 };
            let bounds = Bounds::from_corners(
                point(px(n.cx - n.hw), px(n.cy - n.hh)),
                point(px(n.cx + n.hw), px(n.cy + n.hh)),
            );
            let radii = gpui::Corners::all(px((n.font * 0.24).min(n.hw.min(n.hh))));
            // A drop onto a picture re-hangs a branch under it just as it does
            // onto text, so it has to light up the same way.
            if n.is_drop_target {
                window.paint_quad(
                    gpui::fill(bounds, ca(f.theme.accent, 0.35)).corner_radii(radii),
                );
            }
            match &n.image {
                Some(data) => {
                    let _ = window.paint_image(bounds, radii, data.clone(), 0, false);
                    if wash < 1.0 {
                        window.paint_quad(
                            gpui::fill(bounds, ca(f.theme.bg, 1.0 - wash)).corner_radii(radii),
                        );
                    }
                }
                // A plate the exact size of the picture, so nothing moves when
                // it arrives — but a missing file gets a distinctly heavier one,
                // because it is never going to arrive.
                None => {
                    let wash = if n.image_missing { 0.30 } else { 0.14 };
                    window.paint_quad(
                        gpui::fill(bounds, ca(f.theme.ink_faint, wash)).corner_radii(radii),
                    );
                    if n.image_missing {
                        let mut cross = Strokes::new((1.2 * z.sqrt()).clamp(0.8, 2.0));
                        cross.line((n.cx - n.hw, n.cy - n.hh), (n.cx + n.hw, n.cy + n.hh));
                        cross.line((n.cx + n.hw, n.cy - n.hh), (n.cx - n.hw, n.cy + n.hh));
                        paths += cross.flush(window, c(f.theme.ink_faint));
                    }
                }
            }
            // The grip. Only on a picture the pointer is on or that is
            // selected: a handle on every image at all times would be clutter
            // on a map that is otherwise only lines and letters.
            // Selection and focus read on the picture as a frame around it,
            // since there are no letters to wash.
            //
            // Two rules, because one is not enough: an accent frame drawn on a
            // picture the same colour as the accent is invisible. The accent
            // sits just *outside* the picture and a pale hairline runs along the
            // picture's own edge to separate them, so the pair reads on a
            // photograph of anything.
            if n.is_selected || n.is_focus || n.is_hover {
                let color = if n.is_selected || n.is_focus {
                    c(f.theme.accent)
                } else {
                    c(f.theme.ink_faint)
                };
                let out = (2.0 * z.sqrt()).clamp(1.5, 3.0);
                let (ox, oy) = (n.hw + out, n.hh + out);
                let mut ring = Strokes::new(out);
                ring.line((n.cx - ox, n.cy - oy), (n.cx + ox, n.cy - oy));
                ring.line((n.cx + ox, n.cy - oy), (n.cx + ox, n.cy + oy));
                ring.line((n.cx + ox, n.cy + oy), (n.cx - ox, n.cy + oy));
                ring.line((n.cx - ox, n.cy + oy), (n.cx - ox, n.cy - oy));
                paths += ring.flush(window, color);

                let mut edge = Strokes::new((0.8 * z.sqrt()).clamp(0.6, 1.4));
                edge.line((n.cx - n.hw, n.cy - n.hh), (n.cx + n.hw, n.cy - n.hh));
                edge.line((n.cx + n.hw, n.cy - n.hh), (n.cx + n.hw, n.cy + n.hh));
                edge.line((n.cx + n.hw, n.cy + n.hh), (n.cx - n.hw, n.cy + n.hh));
                edge.line((n.cx - n.hw, n.cy + n.hh), (n.cx - n.hw, n.cy - n.hh));
                paths += edge.flush(window, gpui::hsla(0., 0., 1., 0.85));
            }
            // Paper filled, accent edged, and centred *on* the corner: half on
            // the picture and half off it, so it reads as a handle on the
            // boundary rather than a button dropped over the image. Two quads
            // rather than a border, which is what the immediate-mode painter
            // gives us.
            if n.is_hover || n.is_selected || n.is_focus {
                let (gx, gy, r) = resize_grip(n.cx, n.cy, n.hw, n.hh, z);
                let radii = gpui::Corners::all(px(r * 0.3));
                let quad = |inset: f32, colour| {
                    gpui::fill(
                        Bounds::from_corners(
                            point(px(gx - r + inset), px(gy - r + inset)),
                            point(px(gx + r - inset), px(gy + r - inset)),
                        ),
                        colour,
                    )
                    .corner_radii(radii)
                };
                window.paint_quad(quad(0.0, c(f.theme.accent)));
                window.paint_quad(quad((r * 0.42).max(1.2), c(f.theme.bg)));
            }

            continue;
        }

        // Engraved, not chunky: a true hairline that barely thickens with zoom.
        let hair = (0.75 * z.sqrt()).clamp(0.6, 1.4);

        // A drop target is the one gesture that changes the *shape* of the map
        // rather than where things sit, so it has to be unmistakable before the
        // button comes up. With nothing drawn around a node, the only way left
        // to say "into this one" is to light the ground under its letters.
        if n.is_drop_target {
            let pad = n.font * 0.3;
            window.paint_quad(
                gpui::fill(
                    Bounds::from_corners(
                        point(px(n.cx - n.hw), px(n.cy - n.hh - pad * 0.2)),
                        point(px(n.cx + n.hw), px(n.cy + n.hh + pad * 0.2)),
                    ),
                    ca(f.theme.accent, 0.16),
                )
                .corner_radii(gpui::Corners::all(px(n.font * 0.24))),
            );
        }

        // State is a wash under the letters, not a rule beside them. `None` is
        // the common case — a node you are neither on nor pointing at draws
        // nothing at all. The drop target lights its own ground above, so it is
        // left out here.
        // A low-alpha accent over a near-black ground loses its blue, so the
        // dark theme leans on stronger washes to keep selection legible.
        let dark = f.theme.dark;
        let wash = if n.is_selected && n.is_focus {
            Some(ca(f.theme.accent, if dark { 0.32 } else { 0.20 }))
        } else if n.is_selected {
            // Selected, but not the one the arrows start from: lighter, so the
            // primary still reads as primary.
            Some(ca(f.theme.accent, if dark { 0.20 } else { 0.12 }))
        } else if n.is_focus && f.editing {
            // The node you are typing into, with no selection band to show it:
            // a quiet ink plate. In Browse a bare cursor draws nothing — clicking
            // empty ground must leave no lingering square behind the last node.
            Some(ca(f.theme.ink, if dark { 0.15 } else { 0.11 }))
        } else if n.is_hover {
            Some(ca(f.theme.ink, if dark { 0.08 } else { 0.05 }))
        } else {
            None
        };
        if let Some(color) = wash {
            let pad = n.font * 0.26;
            window.paint_quad(
                gpui::fill(
                    Bounds::from_corners(
                        point(px(n.cx - n.hw - pad * 0.2), px(n.cy - n.hh - pad * 0.1)),
                        point(px(n.cx + n.hw + pad * 0.2), px(n.cy + n.hh + pad * 0.1)),
                    ),
                    color,
                )
                .corner_radii(gpui::Corners::all(px(n.font * 0.3))),
            );
        }

        // The fold control: hollow when the branch is open, filled when it is
        // folded away. A filled dot is the map saying "there is more here" —
        // the one thing a folded branch cannot say for itself.
        if let Some(state) = n.fold {
            let (bx, by, r) = fold_bubble(n.cx, n.cy, n.hw, n.font);
            let ring_color = if n.is_selected {
                ca(f.theme.accent, 0.9)
            } else {
                ca(f.theme.ink, 0.55)
            };
            // Knock the ground out under it first: the first outgoing channel
            // leaves the ring right about here, and a line running through the
            // middle of a control makes it read as decoration.
            let mut moat = Strokes::new(r * 1.5);
            moat.polygon(&circle(bx, by, r * 0.7));
            paths += moat.flush(window, c(f.theme.bg));
            let mut bubble = Strokes::new((hair * 0.9).max(0.6));
            bubble.polygon(&circle(bx, by, r));
            paths += bubble.flush(window, ring_color);
            if state.is_some() {
                // Folded: actually fill it. A short bar at width r*1.1 left a
                // squat rectangle inside a circle — an ⊙, not a dot — and this
                // is the one control whose whole job is to say "there is more".
                let mut fill = Strokes::new(r * 1.0);
                fill.polygon(&circle(bx, by, r * 0.5));
                paths += fill.flush(window, ring_color);
            }
        }

        // Find highlight: a soft wash behind a match, brighter for the current
        // one. Painted over the state wash so a selected match still reads.
        if let Some(current) = n.find_hit {
            let a = match (current, dark) {
                (true, true) => 0.34,
                (true, false) => 0.22,
                (false, true) => 0.18,
                (false, false) => 0.12,
            };
            let pad = n.font * 0.26;
            window.paint_quad(
                gpui::fill(
                    Bounds::from_corners(
                        point(px(n.cx - n.hw - pad * 0.2), px(n.cy - n.hh - pad * 0.1)),
                        point(px(n.cx + n.hw + pad * 0.2), px(n.cy + n.hh + pad * 0.1)),
                    ),
                    ca(f.theme.accent, a),
                )
                .corner_radii(gpui::Corners::all(px(n.font * 0.3))),
            );
        }

        // `build_frame` already resolved the placeholder, if any. A colour tag
        // wins over plain ink; selection still shows through the wash behind it.
        let display: &str = &n.text;
        let base_rgb = if let Some(rgb) = tag_color(n.color) {
            c(rgb)
        } else if n.is_selected {
            c(f.theme.accent)
        } else if n.empty {
            c(f.theme.ink_faint)
        } else {
            c(f.theme.ink)
        };
        // Dim the labels a find has filtered out, so the hits carry the eye.
        let mut color: gpui::Hsla = base_rgb.into();
        if n.dim {
            color.a *= 0.4;
        }
        let weight = if n.is_root {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        };
        for &(dy, x0, x1) in &n.sel {
            paint_selection(
                window,
                (n.cx, n.cy + dy),
                (x0, x1),
                n.font,
                ca(f.theme.accent, 0.14),
            );
        }
        paint_label(window, cx, display, (n.cx, n.cy), color.into(), n.font, weight, &f.theme);
        if let (Some(caret), true) = (n.caret, f.editing && f.cursor_on) {
            paths += paint_caret(window, (n.cx, n.cy), caret, n.font, c(f.theme.accent));
        }
    }
    paths
}

/// The colour-tag palette, keyed 1..=6. Chosen to read on both the paper and
/// the night ground. `None` and anything out of range fall through to ink.
pub fn tag_color(tag: Option<u8>) -> Option<u32> {
    match tag {
        Some(1) => Some(0xD1_43_43), // red
        Some(2) => Some(0xC7_8A_1E), // amber
        Some(3) => Some(0x2E_9E_54), // green
        Some(4) => Some(0x1F_9E_9E), // teal
        Some(5) => Some(0x3B_6F_E0), // blue
        Some(6) => Some(0x8A_5C_D6), // violet
        _ => None,
    }
}

/// Where the segment `from -> toward` leaves the ellipse inscribed in `from`'s
/// ring. Used to start channels at the ring instead of the text centre.
fn exit_point(from: (f32, f32), toward: (f32, f32), hw: f32, hh: f32) -> (f32, f32) {
    let (dx, dy) = (toward.0 - from.0, toward.1 - from.1);
    let (hw, hh) = (hw.max(1.0), hh.max(1.0));
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return from;
    }
    let (ux, uy) = (dx / len, dy / len);
    // The ring is a *stadium*, not the ellipse inscribed in it. The two agree
    // only on the axes; on every diagonal the stadium is strictly outside, so
    // solving against the ellipse ended the channel — and its end tick — inside
    // the outline, on top of the letters.
    let flat = (hw - hh).max(0.0);
    // Flat top or bottom: leave through the straight side.
    if uy.abs() > 1e-4 {
        let t = hh / uy.abs();
        if (ux * t).abs() <= flat {
            return (from.0 + ux * t, from.1 + uy * t);
        }
    }
    // Otherwise it leaves through one of the semicircular caps. Solve
    // |c + t·u| = hh with the cap centre offset along x.
    let cx = if ux >= 0.0 { flat } else { -flat };
    let (px, py) = (-cx, 0.0);
    let b = px * ux + py * uy;
    let c = px * px + py * py - hh * hh;
    let disc = (b * b - c).max(0.0);
    let t = -b + disc.sqrt();
    (from.0 + ux * t, from.1 + uy * t)
}

/// Closed outline of a circle, for the fold bubble.
fn circle(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    use std::f32::consts::PI;
    let seg = 14;
    (0..=seg)
        .map(|i| {
            let a = i as f32 / seg as f32 * PI * 2.0;
            (cx + a.cos() * r, cy + a.sin() * r)
        })
        .collect()
}

/// The corner minimap: the whole map shrunk into a panel, with a box marking
/// what the camera is looking at. Nothing here is hit-tested — it is a compass,
/// not a control.
fn paint_minimap(f: &Frame, bounds: Bounds<Pixels>, window: &mut Window) -> u64 {
    let Some(m) = &f.minimap else { return 0 };
    let mut paths = 0;

    // Fit the world bounds into a box of at most this size, keeping aspect.
    const MAX_W: f32 = 200.0;
    const MAX_H: f32 = 150.0;
    const PAD: f32 = 8.0;
    const MARGIN: f32 = 16.0;

    let (wx0, wy0, wx1, wy1) = m.world;
    let ww = (wx1 - wx0).max(1.0);
    let wh = (wy1 - wy0).max(1.0);
    let scale = (MAX_W / ww).min(MAX_H / wh);
    let inner_w = ww * scale;
    let inner_h = wh * scale;
    let panel_w = inner_w + PAD * 2.0;
    let panel_h = inner_h + PAD * 2.0;

    let right = f32::from(bounds.origin.x) + f32::from(bounds.size.width);
    let bottom = f32::from(bounds.origin.y) + f32::from(bounds.size.height);
    let ox = right - MARGIN - panel_w;
    let oy = bottom - MARGIN - panel_h;

    // Ground plate: opaque, so the map behind does not read through it.
    window.paint_quad(
        gpui::fill(
            Bounds::from_corners(
                point(px(ox), px(oy)),
                point(px(ox + panel_w), px(oy + panel_h)),
            ),
            ca(f.theme.bg, 0.96),
        )
        .corner_radii(gpui::Corners::all(px(6.0))),
    );
    // A hairline frame around it.
    let mut frame = Strokes::new(1.0);
    frame.polygon(&[
        (ox, oy),
        (ox + panel_w, oy),
        (ox + panel_w, oy + panel_h),
        (ox, oy + panel_h),
    ]);
    paths += frame.flush(window, ca(f.theme.ink, 0.25));

    let map = |x: f32, y: f32| (ox + PAD + (x - wx0) * scale, oy + PAD + (y - wy0) * scale);

    // The view rectangle, clamped to the panel so it always reads as "here".
    let (vx0, vy0) = map(m.view.0, m.view.1);
    let (vx1, vy1) = map(m.view.2, m.view.3);
    let clx0 = vx0.clamp(ox + PAD, ox + PAD + inner_w);
    let cly0 = vy0.clamp(oy + PAD, oy + PAD + inner_h);
    let clx1 = vx1.clamp(ox + PAD, ox + PAD + inner_w);
    let cly1 = vy1.clamp(oy + PAD, oy + PAD + inner_h);
    window.paint_quad(gpui::fill(
        Bounds::from_corners(point(px(clx0), px(cly0)), point(px(clx1), px(cly1))),
        ca(f.theme.accent, 0.12),
    ));
    let mut view = Strokes::new(1.0);
    view.polygon(&[
        (clx0, cly0),
        (clx1, cly0),
        (clx1, cly1),
        (clx0, cly1),
    ]);
    paths += view.flush(window, ca(f.theme.accent, 0.7));

    // Every node as a small mark. Coloured dots keep their colour; the rest are
    // quiet ink. Batched by colour so the whole map is a handful of paths.
    let mut ink = Strokes::new(2.4);
    let mut tinted: [(Strokes, bool); 6] = [
        (Strokes::new(2.4), false),
        (Strokes::new(2.4), false),
        (Strokes::new(2.4), false),
        (Strokes::new(2.4), false),
        (Strokes::new(2.4), false),
        (Strokes::new(2.4), false),
    ];
    for &(x, y, color) in &m.dots {
        let (sx, sy) = map(x, y);
        match color {
            Some(t @ 1..=6) => {
                let slot = &mut tinted[(t - 1) as usize];
                slot.0.line((sx - 1.0, sy), (sx + 1.0, sy));
                slot.1 = true;
            }
            _ => ink.line((sx - 1.0, sy), (sx + 1.0, sy)),
        }
    }
    paths += ink.flush(window, ca(f.theme.ink, 0.55));
    for (i, (strokes, used)) in tinted.into_iter().enumerate() {
        if used {
            let rgb = tag_color(Some((i + 1) as u8)).unwrap_or(f.theme.ink);
            paths += strokes.flush(window, c(rgb));
        }
    }
    paths
}

/// On-screen half-length of the mark a node collapses to below
/// `MIN_READABLE_PX`. Shared by paint and hit-testing so the two agree.
pub fn mark_radius(size: f32) -> f32 {
    (2.5f32).max(size)
}

/// Where a node's fold bubble sits: just past the label, on the side its
/// children hang off. Shared by paint and hit-testing so the two agree.
pub fn fold_bubble(cx: f32, cy: f32, hw: f32, font: f32) -> (f32, f32, f32) {
    // Sized to the text it rides beside, so it reads as part of the label at
    // every zoom rather than a fixed dot that looms when you zoom out. `font`
    // is already screen-space (base * zoom), so the ring tracks the glyphs.
    let r = (font * FOLD_BUBBLE_K).max(1.5);
    (cx + hw + r * 1.4, cy, r)
}

/// The corner grip that resizes a picture, in screen coordinates: `(x, y, r)`.
///
/// One function, shared by paint and hit test, exactly as [`fold_bubble`] is —
/// a grip drawn anywhere other than where it is grabbed is worse than no grip.
pub fn resize_grip(cx: f32, cy: f32, hw: f32, hh: f32, z: f32) -> (f32, f32, f32) {
    let r = (7.0 * z.clamp(0.6, 1.6)).clamp(4.0, 11.0);
    (cx + hw, cy + hh, r)
}

/// The rubber band: a filled wash under a dashed outline, so it reads as a
/// gesture in progress rather than as a thing that has been added to the map.
fn paint_marquee(f: &Frame, window: &mut Window) -> u64 {
    let Some((x0, y0, x1, y1)) = f.marquee else {
        return 0;
    };
    let (l, r) = (x0.min(x1), x0.max(x1));
    let (t, b) = (y0.min(y1), y0.max(y1));
    if r - l < 1.0 && b - t < 1.0 {
        return 0;
    }
    window.paint_quad(gpui::fill(
        Bounds::from_corners(point(px(l), px(t)), point(px(r), px(b))),
        ca(f.theme.accent, 0.07),
    ));
    let mut edge = Strokes::new(1.0);
    // Dashes, not a solid rule: the plate's lines all mean something permanent.
    let dash = DASH;
    let mut run = |ax: f32, ay: f32, bx: f32, by: f32| {
        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        if len < 1.0 {
            return;
        }
        let (ux, uy) = ((bx - ax) / len, (by - ay) / len);
        let mut d = 0.0;
        while d < len {
            let e = (d + dash).min(len);
            edge.line((ax + ux * d, ay + uy * d), (ax + ux * e, ay + uy * e));
            d = e + dash;
        }
    };
    run(l, t, r, t);
    run(r, t, r, b);
    run(r, b, l, b);
    run(l, b, l, t);
    edge.flush(window, ca(f.theme.accent, 0.55))
}

/// The band behind selected glyphs. `x0`/`x1` are offsets from the label's
/// centre. Painted before the text so the glyphs stay on top of it, and kept
/// low-alpha so the ink still reads as ink rather than reversing out.
fn paint_selection(
    window: &mut Window,
    origin: (f32, f32),
    (x0, x1): (f32, f32),
    font_size: f32,
    color: gpui::Hsla,
) {
    let pad = font_size * 0.14;
    let (top, bottom) = (
        origin.1 - font_size * 0.62 - pad * 0.5,
        origin.1 + font_size * 0.52 + pad * 0.5,
    );
    // A zero-width band would still paint a sliver; a selection always spans
    // at least the glyph it started on.
    let left = origin.0 + x0.min(x1) - pad;
    let right = origin.0 + x0.max(x1) + pad;
    let bounds = Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom)));
    window.paint_quad(
        gpui::fill(bounds, color).corner_radii(gpui::Corners::all(px(font_size * 0.16))),
    );
}

/// `(dx, dy)` is the caret's offset from the label's centre, already measured
/// against the text before it — `dy` picks the visual line.
fn paint_caret(
    window: &mut Window,
    origin: (f32, f32),
    (dx, dy): (f32, f32),
    font_size: f32,
    color: impl Into<gpui::Background>,
) -> u64 {
    let caret_x = origin.0 + dx;
    let cy = origin.1 + dy;
    let mut s = Strokes::new(1.5);
    s.line(
        (caret_x, cy - font_size * 0.55),
        (caret_x, cy + font_size * 0.45),
    );
    s.flush(window, color)
}

/// The visual lines of a label — split on the hard line breaks a ⇧↩ inserts.
/// Empty text is one empty line, so an empty node still measures like a line.
pub fn label_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        vec![""]
    } else {
        text.split('\n').collect()
    }
}

/// y offset of visual line `i` of `n`, from the block's vertical centre.
pub fn line_offset_y(i: usize, n: usize, font_size: f32) -> f32 {
    (i as f32 - (n as f32 - 1.0) * 0.5) * font_size * LINE_H
}

/// Paint centered text, one row per hard line break, the block centred on
/// `origin`.
fn paint_label(
    window: &mut Window,
    cx: &mut App,
    text: &str,
    origin: (f32, f32),
    color: gpui::Hsla,
    font_size: f32,
    weight: FontWeight,
    theme: &Theme,
) {
    let lines = label_lines(text);
    let n = lines.len();
    for (i, line) in lines.iter().enumerate() {
        let mut style = window.text_style();
        style.font_family = CODE_FONT.into();
        style.font_weight = weight;
        let base_font = style.font();
        // Markdown styling rides on the monospace font, so bold/italic/colour
        // never change a glyph's advance — the measured width and caret stay
        // exact even though the label paints styled. Markers stay in the text,
        // dimmed, so what you edit is what you see.
        let spans = markdown::highlight_line(line, false);
        let runs: Vec<TextRun> = if spans.is_empty() {
            vec![TextRun {
                len: line.len(),
                font: base_font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }]
        } else {
            spans
                .iter()
                .map(|s| {
                    let (w, italic, run_color, underline, strike) =
                        md_attrs(s.style, color, weight, theme);
                    let mut font = base_font.clone();
                    font.weight = w;
                    font.style = if italic {
                        gpui::FontStyle::Italic
                    } else {
                        gpui::FontStyle::Normal
                    };
                    TextRun {
                        len: s.end - s.start,
                        font,
                        color: run_color,
                        background_color: None,
                        underline: underline.then(|| gpui::UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(run_color),
                            wavy: false,
                        }),
                        strikethrough: strike.then(|| gpui::StrikethroughStyle {
                            thickness: px(1.0),
                            color: Some(run_color),
                        }),
                    }
                })
                .collect()
        };
        let shaped = window.text_system().shape_line(
            SharedString::from((*line).to_owned()),
            px(font_size),
            &runs,
            None,
        );
        let w: f32 = shaped.width.into();
        let dy = line_offset_y(i, n, font_size);
        let draw_at = point(
            px(origin.0 - w * 0.5),
            px(origin.1 + dy - font_size * LINE_H * 0.5),
        );
        let _ = shaped.paint(draw_at, px(font_size * LINE_H), window, cx);
    }
}

/// Map a markdown role to run attributes, size-preserving on purpose (headings
/// go bold, not big) so a styled label measures exactly like its plain text.
/// Returns `(weight, italic, colour, underline, strikethrough)`.
fn md_attrs(
    style: markdown::MdStyle,
    base: gpui::Hsla,
    weight: FontWeight,
    theme: &Theme,
) -> (FontWeight, bool, gpui::Hsla, bool, bool) {
    use markdown::MdStyle::*;
    let dim: gpui::Hsla = gpui::rgb(theme.ink_faint).into();
    let soft: gpui::Hsla = gpui::rgb(theme.ink_soft).into();
    let accent: gpui::Hsla = gpui::rgb(theme.accent).into();
    match style {
        Text | Quote | CodeBlock => (weight, false, base, false, false),
        Heading(_) | Bold => (FontWeight::BOLD, false, base, false, false),
        Italic => (weight, true, base, false, false),
        BoldItalic => (FontWeight::BOLD, true, base, false, false),
        Strikethrough => (weight, false, soft, false, true),
        Code => (weight, false, soft, false, false),
        LinkText => (weight, false, accent, false, false),
        LinkUrl => (weight, false, accent, true, false),
        TaskDone => (weight, false, accent, false, false),
        HeadingMarker(_) | Marker | Fence | ListMarker | QuoteMarker | Separator | TaskOpen => {
            (weight, false, dim, false, false)
        }
    }
}

/// Lay `text` out exactly as `paint_label` would, without painting it.
fn shape(
    window: &mut Window,
    text: &str,
    font_size: f32,
    weight: FontWeight,
) -> gpui::ShapedLine {
    let mut style = window.text_style();
    style.font_family = CODE_FONT.into();
    style.font_weight = weight;
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: gpui::Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window.text_system().shape_line(
        SharedString::from(text.to_string()),
        px(font_size),
        &[run],
        None,
    )
}

/// Shaped width of `text` at `font_size`, without painting.
pub fn shape_width(window: &mut Window, text: &str, font_size: f32, weight: FontWeight) -> f32 {
    shape(window, text, font_size, weight).width.into()
}

/// Byte offset of the caret position nearest `dx`, where `dx` is measured from
/// the *centre* of the label — the same origin `caret_dx` and `paint_caret`
/// use, so a click lands on the boundary the caret is then drawn at.
pub fn index_for_offset(
    window: &mut Window,
    text: &str,
    font_size: f32,
    weight: FontWeight,
    dx: f32,
) -> usize {
    let line = shape(window, text, font_size, weight);
    let half: f32 = f32::from(line.width) * 0.5;
    line.closest_index_for_x(px(dx + half))
}
