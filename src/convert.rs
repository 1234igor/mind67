//! Pure conversions between a `Graph` and text formats: Markdown, OPML, and a
//! plain indented outline. No GPUI, no I/O — just `&Graph` in, `String` out and
//! `&str` in, `Graph` out. Import builds the graph through the same path the app
//! uses (`Graph::new` for the root, then `create_child` for every descendant),
//! matching `Graph::demo`, so the result is a first-class map, not a hand-poked
//! struct.
//!
//! The exported tree is the parent/child hierarchy rooted at `root_id`. Only
//! nodes reachable from the root are emitted; a free-floating node (possible
//! only in a hand-edited save file) is skipped rather than guessed at.
//!
//! No new dependency: OPML is parsed with a small hand-rolled scanner rather
//! than pulling in an XML crate, which is plenty for the flat `<outline>` nesting
//! OPML uses.

use std::cmp::Ordering;

use crate::graph::{Direction, Graph};

// ---- ordering -----------------------------------------------------------

/// Children of `parent` in a STABLE, deterministic order: by `y`, then `x`,
/// then id. `Graph::children_of` orders by id alone; for export we want the
/// on-screen top-to-bottom, left-to-right reading order, and a total tiebreak
/// so the output never depends on `HashMap` iteration order.
fn ordered_children(g: &Graph, parent: u64) -> Vec<u64> {
    let mut kids = g.children_of(parent);
    kids.sort_by(|&a, &b| {
        let na = &g.nodes[&a];
        let nb = &g.nodes[&b];
        na.y
            .partial_cmp(&nb.y)
            .unwrap_or(Ordering::Equal)
            .then(na.x.partial_cmp(&nb.x).unwrap_or(Ordering::Equal))
            .then(a.cmp(&b))
    });
    kids
}

// ---- export -------------------------------------------------------------

/// Root as an H1, descendants as a nested `- ` bullet list, 2 spaces of indent
/// per depth level. Text is emitted verbatim (these are short labels).
pub fn to_markdown(g: &Graph) -> String {
    let mut out = String::new();
    md_node(g, g.root_id, 0, &mut out);
    out
}

fn md_node(g: &Graph, id: u64, depth: usize, out: &mut String) {
    let text = &g.nodes[&id].text;
    if depth == 0 {
        out.push_str("# ");
        out.push_str(text);
        out.push('\n');
    } else {
        for _ in 0..(depth - 1) {
            out.push_str("  ");
        }
        out.push_str("- ");
        out.push_str(text);
        out.push('\n');
    }
    for k in ordered_children(g, id) {
        md_node(g, k, depth + 1, out);
    }
}

/// Valid OPML 2.0 with nested `<outline text="...">`. The text attribute is
/// XML-escaped.
pub fn to_opml(g: &Graph) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<opml version=\"2.0\">\n");
    out.push_str("  <head>\n");
    out.push_str("    <title>");
    out.push_str(&xml_escape(&g.nodes[&g.root_id].text));
    out.push_str("</title>\n");
    out.push_str("  </head>\n");
    out.push_str("  <body>\n");
    opml_node(g, g.root_id, 2, &mut out);
    out.push_str("  </body>\n");
    out.push_str("</opml>\n");
    out
}

fn opml_node(g: &Graph, id: u64, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let esc = xml_escape(&g.nodes[&id].text);
    let kids = ordered_children(g, id);
    if kids.is_empty() {
        out.push_str(&indent);
        out.push_str("<outline text=\"");
        out.push_str(&esc);
        out.push_str("\"/>\n");
    } else {
        out.push_str(&indent);
        out.push_str("<outline text=\"");
        out.push_str(&esc);
        out.push_str("\">\n");
        for k in kids {
            opml_node(g, k, depth + 1, out);
        }
        out.push_str(&indent);
        out.push_str("</outline>\n");
    }
}

/// Plain text, one node per line, 2 spaces of indent per depth. Root at col 0.
pub fn to_outline(g: &Graph) -> String {
    let mut out = String::new();
    outline_node(g, g.root_id, 0, &mut out);
    out
}

fn outline_node(g: &Graph, id: u64, depth: usize, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&g.nodes[&id].text);
    out.push('\n');
    for k in ordered_children(g, id) {
        outline_node(g, k, depth + 1, out);
    }
}

// ---- import: markdown ---------------------------------------------------

