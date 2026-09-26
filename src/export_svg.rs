//! Render a [`Graph`] to a standalone SVG document — a pure, dependency-free
//! function behind "File ▸ Export…". No GPUI, no I/O, no camera: an export is
//! the map in its own **world** coordinates.
//!
//! The visual language mirrors `paint.rs` after its simplification: an edge is
//! a single thin line running from one node to another (no doubled rails, no
//! ticks, no crosses), and a node is just its text label (no box, no
//! underline). The canvas font is Lilex, a monospace, so `font-family` is
//! `"Lilex, monospace"`.
//!
//! Note on folding: `paint.rs` speaks of nodes collapsing to *marks*, but that
//! is a zoom/readability effect — this build's [`Node`] has no `collapsed`
//! field and the model has no fold state, so every node in the map is visible
//! and every edge between two present nodes is drawn.

// Until the UI wires "File ▸ Export…" to `to_svg`, the binary never calls into
// this module, so its items read as dead code outside the test build.
#![allow(dead_code)]

use std::fmt::Write as _;

use crate::graph::Graph;

/// Node text sizes, matched to `paint.rs` (`FONT_ROOT` / `FONT_NODE` /
/// `FONT_EDGE`) so an export reads like the canvas it came from.
const FONT_ROOT: f32 = 17.0;
const FONT_NODE: f32 = 15.0;
const FONT_EDGE: f32 = 12.0;

/// Lilex is monospace: one glyph advances ~0.6em. Used to estimate a label's
/// footprint for the bounding box and to trim edges short of the letters.
const ADVANCE: f32 = 0.6;

/// Blank space kept around the whole drawing, in world units.
const MARGIN: f32 = 40.0;

/// Line-height factor, matched to `paint.rs`, for stacking hard-broken lines.
const LINE_H: f32 = 1.2;

/// The visual lines of a label — split on the hard breaks a ⇧↩ inserts. Empty
/// text is one empty line.
fn label_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        vec![""]
    } else {
        text.split('\n').collect()
    }
}

const BG: &str = "#fafafa";
const INK: &str = "#1a1a1a";
const LINE: &str = "#555";

/// Char count; labels are short, so `chars().count()` is a fine width proxy.
fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Text size for a node: the root is bigger (and painted bold), like the app.
fn font_for(g: &Graph, id: u64) -> f32 {
    if id == g.root_id {
        FONT_ROOT
    } else {
        FONT_NODE
    }
}

/// Estimated half-width / half-height of a label's box, in world units. A
/// floor keeps an empty or single-letter node from letting an edge run into
/// its centre.
fn half_extent(text: &str, font: f32) -> (f32, f32) {
    let lines = label_lines(text);
    let widest = lines.iter().map(|l| char_count(l)).max().unwrap_or(0);
    let w = widest as f32 * font * ADVANCE;
    let h = lines.len() as f32 * font * LINE_H;
    ((w * 0.5).max(font * 0.5), (h * 0.5).max(font * 0.5))
}

/// XML-escape text destined for element content or an attribute value.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Point where the ray `center -> toward` leaves the axis-aligned box of
/// half-size `(hw, hh)` centred on `center`, clamped so it never overshoots
/// `toward`. This is where an edge should start, so it stops at the label.
fn box_exit(center: (f32, f32), toward: (f32, f32), hw: f32, hh: f32) -> (f32, f32) {
    let (dx, dy) = (toward.0 - center.0, toward.1 - center.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return center;
    }
    let (ux, uy) = (dx / len, dy / len);
    let tx = if ux.abs() < 1e-6 {
        f32::INFINITY
    } else {
        hw / ux.abs()
    };
    let ty = if uy.abs() < 1e-6 {
        f32::INFINITY
    } else {
        hh / uy.abs()
    };
    let t = tx.min(ty).min(len);
    (center.0 + ux * t, center.1 + uy * t)
}