/// Parse an optional H1 plus a nested bullet list back into a `Graph`.
///
/// The H1 (`# text`) names the root. With no H1 the root is created titled ""
/// (empty) and the top-level bullets hang off it — a bullet list has no single
/// natural root, and an empty anchor is less surprising than promoting an
/// arbitrary first item. Indentation is 2 spaces per level; a deeper-than-
/// expected jump just attaches to the nearest shallower node.
pub fn from_markdown(s: &str) -> Graph {
    let mut root_text: Option<String> = None;
    // (level, text) for each bullet, in document order.
    let mut items: Vec<(usize, String)> = Vec::new();

    for raw in s.lines() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            // First heading is the root title; later headings are ignored.
            if root_text.is_none() {
                root_text = Some(rest.trim_start_matches('#').trim().to_string());
            }
            continue;
        }
        if trimmed.starts_with('-') || trimmed.starts_with('*') {
            let indent = raw.len() - trimmed.len();
            let text = trimmed
                .trim_start_matches(['-', '*'])
                .trim()
                .to_string();
            items.push((indent / 2, text));
        }
    }

    let mut g = Graph::new(&root_text.unwrap_or_default());
    let root = g.root_id;
    // Stack of (level, id); the nearest entry with a smaller level is the parent.
    let mut stack: Vec<(usize, u64)> = Vec::new();
    for (level, text) in items {
        while stack.last().map(|&(l, _)| l >= level).unwrap_or(false) {
            stack.pop();
        }
        let parent = stack.last().map(|&(_, id)| id).unwrap_or(root);
        let id = g
            .create_child(parent, Direction::Right, &text)
            .expect("parent exists");
        stack.push((level, id));
    }

    // `create_child` places each node beside the one before it and nudges
    // whatever it collides with out of the way, which over a whole imported
    // outline adds up to a pile rather than a map. Lay the finished tree out
    // once, the same way the app does after any structural change.
    g.tidy();
    // create_child leaves the map mid-edit on the last child; hand it back the
    // way demo() does — parked in Browse on the root.
    g.leave_edit();
    g.focus(root);
    g
}

// ---- import: OPML -------------------------------------------------------

/// A parsed `<outline>` subtree, before it becomes graph nodes.
#[derive(Default)]
struct Outline {
    text: String,
    children: Vec<Outline>,
}

/// Parse nested `<outline text="...">` into a `Graph`.
///
/// A single top-level outline becomes the root. Multiple top-level outlines
/// (no single wrapper) get an empty "" root, matching `from_markdown`.
pub fn from_opml(s: &str) -> Graph {
    let tops = parse_outlines(s);

    let (root_text, top_children): (String, Vec<Outline>) = match tops.len() {
        1 => {
            let mut it = tops;
            let only = it.pop().unwrap();
            (only.text, only.children)
        }
        // 0 => empty map; >1 => empty root wrapping the top-level items.
        _ => (String::new(), tops),
    };

    let mut g = Graph::new(&root_text);
    let root = g.root_id;
    for child in &top_children {
        add_outline(&mut g, root, child);
    }
    // Same as the Markdown importer: the tree is built by repeated insertion,
    // so it needs one layout pass before it is a map.
    g.tidy();
    g.leave_edit();
    g.focus(root);
    g
}

fn add_outline(g: &mut Graph, parent: u64, o: &Outline) {
    let id = g
        .create_child(parent, Direction::Right, &o.text)
        .expect("parent exists");
    for c in &o.children {
        add_outline(g, id, c);
    }
}

/// Hand-rolled scan for `<outline>` nesting. Returns the top-level outlines.
/// Non-outline tags (`opml`, `head`, `body`, `title`, …) are skipped.
fn parse_outlines(s: &str) -> Vec<Outline> {
    // A sentinel at the bottom collects the top-level outlines.
    let mut stack: Vec<Outline> = vec![Outline::default()];
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            // Safe: '<' (0x3C) never appears in a UTF-8 continuation byte, so
            // stepping one byte at a time only ever lands on '<' at a boundary.
            i += 1;
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("</outline") {
            if stack.len() > 1 {
                let done = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(done);
            }
            i += rest.find('>').map(|p| p + 1).unwrap_or(1);
        } else if rest.starts_with("<outline") {
            // No closing '>' means a truncated tag — stop rather than slice at a
            // byte that may fall inside a multi-byte char (which would panic).
            let Some(rel) = rest.find('>') else { break };
            let tag = &rest[..rel];
            let self_closing = tag.trim_end().ends_with('/');
            let text = extract_text_attr(tag);
            if self_closing {
                stack
                    .last_mut()
                    .unwrap()
                    .children
                    .push(Outline { text, children: Vec::new() });
            } else {
                stack.push(Outline { text, children: Vec::new() });
            }
            i += rel + 1;
        } else {
            // Some other tag — jump past it.
            i += rest.find('>').map(|p| p + 1).unwrap_or(1);
        }
    }
    stack.remove(0).children
}