/// Render `g` to a complete, self-contained SVG document string.
pub fn to_svg(g: &Graph) -> String {
    // Stable id order → deterministic output (`HashMap` iteration is not).
    let mut ids: Vec<u64> = g.nodes.keys().copied().collect();
    ids.sort_unstable();

    // Bounding box over every node, padded for its label so text is never
    // clipped by the viewBox edge.
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for &id in &ids {
        let n = &g.nodes[&id];
        let (hw, hh) = half_extent(&n.text, font_for(g, id));
        min_x = min_x.min(n.x - hw);
        max_x = max_x.max(n.x + hw);
        min_y = min_y.min(n.y - hh);
        max_y = max_y.max(n.y + hh);
    }
    // No nodes, or non-finite coordinates from a corrupt file: fall back to a
    // small, valid frame rather than emitting NaN into the viewBox.
    if !(min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite()) {
        min_x = -100.0;
        min_y = -100.0;
        max_x = 100.0;
        max_y = 100.0;
    }
    let vx = min_x - MARGIN;
    let vy = min_y - MARGIN;
    let vw = (max_x - min_x + 2.0 * MARGIN).max(1.0);
    let vh = (max_y - min_y + 2.0 * MARGIN).max(1.0);

    let mut s = String::new();
    let _ = write!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{vx:.2} {vy:.2} {vw:.2} {vh:.2}\" \
         width=\"{vw:.0}\" height=\"{vh:.0}\" font-family=\"Lilex, monospace\">\n"
    );
    // Full-viewBox light background.
    let _ = write!(
        s,
        "  <rect x=\"{vx:.2}\" y=\"{vy:.2}\" width=\"{vw:.2}\" height=\"{vh:.2}\" fill=\"{BG}\"/>\n"
    );

    // Edges first, so node labels sit on top of the lines.
    let mut eids: Vec<u64> = g.edges.keys().copied().collect();
    eids.sort_unstable();
    for eid in eids {
        let e = &g.edges[&eid];
        let (Some(a), Some(b)) = (g.nodes.get(&e.from), g.nodes.get(&e.to)) else {
            continue;
        };
        let (ahw, ahh) = half_extent(&a.text, font_for(g, e.from));
        let (bhw, bhh) = half_extent(&b.text, font_for(g, e.to));
        let ac = (a.x, a.y);
        let bc = (b.x, b.y);
        // Start each end at the label box, not the centre — the line stops
        // short of the letters at both nodes.
        let p0 = box_exit(ac, bc, ahw, ahh);
        let p1 = box_exit(bc, ac, bhw, bhh);
        let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
        if (dx * dx + dy * dy).sqrt() >= 1.0 {
            let _ = write!(
                s,
                "  <line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" \
                 stroke=\"{LINE}\" stroke-width=\"1\"/>\n",
                p0.0, p0.1, p1.0, p1.1
            );
        }
        // A non-empty label rides the straight midpoint (where the app
        // hit-tests it), on a small background chip so the line never shows
        // through the text.
        if !e.label.is_empty() {
            let mx = (a.x + b.x) * 0.5;
            let my = (a.y + b.y) * 0.5;
            let lw = char_count(&e.label) as f32 * FONT_EDGE * ADVANCE;
            let _ = write!(
                s,
                "  <rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{BG}\"/>\n",
                mx - lw * 0.5 - 2.0,
                my - FONT_EDGE * 0.5 - 1.0,
                lw + 4.0,
                FONT_EDGE + 2.0
            );
            let _ = write!(
                s,
                "  <text x=\"{mx:.2}\" y=\"{my:.2}\" text-anchor=\"middle\" \
                 dominant-baseline=\"central\" font-size=\"{FONT_EDGE:.2}\" fill=\"{LINE}\">{}</text>\n",
                escape(&e.label)
            );
        }
    }

    // Node labels — one <text> row per hard line break, the block centred on
    // the node, matching the canvas.
    for &id in &ids {
        let n = &g.nodes[&id];
        let font = font_for(g, id);
        let weight = if id == g.root_id {
            " font-weight=\"bold\""
        } else {
            ""
        };
        let lines = label_lines(&n.text);
        let count = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let dy = (i as f32 - (count as f32 - 1.0) * 0.5) * font * LINE_H;
            let _ = write!(
                s,
                "  <text x=\"{:.2}\" y=\"{:.2}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
                 font-size=\"{font:.2}\"{weight} fill=\"{INK}\">{}</text>\n",
                n.x,
                n.y + dy,
                escape(line)
            );
        }
    }

    s.push_str("</svg>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Direction, Graph};

    #[test]
    fn demo_svg_is_wellformed() {
        let g = Graph::demo();
        let svg = to_svg(&g);
        let root_text = escape(&g.nodes[&g.root_id].text);
        assert!(svg.starts_with("<svg"), "document opens with <svg");
        assert!(svg.ends_with("</svg>"), "and closes cleanly");
        assert!(svg.contains("<text"), "has at least one label");
        assert!(
            svg.contains(&format!(">{root_text}<")),
            "the root's label is rendered as text content"
        );
        assert!(svg.contains("<line"), "has at least one edge line");
    }

    #[test]
    fn multi_line_label_exports_each_line() {
        let g = Graph::new("first\nsecond");
        let svg = to_svg(&g);
        assert!(svg.contains(">first<"), "top line is its own text element");
        assert!(svg.contains(">second<"), "bottom line too");
        // The two lines are separate <text> rows, not one flattened string.
        assert!(!svg.contains("first\nsecond") && !svg.contains("first second"));
    }

    #[test]
    fn text_is_xml_escaped() {
        let g = Graph::new("a & b < c > d");
        let svg = to_svg(&g);
        assert!(
            svg.contains("a &amp; b &lt; c &gt; d"),
            "special chars are entity-escaped"
        );
        // None of the raw sequences survive inside the label.
        assert!(!svg.contains("a & b"), "no raw ampersand");
        assert!(!svg.contains("b < c"), "no raw less-than");
        assert!(!svg.contains("c > d"), "no raw greater-than");
    }

    #[test]
    fn single_node_viewbox_is_finite() {
        let svg = to_svg(&Graph::new("solo"));
        let start = svg.find("viewBox=\"").expect("has a viewBox") + "viewBox=\"".len();
        let end = svg[start..].find('"').expect("viewBox closes") + start;
        let nums: Vec<f32> = svg[start..end]
            .split_whitespace()
            .map(|t| t.parse::<f32>().expect("viewBox holds numbers"))
            .collect();
        assert_eq!(nums.len(), 4, "minx miny w h");
        assert!(nums.iter().all(|v| v.is_finite()), "no NaN/inf: {nums:?}");
        assert!(nums[2] > 0.0 && nums[3] > 0.0, "positive extent");
        assert!(svg.contains(">solo<"), "the lone label is drawn");
    }

    #[test]
    fn every_node_and_descendant_renders() {
        // This build has no `collapsed` field, so nothing is ever hidden: a
        // child and its grandchild both reach the SVG. (If folding is added
        // later, this is where a hidden-descendant assertion would live.)
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        g.focus(child);
        g.create_from_focused(Direction::Right, "grandchild").unwrap();
        let svg = to_svg(&g);
        assert!(svg.contains(">root<"));
        assert!(svg.contains(">child<"));
        assert!(svg.contains(">grandchild<"));
    }
}