/// Pull the (unescaped) `text="..."` / `text='...'` attribute out of an opening
/// tag. Our own export only writes a `text` attribute, so a plain substring
/// search is enough.
fn extract_text_attr(tag: &str) -> String {
    let Some(idx) = tag.find("text") else {
        return String::new();
    };
    let rest = tag[idx + 4..].trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
        return String::new();
    };
    let rest = rest.trim_start();
    let Some(quote) = rest.chars().next() else {
        return String::new();
    };
    if quote != '"' && quote != '\'' {
        return String::new();
    }
    let after = &rest[quote.len_utf8()..];
    match after.find(quote) {
        Some(end) => xml_unescape(&after[..end]),
        None => String::new(),
    }
}

// ---- XML escaping -------------------------------------------------------

/// Escape the five characters that cannot sit raw in a double-quoted XML
/// attribute. `&` first so it does not double-escape the entities that follow.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Inverse of `xml_escape`. `&amp;` is undone last for the same reason.
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical `(depth, text)` traversal in the same deterministic order the
    /// exporters use. Two graphs with the same tree shape and labels produce the
    /// same vector, regardless of ids or exact coordinates.
    fn traverse(g: &Graph) -> Vec<(usize, String)> {
        fn go(g: &Graph, id: u64, depth: usize, out: &mut Vec<(usize, String)>) {
            out.push((depth, g.nodes[&id].text.clone()));
            for k in ordered_children(g, id) {
                go(g, k, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        go(g, g.root_id, 0, &mut out);
        out
    }

    /// The same traversal with siblings sorted by label, so it compares the
    /// *tree* rather than the order the tree happens to be drawn in.
    ///
    /// A map has no sibling order of its own: order is read off the geometry,
    /// and `tidy` reorders a root's branches to balance them across the two
    /// sides. So a round trip through an ordered format preserves the
    /// hierarchy and the labels, not the sequence. Anything asserting the
    /// sequence survives a round trip is asserting a coincidence.
    fn shape(g: &Graph) -> Vec<(usize, String)> {
        fn go(g: &Graph, id: u64, depth: usize, out: &mut Vec<(usize, String)>) {
            out.push((depth, g.nodes[&id].text.clone()));
            let mut kids = ordered_children(g, id);
            kids.sort_by(|&a, &b| g.nodes[&a].text.cmp(&g.nodes[&b].text));
            for k in kids {
                go(g, k, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        go(g, g.root_id, 0, &mut out);
        out
    }

    #[test]
    fn markdown_round_trips_the_demo() {
        let g = Graph::demo();
        let g2 = from_markdown(&to_markdown(&g));
        assert_eq!(shape(&g), shape(&g2));
    }

    #[test]
    fn a_second_round_trip_changes_nothing() {
        // The first trip through a text format loses the sibling sequence,
        // because the map keeps no sibling order and `tidy` re-balances the
        // root's branches. What must not happen is drift: once a map has been
        // through the importer, exporting and re-importing it has to land in
        // exactly the same place, or every save/open cycle would reshuffle it.
        let once = from_markdown(&to_markdown(&Graph::demo()));
        let text = to_markdown(&once);
        assert_eq!(text, to_markdown(&from_markdown(&text)));

        let once = from_opml(&to_opml(&Graph::demo()));
        let text = to_opml(&once);
        assert_eq!(text, to_opml(&from_opml(&text)));
    }

    #[test]
    fn imported_nodes_are_laid_out_not_piled_up() {
        // A tree built by repeated insertion collides with itself; the importer
        // runs a layout pass so the result is a readable map. Every node must
        // sit clear of its siblings, which a pile does not.
        let g = from_markdown(&to_markdown(&Graph::demo()));
        let mut seen: Vec<(u64, f32, f32)> = Vec::new();
        for (&id, n) in &g.nodes {
            for &(other, x, y) in &seen {
                let (dx, dy) = ((n.x - x).abs(), (n.y - y).abs());
                assert!(
                    dx > 1.0 || dy > 1.0,
                    "{id} and {other} landed on the same spot: ({}, {})",
                    n.x,
                    n.y
                );
            }
            seen.push((id, n.x, n.y));
        }
        // And the map has both sides, rather than one long ribbon to the right.
        let kids = ordered_children(&g, g.root_id);
        assert!(kids.iter().any(|k| g.nodes[k].x < 0.0), "nothing on the left");
        assert!(kids.iter().any(|k| g.nodes[k].x > 0.0), "nothing on the right");
    }

    #[test]
    fn opml_round_trips_the_demo() {
        let g = Graph::demo();
        let g2 = from_opml(&to_opml(&g));
        assert_eq!(shape(&g), shape(&g2));
    }

    #[test]
    fn opml_escapes_and_unescapes_special_chars() {
        let mut g = Graph::new("root & <tag>");
        let root = g.root_id;
        g.create_child(root, Direction::Right, "quote \"here\" & <b>")
            .unwrap();

        let xml = to_opml(&g);
        // Raw specials must not appear inside the escaped attribute payload.
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&quot;"));
        assert!(!xml.contains("<tag>"));

        let back = from_opml(&xml);
        assert_eq!(shape(&g), shape(&back));
        assert_eq!(back.nodes[&back.root_id].text, "root & <tag>");
        let kid = ordered_children(&back, back.root_id)[0];
        assert_eq!(back.nodes[&kid].text, "quote \"here\" & <b>");
    }

    #[test]
    fn markdown_snippet_imports_to_the_expected_tree() {
        let md = "\
# Project
- alpha
  - alpha one
  - alpha two
- beta
";
        let g = from_markdown(md);
        assert_eq!(
            shape(&g),
            vec![
                (0, "Project".to_string()),
                (1, "alpha".to_string()),
                (2, "alpha one".to_string()),
                (2, "alpha two".to_string()),
                (1, "beta".to_string()),
            ]
        );
    }

    #[test]
    fn markdown_without_h1_gets_an_empty_root() {
        let g = from_markdown("- lonely\n- pair\n");
        assert_eq!(g.nodes[&g.root_id].text, "");
        assert_eq!(
            shape(&g),
            vec![
                (0, "".to_string()),
                (1, "lonely".to_string()),
                (1, "pair".to_string()),
            ]
        );
    }

    #[test]
    fn opml_with_multiple_top_level_items_gets_an_empty_root() {
        let opml = "\
<opml version=\"2.0\"><body>
  <outline text=\"one\"/>
  <outline text=\"two\"><outline text=\"two-a\"/></outline>
</body></opml>";
        let g = from_opml(opml);
        assert_eq!(
            shape(&g),
            vec![
                (0, "".to_string()),
                (1, "one".to_string()),
                (1, "two".to_string()),
                (2, "two-a".to_string()),
            ]
        );
    }

    #[test]
    fn truncated_multibyte_opml_does_not_panic() {
        // A tag with no closing '>' that ends inside a multi-byte character
        // must not slice at a non-boundary byte.
        let _ = from_opml("<opml><body><outline text=\"café");
        let _ = from_opml("<outline text=\"naïve");
        // A well-formed one still parses.
        let g = from_opml("<outline text=\"café\"/>");
        assert!(g.nodes.values().any(|n| n.text == "café"));
    }

    #[test]
    fn output_ordering_is_deterministic_and_stable() {
        let g = Graph::demo();
        // Same graph, repeated calls: byte-for-byte identical.
        assert_eq!(to_outline(&g), to_outline(&g));
        assert_eq!(to_markdown(&g), to_markdown(&g));
        assert_eq!(to_opml(&g), to_opml(&g));

        // A known small tree exports in y-order, root at column 0.
        let mut h = Graph::new("root");
        let r = h.root_id;
        // Placed out of id-order on the y axis to prove the sort is spatial.
        let a = h.create_child(r, Direction::Right, "a").unwrap();
        let b = h.create_child(r, Direction::Right, "b").unwrap();
        // Place them out of id-order on the y axis to prove the sort is spatial.
        h.nodes.get_mut(&a).unwrap().y = 50.0; // a lower on screen
        h.nodes.get_mut(&b).unwrap().y = -50.0; // b higher on screen
        assert_eq!(to_outline(&h), "root\n  b\n  a\n");
    }
}
