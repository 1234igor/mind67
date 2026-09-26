//! Pure mind-map graph — no GPUI. Unit tests drive these APIs only.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;

/// Layout constants used by placement + UI paint.
pub mod layout {
    pub const NODE_HALF_W: f32 = 48.0;
    /// Half-height of a node's ring — the click target, and the row pitch.
    pub const NODE_HALF_H: f32 = 13.0;
    pub const MIN_GAP: f32 = 28.0;
    pub const STEP_RIGHT: f32 = 160.0;
    pub const STEP_UP: f32 = 56.0;
    pub const STEP_DOWN: f32 = 56.0;
    /// Hit radius around edge midpoints for label editing.
    pub const EDGE_HIT_R: f32 = 18.0;
    /// Vertical slot height used by `Graph::tidy`.
    pub const TIDY_ROW: f32 = NODE_HALF_H * 2.0 + MIN_GAP;
    /// Horizontal distance between tree depths used by `Graph::tidy`.
    pub const TIDY_COL: f32 = 210.0;
    /// Grid pitch that ⌥-drag and ⌥-nudge snap to.
    pub const SNAP: f32 = 20.0;
}

/// Recursion cap for `Graph::tidy`. Far past any hand-built tree; it exists so
/// a corrupt file cannot turn a re-layout into a stack overflow.
const MAX_TIDY_DEPTH: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Right,
    Left,
    Up,
    Down,
}

impl Direction {
    pub fn offset(self) -> (f32, f32) {
        match self {
            Direction::Right => (layout::STEP_RIGHT, 0.0),
            Direction::Left => (-layout::STEP_RIGHT, 0.0),
            Direction::Up => (0.0, -layout::STEP_UP),
            Direction::Down => (0.0, layout::STEP_DOWN),
        }
    }
}

/// A node has no id of its own: the key it is stored under *is* its identity.
/// Carrying both invites them to disagree, which a hand-edited file will
/// eventually do, and every lookup goes through the key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub parent: Option<u64>,
    /// Folded: this node's descendants are not drawn, not hit-tested, not
    /// walked by the arrows and not laid out. A property of the document, so
    /// it is saved — reopening a big map with everything unfolded would undo
    /// the only thing that made it readable.
    #[serde(default)]
    pub collapsed: bool,
    /// A colour tag, 1..=6, or `None` for the plain ink default. Saved with the
    /// document — a colour is a property of the map, not of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
    /// The picture this node shows instead of its text, if any.
    ///
    /// Behind an `Arc` on purpose. Undo records whole-graph clones two hundred
    /// deep (`history.rs`), so a plain `Option<NodeImage>` would add its bytes
    /// to *every* node on *every* snapshot — including the maps that contain no
    /// images at all — and heap-allocate the name once per image node per
    /// snapshot. An `Arc` is eight bytes idle and a refcount bump when cloned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<Arc<NodeImage>>,
}

/// The bounds a stored image box is trusted within. Shared with the resize
/// gesture in `ui.rs`, which clamps to the same numbers.
pub const MIN_IMAGE_BOX: f32 = 24.0;
pub const MAX_IMAGE_BOX: f32 = 4000.0;

/// A node's picture: which file in the store, and how big it is drawn.
///
/// The size is in world units, so an image keeps its place on the canvas as you
/// zoom, exactly like a label does. It is stored rather than derived because it
/// is a decision the reader made by dragging, not a property of the file — and
/// because the extent has to be known before the image has been decoded, or a
/// map full of photos would reflow as it loaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeImage {
    /// The file name in the store. `Arc<str>` for the same reason as above.
    pub name: Arc<str>,
    pub w: f32,
    pub h: f32,
}

impl NodeImage {
    /// Width over height, never zero or negative however the file was edited.
    pub fn aspect(&self) -> f32 {
        if self.h > 0.0 && self.w > 0.0 {
            self.w / self.h
        } else {
            1.0
        }
    }
}

/// Likewise keyed by its id in `Graph::edges`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: u64,
    pub to: u64,
    pub label: String,
}

/// What the user is currently editing / navigating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Node(u64),
    Edge(u64),
}

/// Whether the arrow keys move through the map or through the text.
///
/// Without this the two meanings collide: arrows cannot both walk the map and
/// walk the caret. Browse is the resting state; typing drops into Edit, and
/// Enter or Escape comes back out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Browse,
    Edit,
}

/// Nodes lifted out of a map, ready to be dropped back into one. Carries no
/// ids: parents are indices into `nodes` and positions are relative to the
/// clip's own top-left, so the same clip pastes anywhere, any number of times.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    nodes: Vec<ClipNode>,
}

impl Clip {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Round-trips through the system clipboard as text, so a copied branch can
    /// be pasted into another window — or read by a human.
    pub fn to_text(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_text(s: &str) -> Option<Clip> {
        let clip: Clip = serde_json::from_str(s).ok()?;
        (!clip.nodes.is_empty()).then_some(clip)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipNode {
    text: String,
    dx: f32,
    dy: f32,
    /// Index into `Clip::nodes`, or `None` for a node the clip does not own the
    /// parent of — those attach wherever the clip is pasted.
    parent: Option<usize>,
    /// Label on the edge coming in from `parent`.
    #[serde(default)]
    label: String,
    /// Folded state travels with the clip; a branch copied folded should
    /// arrive folded, not explode into the map.
    #[serde(default)]
    collapsed: bool,
    /// A colour tag travels with the clip too, so a copied branch keeps its
    /// colours when pasted.
    #[serde(default)]
    color: Option<u8>,
    /// So does a picture. The clip carries the *reference*, never the pixels:
    /// the store is content-addressed and app-wide, so the name still resolves
    /// in whichever document the branch is pasted into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<Arc<NodeImage>>,
}

/// Everything but `nodes` defaults, so a hand-edited file that drops a
/// bookkeeping field still loads — `sanitize` derives them from the nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    pub nodes: HashMap<u64, Node>,
    #[serde(default)]
    pub edges: HashMap<u64, Edge>,
    #[serde(default)]
    pub root_id: u64,
    #[serde(default)]
    pub focused_id: u64,
    /// When set, typing edits the edge label instead of the node text.
    #[serde(default)]
    pub focused_edge: Option<u64>,
    /// Browse (arrows walk the map) or Edit (arrows walk the caret). Not
    /// saved: a document always opens in Browse rather than mid-edit.
    #[serde(skip)]
    pub mode: Mode,
    /// Caret position in the active text, as a byte offset. Always on a char
    /// boundary and always within the text it points into. Not saved.
    #[serde(skip)]
    caret: usize,
    /// The fixed end of the selection, when there is one; `None` means the
    /// caret stands alone. Held as an anchor rather than a range so ⇧-arrow
    /// keeps pivoting about the place the selection was started from.
    #[serde(skip)]
    anchor: Option<usize>,
    /// Nodes picked out for a bulk move or delete. May be empty, which means
    /// "just the node you are on" — see [`Graph::acting_on`]. Ordered so that
    /// a bulk operation is deterministic. Not saved: a selection is a thing
    /// you are holding, not a property of the document.
    #[serde(skip)]
    selected: BTreeSet<u64>,
}

impl Graph {
    pub fn new(root_text: &str) -> Self {
        let id = 1;
        let mut nodes = HashMap::new();
        nodes.insert(
            id,
            Node {
                text: root_text.to_string(),
                x: 0.0,
                y: 0.0,
                parent: None,
                collapsed: false,
                color: None,
                image: None,
            },
        );
        Self {
            next_id: 2,
            nodes,
            edges: HashMap::new(),
            root_id: id,
            focused_id: id,
            focused_edge: None,
            mode: Mode::Browse,
            caret: root_text.len(),
            anchor: None,
            // A map opens holding its root, so ⌫ and drag have a subject and
            // the first thing you see is what the arrows will move.
            selected: BTreeSet::from([id]),
        }
    }

    // ---- node selection --------------------------------------------------
    //
    // Two selections live in this type and they must not be confused: the text
    // selection inside one label (`anchor`), and this one, over nodes. Every
    // name here says `node`.

    /// The nodes a bulk operation acts on. **Empty means empty** — nothing is
    /// selected and delete, drag and nudge must all do nothing. That state has
    /// to be representable: clicking empty ground has to be able to leave you
    /// holding nothing, which is the first thing anyone tries on a canvas.
    ///
    /// `focused_id` is a separate idea — the keyboard cursor, always valid, so
    /// the arrows have somewhere to start from even with nothing selected.
    pub fn acting_on(&self) -> Vec<u64> {
        self.selected.iter().copied().collect()
    }

    /// Put the keyboard cursor somewhere without touching the selection — for
    /// undoing a sweep that ended up selecting nothing.
    pub fn set_cursor(&mut self, id: u64) {
        if self.nodes.contains_key(&id) {
            self.focused_id = id;
            self.caret_to_end();
        }
    }

    /// Let go of everything. The cursor stays where it is.
    pub fn deselect_all(&mut self) -> bool {
        let had = !self.selected.is_empty();
        self.selected.clear();
        had
    }

    pub fn selected_nodes(&self) -> &BTreeSet<u64> {
        &self.selected
    }

    pub fn is_node_selected(&self, id: u64) -> bool {
        self.selected.contains(&id)
    }

    /// Add or remove one node — ⇧-click. Removing the cursor's node hands the
    /// cursor to another member, so the arrows never start from nowhere.
    pub fn toggle_node_selected(&mut self, id: u64) {
        if !self.nodes.contains_key(&id) {
            return;
        }
        if self.selected.remove(&id) {
            if id == self.focused_id {
                if let Some(&next) = self.selected.iter().next() {
                    self.focus_keeping_selection(next);
                }
            }
        } else {
            self.selected.insert(id);
            self.focus_keeping_selection(id);
        }
    }

    /// Replace the whole selection — what a marquee lands on. An empty `ids`
    /// really does mean nothing selected.
    pub fn set_node_selection(&mut self, ids: impl IntoIterator<Item = u64>) {
        self.selected = ids
            .into_iter()
            .filter(|id| self.nodes.contains_key(id))
            .collect();
        if !self.selected.is_empty() && !self.selected.contains(&self.focused_id) {
            if let Some(&first) = self.selected.iter().next() {
                self.focus_keeping_selection(first);
            }
        }
    }

    /// Add every node in `id`'s subtree to the selection.
    pub fn select_subtree_of(&mut self, id: u64) {
        let ids = self.subtree_ids(id);
        if ids.is_empty() {
            return;
        }
        self.selected.extend(ids);
        self.focus_keeping_selection(id);
    }

    /// The nodes sharing `id`'s parent, `id` included. Top-level nodes count the
    /// other roots as their siblings.
    pub fn siblings_of(&self, id: u64) -> Vec<u64> {
        let parent = self.nodes.get(&id).and_then(|n| n.parent);
        self.nodes
            .iter()
            .filter(|(_, n)| n.parent == parent)
            .map(|(&i, _)| i)
            .collect()
    }

    /// Add every sibling of `id` to the selection.
    pub fn select_siblings_of(&mut self, id: u64) {
        let sibs = self.siblings_of(id);
        if sibs.len() < 2 {
            return;
        }
        self.selected.extend(sibs);
        self.focus_keeping_selection(id);
    }

    /// Move focus without disturbing the selection — the internal half of
    /// `focus`, which a plain click uses to reset both at once.
    fn focus_keeping_selection(&mut self, id: u64) {
        if self.nodes.contains_key(&id) {
            self.focused_id = id;
            self.focused_edge = None;
            self.caret_to_end();
        }
    }

    /// Tag every acting-on node with a colour (1..=6), or clear it with `None`.
    /// Returns true if anything changed. Acts on the selection.
    pub fn set_color_of_selection(&mut self, color: Option<u8>) -> bool {
        let ids = self.acting_on();
        let mut changed = false;
        for id in ids {
            if let Some(n) = self.nodes.get_mut(&id) {
                if n.color != color {
                    n.color = color;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Move everything in the selection by the same delta. There is no subtree
    /// special case: what moves is exactly what is selected, so "drag the whole
    /// branch" is "select the branch, then drag" rather than a hidden modifier.
    pub fn move_selection_by(&mut self, dx: f32, dy: f32) -> bool {
        self.move_nodes_by(&self.acting_on(), dx, dy)
    }

    /// Move exactly these nodes. A drag captures its set at press time and
    /// passes it here every event: recomputing the set mid-gesture means that
    /// anything which re-points the cursor — an arrow key, a paste — silently
    /// changes what the pointer is dragging.
    pub fn move_nodes_by(&mut self, ids: &[u64], dx: f32, dy: f32) -> bool {
        // A folded node stands for its whole branch, and only the folded node
        // itself can be clicked or swept — so moving it has to carry what it
        // is hiding, or unfolding later reveals a branch stranded where the
        // node used to be.
        let mut all: Vec<u64> = ids.to_vec();
        for &id in ids {
            if self.is_collapsed(id) {
                all.extend(self.subtree_ids(id).into_iter().filter(|&k| k != id));
            }
        }
        all.sort_unstable();
        all.dedup();
        let mut moved = false;
        for &id in &all {
            if let Some(n) = self.nodes.get_mut(&id) {
                n.x += dx;
                n.y += dy;
                moved = true;
            }
        }
        moved
    }

    /// Delete every selected node and its subtree. Returns how many nodes went.
    /// The root is never one of them.
    pub fn delete_selection_of_nodes(&mut self) -> usize {
        let ids = self.acting_on();
        // The cursor's ancestry, captured before anything is removed: if the
        // focused node is about to be deleted, the cursor lands on its nearest
        // surviving ancestor so ⇥, the arrows and paste keep working instead of
        // pointing at a node that no longer exists.
        let ancestry: Vec<u64> = {
            let mut chain = Vec::new();
            let mut cur = self.nodes.get(&self.focused_id).and_then(|n| n.parent);
            let mut hops = 0;
            while let Some(p) = cur {
                chain.push(p);
                hops += 1;
                if hops > MAX_TIDY_DEPTH {
                    break;
                }
                cur = self.nodes.get(&p).and_then(|n| n.parent);
            }
            chain
        };
        // Count first: deleting a parent takes its children with it, so the
        // per-call return values would double-count.
        let mut doomed: HashSet<u64> = HashSet::new();
        for &id in &ids {
            if id == self.root_id {
                continue;
            }
            doomed.extend(self.subtree_ids(id));
        }
        doomed.remove(&self.root_id);
        let n = doomed.len();
        for &id in &ids {
            if self.nodes.contains_key(&id) {
                self.delete_subtree(id);
            }
        }
        self.selected.clear();
        // Rescue the cursor if it went down with the deletion.
        if !self.nodes.contains_key(&self.focused_id) {
            self.focused_id = ancestry
                .into_iter()
                .find(|p| self.nodes.contains_key(p))
                .unwrap_or(self.root_id);
            self.focused_edge = None;
            self.caret_to_end();
        }
        n
    }

    /// Every node whose ring meets the rectangle — the marquee's answer.
    pub fn nodes_in_rect(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        half: impl Fn(u64, &Node) -> (f32, f32),
    ) -> Vec<u64> {
        self.nodes_in_rect_masked(x0, y0, x1, y1, &self.hidden_set(), half)
    }

    /// The same, with the hidden set supplied — the marquee reuses the frame's.
    pub fn nodes_in_rect_masked(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        hidden: &HashSet<u64>,
        half: impl Fn(u64, &Node) -> (f32, f32),
    ) -> Vec<u64> {
        let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
        let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
        let mut out: Vec<u64> = self
            .nodes
            .iter()
            .filter(|(&id, n)| {
                if hidden.contains(&id) {
                    return false;
                }
                let (hw, hh) = half(id, n);
                n.x + hw >= lo_x && n.x - hw <= hi_x && n.y + hh >= lo_y && n.y - hh <= hi_y
            })
            .map(|(&id, _)| id)
            .collect();
        out.sort_unstable();
        out
    }

    pub fn selection(&self) -> Selection {
        if let Some(eid) = self.focused_edge {
            if self.edges.contains_key(&eid) {
                return Selection::Edge(eid);
            }
        }
        Selection::Node(self.focused_id)
    }

    pub fn focused(&self) -> &Node {
        self.nodes
            .get(&self.focused_id)
            .or_else(|| self.nodes.get(&self.root_id))
            .expect("graph always has root")
    }

    /// Text currently being edited (node text or edge label).
    pub fn active_text(&self) -> &str {
        match self.selection() {
            Selection::Edge(eid) => self
                .edges
                .get(&eid)
                .map(|e| e.label.as_str())
                .unwrap_or(""),
            Selection::Node(_) => self
                .nodes
                .get(&self.focused_id)
                .map(|n| n.text.as_str())
                .unwrap_or(""),
        }
    }

    /// Mutable handle on whatever the caret is currently pointing into.
    fn active_text_mut(&mut self) -> Option<&mut String> {
        match self.selection() {
            Selection::Edge(eid) => self.edges.get_mut(&eid).map(|e| &mut e.label),
            Selection::Node(_) => self.nodes.get_mut(&self.focused_id).map(|n| &mut n.text),
        }
    }

    pub fn is_editing(&self) -> bool {
        self.mode == Mode::Edit
    }

    /// Enter text editing with the caret at the end of the current text.
    pub fn enter_edit(&mut self) {
        self.mode = Mode::Edit;
        self.caret_to_end();
    }

    pub fn leave_edit(&mut self) {
        self.mode = Mode::Browse;
        // A highlight left painted behind a node you are only *looking* at
        // would claim the arrows still edit it.
        self.anchor = None;
    }

    /// Park the caret after the active text with nothing selected. What every
    /// gesture that changes *which* text is active wants.
    fn caret_to_end(&mut self) {
        self.caret = self.active_text().len();
        self.anchor = None;
    }

    pub fn toggle_edit(&mut self) {
        if self.is_editing() {
            self.leave_edit();
        } else {
            self.enter_edit();
        }
    }

    pub fn caret(&self) -> usize {
        self.caret.min(self.active_text().len())
    }

    /// Keep the caret inside the text and on a char boundary. Called after
    /// anything that can change which text is active or how long it is.
    fn clamp_caret(&mut self) {
        let len = self.active_text().len();
        if self.caret > len {
            self.caret = len;
            return;
        }
        // Walk back to a boundary rather than panicking on a split codepoint.
        while self.caret > 0 && !self.active_text().is_char_boundary(self.caret) {
            self.caret -= 1;
        }
    }

    pub fn set_focused_text(&mut self, text: String) {
        if !self.is_editing() {
            return;
        }
        let caret = text.len();
        if let Some(t) = self.active_text_mut() {
            *t = text;
        }
        self.caret = caret;
        self.anchor = None;
    }

    // ---- selection ------------------------------------------------------

    /// The selected byte range, or `None` when the caret stands alone. Ordered,
    /// so callers never have to work out which end the anchor is.
    pub fn selected_range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        let (a, b) = (anchor.min(self.caret), anchor.max(self.caret));
        (a != b).then_some((a, b))
    }

    pub fn selected_text(&self) -> Option<&str> {
        let (a, b) = self.selected_range()?;
        self.active_text().get(a..b)
    }

    pub fn select_all(&mut self) -> bool {
        let len = self.active_text().len();
        if len == 0 {
            return false;
        }
        let changed = self.anchor != Some(0) || self.caret != len;
        self.anchor = Some(0);
        self.caret = len;
        changed
    }

    /// Select the word `at` falls in — the double-click gesture.
    pub fn select_word_at(&mut self, at: usize) -> bool {
        let (a, b) = self.active_text().word_at(at);
        if a == b {
            return false;
        }
        self.anchor = Some(a);
        self.caret = b;
        true
    }

    /// Remove the selection from the text, leaving the caret where it was.
    /// The one place any editing operation is allowed to consume a selection.
    pub fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selected_range() else {
            return false;
        };
        if let Some(t) = self.active_text_mut() {
            t.replace_range(a..b, "");
        }
        self.caret = a;
        self.anchor = None;
        true
    }

    /// Move the caret to `at`. `extend` drags the selection along with it;
    /// otherwise the selection collapses — the ⇧ bit is the whole difference
    /// between the two families of motion, so it lives in one place.
    /// Returns whether caret *or* selection changed, which is what callers
    /// actually want to know: collapsing a selection in place is a change.
    fn place_caret(&mut self, at: usize, extend: bool) -> bool {
        let before = (self.caret, self.anchor);
        if extend {
            self.anchor.get_or_insert(self.caret);
        } else {
            self.anchor = None;
        }
        self.caret = at;
        // An anchor the caret has walked back onto is not a selection.
        if self.anchor == Some(self.caret) {
            self.anchor = None;
        }
        (self.caret, self.anchor) != before
    }

    /// Insert at the caret, not at the end. Typing over a selection replaces
    /// it, which is the behaviour that makes select-then-type worth having.
    pub fn insert_char(&mut self, c: char) {
        if !self.is_editing() {
            return;
        }
        self.clamp_caret();
        self.delete_selection();
        let at = self.caret;
        if let Some(t) = self.active_text_mut() {
            t.insert(at, c);
        }
        self.caret = at + c.len_utf8();
    }

    /// Insert a whole string at the caret — what paste lands on.
    pub fn insert_str(&mut self, s: &str) {
        if !self.is_editing() || s.is_empty() {
            return;
        }
        self.clamp_caret();
        self.delete_selection();
        let at = self.caret;
        if let Some(t) = self.active_text_mut() {
            t.insert_str(at, s);
        }
        self.caret = at + s.len();
    }

    /// Delete the grapheme cluster before the caret, so a flag emoji or a
    /// combining accent goes in one press instead of falling apart.
    pub fn backspace(&mut self) {
        if !self.is_editing() {
            return;
        }
        self.clamp_caret();
        if self.delete_selection() {
            return;
        }
        let at = self.caret;
        let Some(start) = self.active_text().grapheme_start_before(at) else {
            return;
        };
        if let Some(t) = self.active_text_mut() {
            t.replace_range(start..at, "");
        }
        self.caret = start;
    }

    /// Whether ⌫ would remove anything.
    pub fn can_backspace(&self) -> bool {
        self.selected_range().is_some() || self.caret() > 0
    }

    /// Whether ⌦ would remove anything. Callers check first so a no-op never
    /// pushes an undo step.
    pub fn can_delete_forward(&self) -> bool {
        self.selected_range().is_some() || self.caret() < self.active_text().len()
    }

    /// Whether ⌥⌫ would remove anything.
    pub fn can_delete_word_left(&self) -> bool {
        if self.selected_range().is_some() {
            return true;
        }
        let at = self.caret();
        self.active_text().word_start_before(at) != at
    }

    /// Delete the grapheme after the caret (forward delete).
    pub fn delete_forward(&mut self) {
        if !self.is_editing() {
            return;
        }
        self.clamp_caret();
        if self.delete_selection() {
            return;
        }
        let at = self.caret;
        let Some(end) = self.active_text().grapheme_end_after(at) else {
            return;
        };
        if let Some(t) = self.active_text_mut() {
            t.replace_range(at..end, "");
        }
    }

    /// Delete from the start of the previous word to the caret (⌥⌫).
    pub fn delete_word_left(&mut self) {
        if !self.is_editing() {
            return;
        }
        self.clamp_caret();
        if self.delete_selection() {
            return;
        }
        let at = self.caret;
        let start = self.active_text().word_start_before(at);
        if start == at {
            return;
        }
        if let Some(t) = self.active_text_mut() {
            t.replace_range(start..at, "");
        }
        self.caret = start;
    }

    // ---- caret movement -------------------------------------------------
    //
    // Every motion takes `extend`: false collapses the selection, true drags it.
    // ⇧ is the only difference between the two, so no motion is written twice.

    pub fn caret_left(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        // Unextended, ← steps off the left edge of a selection rather than one
        // character back from the caret — the rule that makes a selection feel
        // like a place, not just two offsets.
        if !extend {
            if let Some((a, _)) = self.selected_range() {
                return self.place_caret(a, false);
            }
        }
        match self.active_text().grapheme_start_before(self.caret) {
            Some(at) => self.place_caret(at, extend),
            None => self.place_caret(self.caret, extend),
        }
    }

    pub fn caret_right(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        if !extend {
            if let Some((_, b)) = self.selected_range() {
                return self.place_caret(b, false);
            }
        }
        match self.active_text().grapheme_end_after(self.caret) {
            Some(at) => self.place_caret(at, extend),
            None => self.place_caret(self.caret, extend),
        }
    }

    pub fn caret_word_left(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        let at = self.active_text().word_start_before(self.caret);
        self.place_caret(at, extend)
    }

    pub fn caret_word_right(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        let at = self.active_text().word_end_after(self.caret);
        self.place_caret(at, extend)
    }

    /// Put the caret at a byte offset that came from outside — the shaper's
    /// answer to "which glyph did that click land on". It reports glyph starts,
    /// which inside a flag or an accented cluster is not a place a caret may
    /// sit, so snap back to the start of the cluster it fell into.
    pub fn set_caret(&mut self, at: usize) -> bool {
        let at = self.snap_offset(at);
        self.place_caret(at, false)
    }

    /// The same, but dragging the selection along — a press-and-drag through
    /// the text rather than a click in it.
    pub fn extend_caret_to(&mut self, at: usize) -> bool {
        let at = self.snap_offset(at);
        self.place_caret(at, true)
    }

    fn snap_offset(&self, at: usize) -> usize {
        let text = self.active_text();
        let at = at.min(text.len());
        if at == text.len() {
            at
        } else {
            text.grapheme_start_at_or_before(at)
        }
    }

    /// Byte offset of the start of the line the caret is on — just past the
    /// previous hard break, or 0.
    fn line_start(&self, at: usize) -> usize {
        let t = self.active_text();
        let at = at.min(t.len());
        t[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }

    /// Byte offset of the end of the line the caret is on — the next hard break,
    /// or the end of the text.
    fn line_end(&self, at: usize) -> usize {
        let t = self.active_text();
        let at = at.min(t.len());
        t[at..].find('\n').map(|i| at + i).unwrap_or(t.len())
    }

    pub fn caret_home(&mut self, extend: bool) -> bool {
        let at = self.line_start(self.caret);
        self.place_caret(at, extend)
    }

    pub fn caret_end(&mut self, extend: bool) -> bool {
        let at = self.line_end(self.caret);
        self.place_caret(at, extend)
    }

    /// Move the caret to the line above, keeping roughly the same column. On the
    /// first line there is nowhere up to go, so it lands at the very start.
    pub fn caret_up(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        let ls = self.line_start(self.caret);
        if ls == 0 {
            return self.caret_home(extend);
        }
        let col = self.caret - ls;
        let prev_end = ls - 1; // the '\n' that ends the previous line
        let prev_start = self.line_start(prev_end);
        let target = self.snap_offset((prev_start + col).min(prev_end));
        self.place_caret(target, extend)
    }

    /// Move the caret to the line below, keeping roughly the same column. On the
    /// last line there is nowhere down to go, so it lands at the very end.
    pub fn caret_down(&mut self, extend: bool) -> bool {
        self.clamp_caret();
        let le = self.line_end(self.caret);
        if le == self.active_text().len() {
            return self.caret_end(extend);
        }
        let ls = self.line_start(self.caret);
        let col = self.caret - ls;
        let next_start = le + 1;
        let next_end = self.line_end(next_start);
        let target = self.snap_offset((next_start + col).min(next_end));
        self.place_caret(target, extend)
    }

    /// Go to one node and make it the whole selection — a plain click, and
    /// every keyboard navigation. Anything that wants to keep a multi-selection
    /// alive has to say so explicitly.
    pub fn focus(&mut self, id: u64) {
        if self.nodes.contains_key(&id) {
            self.selected.clear();
            self.selected.insert(id);
            self.focus_keeping_selection(id);
        }
    }

    /// Put the caret's attention on an edge label without opening it. Selecting
    /// and editing are two steps for a node, and they are two steps here too —
    /// a first click that silently entered Edit made the same click mean
    /// different things depending on what you had clicked before.
    pub fn focus_edge(&mut self, edge_id: u64) -> bool {
        if self.edges.contains_key(&edge_id) {
            self.selected.clear();
            self.focused_edge = Some(edge_id);
            // Keep node focus on the edge's child so create still makes sense.
            if let Some(to) = self.edges.get(&edge_id).map(|e| e.to) {
                if self.nodes.contains_key(&to) {
                    self.focused_id = to;
                }
            }
            self.caret_to_end();
            true
        } else {
            false
        }
    }

    /// Leave edge edit; keep node focus.
    /// Drop edge focus without grabbing the node behind it — for clicking away
    /// onto empty ground, where the answer is "nothing", not "that node".
    pub fn clear_edge_focus_only(&mut self) {
        self.focused_edge = None;
        self.caret_to_end();
    }

    pub fn clear_edge_focus(&mut self) {
        self.focused_edge = None;
        // Coming back off an edge lands you on its node, holding it.
        if self.nodes.contains_key(&self.focused_id) {
            self.selected.clear();
            self.selected.insert(self.focused_id);
        }
        self.caret_to_end();
    }

    /// Move focus to parent of the focused node. Returns true if moved.
    pub fn focus_parent(&mut self) -> bool {
        self.focused_edge = None;
        let parent = self.nodes.get(&self.focused_id).and_then(|n| n.parent);
        if let Some(pid) = parent {
            self.focus(pid);
            true
        } else {
            false
        }
    }

    /// Cycle focus among siblings (and self) under the same parent.
    /// `delta` = +1 next, -1 previous. Root has no siblings — no-op.
    pub fn focus_sibling(&mut self, delta: i32) -> bool {
        self.focused_edge = None;
        let parent = self.nodes.get(&self.focused_id).and_then(|n| n.parent);
        let Some(pid) = parent else {
            return false;
        };
        let sibs = self.children_of(pid);
        if sibs.len() < 2 {
            return false;
        }
        let Some(idx) = sibs.iter().position(|&id| id == self.focused_id) else {
            return false;
        };
        let n = sibs.len() as i32;
        let next = ((idx as i32 + delta).rem_euclid(n)) as usize;
        self.focus(sibs[next]);
        true
    }

    /// Spatial navigation: move focus to the best neighbor in `direction`.
    /// Prefer graph structure when it aligns (left→parent, right→first child),
    /// otherwise nearest node in that half-plane.
    pub fn navigate(&mut self, direction: Direction) -> bool {
        self.focused_edge = None;
        let cur_id = self.focused_id;
        let Some(cur) = self.nodes.get(&cur_id).cloned() else {
            return false;
        };

        // Structural preferences first.
        match direction {
            Direction::Left => {
                if let Some(pid) = cur.parent.filter(|&p| !self.is_hidden(p)) {
                    self.focus(pid);
                    return true;
                }
            }
            Direction::Right => {
                // A folded node has no children to step into; → stays a
                // spatial move instead.
                let kids = if self.is_collapsed(cur_id) {
                    Vec::new()
                } else {
                    self.children_of(cur_id)
                };
                // Prefer a child that is primarily to the right.
                let kids: Vec<u64> = kids.into_iter().filter(|&k| !self.is_hidden(k)).collect();
                if let Some(&kid) = kids.iter().find(|&&id| {
                    self.nodes
                        .get(&id)
                        .map(|n| n.x > cur.x + 8.0)
                        .unwrap_or(false)
                }) {
                    self.focus(kid);
                    return true;
                }
                if let Some(&kid) = kids.first() {
                    self.focus(kid);
                    return true;
                }
            }
            Direction::Up | Direction::Down => {
                // Prefer siblings in that vertical direction under same parent.
                if let Some(pid) = cur.parent {
                    let sibs = self.children_of(pid);
                    let mut best: Option<(u64, f32)> = None;
                    for &sid in &sibs {
                        if sid == cur_id {
                            continue;
                        }
                        let Some(s) = self.nodes.get(&sid) else {
                            continue;
                        };
                        let dy = s.y - cur.y;
                        let ok = match direction {
                            Direction::Up => dy < -4.0,
                            Direction::Down => dy > 4.0,
                            _ => false,
                        };
                        if !ok {
                            continue;
                        }
                        let score = dy.abs() + (s.x - cur.x).abs() * 0.35;
                        if best.map(|(_, b)| score < b).unwrap_or(true) {
                            best = Some((sid, score));
                        }
                    }
                    if let Some((id, _)) = best {
                        self.focus(id);
                        return true;
                    }
                }
            }
        }

        // Spatial fallback: nearest node in the half-plane of the arrow.
        let mut best: Option<(u64, f32)> = None;
        for (&id, n) in &self.nodes {
            if id == cur_id || self.is_hidden(id) {
                continue;
            }
            let dx = n.x - cur.x;
            let dy = n.y - cur.y;
            let aligned = match direction {
                Direction::Right => dx > 8.0 && dx.abs() >= dy.abs() * 0.55,
                Direction::Left => dx < -8.0 && dx.abs() >= dy.abs() * 0.55,
                Direction::Up => dy < -8.0 && dy.abs() >= dx.abs() * 0.55,
                Direction::Down => dy > 8.0 && dy.abs() >= dx.abs() * 0.55,
            };
            if !aligned {
                continue;
            }
            let dist = (dx * dx + dy * dy).sqrt();
            if best.map(|(_, b)| dist < b).unwrap_or(true) {
                best = Some((id, dist));
            }
        }
        if let Some((id, _)) = best {
            self.focus(id);
            true
        } else {
            false
        }
    }

    /// Hit-test using caller-supplied half-widths — the caller passes the real
    /// shaped text widths, so the clickable box is exactly what was painted.
    pub fn hit_test_with(
        &self,
        wx: f32,
        wy: f32,
        half: impl Fn(u64, &Node) -> (f32, f32),
    ) -> Option<u64> {
        self.hit_test_excluding(wx, wy, &HashSet::new(), half)
    }

    /// The same, ignoring a set of nodes. A drag needs this: the node it is
    /// carrying sits under the pointer the whole time, so a plain hit test can
    /// only ever find the thing being dragged and never what it is over.
    pub fn hit_test_excluding(
        &self,
        wx: f32,
        wy: f32,
        skip: &HashSet<u64>,
        half: impl Fn(u64, &Node) -> (f32, f32),
    ) -> Option<u64> {
        // One hidden-set pass beats calling `is_hidden` per node, which walks
        // each node's ancestry with a hash lookup per hop.
        self.hit_test_masked(wx, wy, skip, &self.hidden_set(), half)
    }

    /// The same, but with the hidden set supplied — so callers on the hot mouse
    /// path (hover, drag) reuse the one the frame already built instead of
    /// rebuilding it on every pointer event.
    pub fn hit_test_masked(
        &self,
        wx: f32,
        wy: f32,
        skip: &HashSet<u64>,
        hidden: &HashSet<u64>,
        half: impl Fn(u64, &Node) -> (f32, f32),
    ) -> Option<u64> {
        let mut best: Option<(u64, f32)> = None;
        for (&id, n) in &self.nodes {
            if skip.contains(&id) || hidden.contains(&id) {
                continue;
            }
            let (half_w, half_h) = half(id, n);
            let (half_w, half_h) = (half_w.max(6.0), half_h.max(4.0));
            let dx = (wx - n.x).abs();
            let dy = (wy - n.y).abs();
            if dx <= half_w && dy <= half_h {
                let d2 = dx * dx + dy * dy;
                if best.map(|(_, bd)| d2 < bd).unwrap_or(true) {
                    best = Some((id, d2));
                }
            }
        }
        best.map(|(id, _)| id)
    }

    /// Midpoint of an edge in world space.
    pub fn edge_mid(&self, edge_id: u64) -> Option<(f32, f32)> {
        let e = self.edges.get(&edge_id)?;
        let a = self.nodes.get(&e.from)?;
        let b = self.nodes.get(&e.to)?;
        Some(((a.x + b.x) * 0.5, (a.y + b.y) * 0.5))
    }

    /// Hit-test edges along their whole length, not only at the midpoint, so a
    /// click anywhere on the line selects it. The radius is the caller's, so it
    /// can stay a constant size on screen as the camera pulls back. Nodes win if
    /// both hit — the caller tests them first.
    pub fn hit_test_edge_r(&self, wx: f32, wy: f32, radius: f32) -> Option<u64> {
        let mut best: Option<(u64, f32)> = None;
        for (&id, e) in &self.edges {
            if self.is_hidden(e.to) {
                continue;
            }
            let (Some(a), Some(b)) = (self.nodes.get(&e.from), self.nodes.get(&e.to)) else {
                continue;
            };
            let d2 = dist2_point_segment(wx, wy, a.x, a.y, b.x, b.y);
            if d2 <= radius * radius && best.map(|(_, bd)| d2 < bd).unwrap_or(true) {
                best = Some((id, d2));
            }
        }
        best.map(|(id, _)| id)
    }

    /// Edge connecting parent → focused node (if any).
    /// Lowest id wins, not whichever the hash happens to yield first: with two
    /// edges into one node — which only a hand-edited file can produce — ⌘L
    /// would otherwise open a different label on each run.
    pub fn edge_to_focused(&self) -> Option<u64> {
        self.edges
            .iter()
            .filter(|(_, e)| e.to == self.focused_id)
            .map(|(&id, _)| id)
            .min()
    }

    /// Toggle edge-label edit on the edge into the focused node.
    pub fn toggle_edge_edit_on_focused(&mut self) -> bool {
        if self.focused_edge.is_some() {
            self.clear_edge_focus();
            return true;
        }
        if let Some(eid) = self.edge_to_focused() {
            // ⌘L asks for the label, so this path *does* open it.
            let ok = self.focus_edge(eid);
            if ok {
                self.enter_edit();
            }
            ok
        } else {
            false
        }
    }



    pub fn children_of(&self, parent: u64) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.parent == Some(parent))
            .map(|(&id, _)| id)
            .collect();
        ids.sort();
        ids
    }

    // ---- repositioning -------------------------------------------------

    /// Snap a world coordinate to the drag grid.
    pub fn snap(v: f32) -> f32 {
        (v / layout::SNAP).round() * layout::SNAP
    }

    // ---- structure -----------------------------------------------------

    /// `id` plus all descendants, in stable order. Cycle-safe.
    /// Parent → sorted children, in one pass. Callers that walk many subtrees
    /// (tidy, subtree_ids) build this once instead of scanning every node per
    /// `children_of`, which turns their O(n²) into O(n).
    pub fn children_index(&self) -> HashMap<u64, Vec<u64>> {
        let mut kids: HashMap<u64, Vec<u64>> = HashMap::new();
        for (&id, n) in &self.nodes {
            if let Some(p) = n.parent {
                kids.entry(p).or_default().push(id);
            }
        }
        for v in kids.values_mut() {
            v.sort_unstable();
        }
        kids
    }

    pub fn subtree_ids(&self, id: u64) -> Vec<u64> {
        if !self.nodes.contains_key(&id) {
            return Vec::new();
        }
        let kids = self.children_index();
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            out.push(cur);
            if let Some(cs) = kids.get(&cur) {
                stack.extend(cs.iter().copied());
            }
        }
        out.sort_unstable();
        out
    }

    /// Depth of each node in `wanted`, sharing one memo across the walk.
    /// Paint needs this for every visible edge on every frame; doing it with
    /// `depth` would re-walk (and re-allocate) per edge.
    pub fn depths_for(&self, wanted: &HashSet<u64>) -> HashMap<u64, usize> {
        let mut out: HashMap<u64, usize> = HashMap::with_capacity(wanted.len());
        let mut chain: Vec<u64> = Vec::new();
        for &start in wanted {
            if out.contains_key(&start) || !self.nodes.contains_key(&start) {
                continue;
            }
            chain.clear();
            let mut cur = start;
            // `base` is the depth the *first* entry of `chain` gets. Stopping
            // on a memo hit means that node sits one below the hit; stopping
            // at a root means the root itself is in `chain`, starting at zero.
            let base = loop {
                if let Some(d) = out.get(&cur) {
                    break *d + 1;
                }
                chain.push(cur);
                if chain.len() > self.nodes.len() {
                    break 0; // corrupt parent cycle; already bounded elsewhere
                }
                match self.nodes.get(&cur).and_then(|n| n.parent) {
                    Some(p) => cur = p,
                    None => break 0,
                }
            };
            for (i, id) in chain.iter().rev().enumerate() {
                out.insert(*id, base + i);
            }
        }
        out
    }

    /// Delete a node and its descendants. The root is never deletable — an
    /// empty canvas with no anchor has nowhere to type. Returns true if removed.
    pub fn delete_subtree(&mut self, id: u64) -> bool {
        if id == self.root_id || !self.nodes.contains_key(&id) {
            return false;
        }
        let parent = self.nodes.get(&id).and_then(|n| n.parent);
        let doomed: HashSet<u64> = self.subtree_ids(id).into_iter().collect();
        self.nodes.retain(|nid, _| !doomed.contains(nid));
        self.edges
            .retain(|_, e| !doomed.contains(&e.from) && !doomed.contains(&e.to));

        if doomed.contains(&self.focused_id) {
            self.focused_id = parent
                .filter(|p| self.nodes.contains_key(p))
                .unwrap_or(self.root_id);
        }
        if let Some(eid) = self.focused_edge {
            if !self.edges.contains_key(&eid) {
                self.focused_edge = None;
            }
        }
        // The selection is part of this type's state, so keeping it live is
        // this method's job, not its caller's — a second caller would otherwise
        // reintroduce dead ids into `acting_on`.
        self.selected.retain(|sid| !doomed.contains(sid));
        // Whatever survived is what you are still holding; if nothing did, the
        // cursor's node takes its place so the map is not left with a cursor
        // pointing at something no operation would touch.
        if self.selected.is_empty() && self.nodes.contains_key(&self.focused_id) {
            self.selected.insert(self.focused_id);
        }
        // Deleting is structural — land in Browse on the survivor.
        self.mode = Mode::Browse;
        self.caret_to_end();
        true
    }

    // ---- tidy layout ---------------------------------------------------

    /// Re-lay the whole tree: children stacked vertically to the right of their
    /// parent, each parent centered on the span its subtree occupies.
    pub fn tidy(&mut self) {
        let root = self.root_id;
        // One children index for the whole pass — layout only moves nodes, never
        // reparents, so it stays valid throughout. Turns tidy's O(n²) of
        // per-node `children_of` scans into O(n).
        let idx = self.children_index();
        let mut placed = HashSet::new();
        placed.insert(root);

        // Two-sided, the way a mind map is actually drawn: the root's branches
        // split left and right instead of all stacking to one side. A tree is
        // as tall as its leaf count, so putting half of them on each side is
        // the one change that halves the height of every map — and it stops a
        // wide map reading as a ribbon hanging off the left edge.
        let kids = if self.is_collapsed(root) {
            // A folded root hides the whole map; its branch keeps its shape
            // rather than being flattened into the orphan column.
            self.move_hidden_branch(root, (0.0, 0.0), &mut placed);
            Vec::new()
        } else {
            idx.get(&root).cloned().unwrap_or_default()
        };
        let mut sized: Vec<(u64, f32)> = kids
            .iter()
            .map(|&k| (k, self.subtree_rows(k, 1, &mut HashSet::new(), &idx)))
            .collect();
        // Biggest first, each onto whichever side is currently shorter: a
        // greedy balance, which for the handful of branches a root has is
        // indistinguishable from the optimal split and needs no search.
        sized.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let (mut right, mut left): (Vec<u64>, Vec<u64>) = (Vec::new(), Vec::new());
        let (mut rw, mut lw) = (0.0f32, 0.0f32);
        for (id, rows) in sized {
            if rw <= lw {
                right.push(id);
                rw += rows;
            } else {
                left.push(id);
                lw += rows;
            }
        }
        // Keep each side in the map's own order, so tidying twice does not
        // shuffle branches that did not change.
        right.sort_unstable();
        left.sort_unstable();

        let span = rw.max(lw).max(1.0) * layout::TIDY_ROW;
        for (side, dir) in [(right, 1.0f32), (left, -1.0f32)] {
            let side_rows: f32 = side
                .iter()
                .map(|&k| self.subtree_rows(k, 1, &mut HashSet::new(), &idx))
                .sum();
            let mut y = -side_rows * layout::TIDY_ROW * 0.5;
            for k in side {
                y += self.place_subtree(k, dir * layout::TIDY_COL, y, 1, &mut placed, dir, &idx);
            }
        }
        if let Some(n) = self.nodes.get_mut(&root) {
            n.x = 0.0;
            n.y = 0.0;
        }

        // Anything still unplaced is genuinely unreachable — a hand-edited file,
        // not a folded branch and not a floating node. Free nodes made by
        // double-clicking empty ground are deliberately parentless and keep the
        // place the user put them.
        for (&id, n) in &self.nodes {
            if n.parent.is_none() && id != root {
                placed.insert(id);
            }
        }
        let orphans: Vec<u64> = {
            let mut v: Vec<u64> = self
                .nodes
                .keys()
                .copied()
                .filter(|id| !placed.contains(id))
                .collect();
            v.sort();
            v
        };
        let mut y = span * 0.5 + layout::TIDY_ROW * 2.0;
        for id in orphans {
            if let Some(n) = self.nodes.get_mut(&id) {
                n.x = 0.0;
                n.y = y;
            }
            y += layout::TIDY_ROW;
        }
    }

    fn subtree_rows(
        &self,
        id: u64,
        depth: usize,
        seen: &mut HashSet<u64>,
        idx: &HashMap<u64, Vec<u64>>,
    ) -> f32 {
        if !seen.insert(id) {
            return 0.0;
        }
        // A folded branch occupies one row, not one per hidden leaf — otherwise
        // folding saves no space at all, which is the whole point of it.
        let empty: &[u64] = &[];
        let kids: &[u64] = if self.is_collapsed(id) {
            empty
        } else {
            idx.get(&id).map(|v| v.as_slice()).unwrap_or(empty)
        };
        if kids.is_empty() || depth >= MAX_TIDY_DEPTH {
            return 1.0;
        }
        let sum: f32 = kids
            .iter()
            .map(|k| self.subtree_rows(*k, depth + 1, seen, idx))
            .sum();
        sum.max(1.0)
    }

    /// Places `id` at column `x`, occupying rows starting at `y_top`.
    /// Returns the vertical span consumed. `depth` caps the recursion so a
    /// pathologically deep chain from a hand-edited file cannot blow the stack;
    /// anything past the cap is parked by the orphan pass in `tidy`.
    fn place_subtree(
        &mut self,
        id: u64,
        x: f32,
        y_top: f32,
        depth: usize,
        placed: &mut HashSet<u64>,
        dir: f32,
        idx: &HashMap<u64, Vec<u64>>,
    ) -> f32 {
        if !placed.insert(id) {
            return 0.0;
        }
        // Folded: lay the node out as a leaf, and carry its hidden branch along
        // by the same delta so unfolding shows it where it belongs. Marking
        // those descendants placed is what stops the orphan pass below from
        // parking them in a column at x=0 — which is what used to happen, and
        // which tore a folded branch off its parent on every ⌘R.
        let folded = self.is_collapsed(id);
        let kids: Vec<u64> = if folded {
            Vec::new()
        } else {
            idx.get(&id).cloned().unwrap_or_default()
        };
        if kids.is_empty() || depth >= MAX_TIDY_DEPTH {
            let to = (x, y_top + layout::TIDY_ROW * 0.5);
            if folded {
                self.move_hidden_branch(id, to, placed);
            }
            if let Some(n) = self.nodes.get_mut(&id) {
                n.x = to.0;
                n.y = to.1;
            }
            return layout::TIDY_ROW;
        }
        let mut y = y_top;
        for k in kids {
            y += self.place_subtree(k, x + dir * layout::TIDY_COL, y, depth + 1, placed, dir, idx);
        }
        let span = (y - y_top).max(layout::TIDY_ROW);
        if let Some(n) = self.nodes.get_mut(&id) {
            n.x = x;
            n.y = y_top + span * 0.5;
        }
        span
    }

    /// Move a folded node's hidden descendants by the delta that takes it to
    /// `to`, and mark them placed. Hidden nodes are invisible to layout, but
    /// they are not homeless.
    fn move_hidden_branch(&mut self, id: u64, to: (f32, f32), placed: &mut HashSet<u64>) {
        let Some(n) = self.nodes.get(&id) else { return };
        let (dx, dy) = (to.0 - n.x, to.1 - n.y);
        for k in self.subtree_ids(id) {
            if k == id {
                continue;
            }
            placed.insert(k);
            if let Some(c) = self.nodes.get_mut(&k) {
                c.x += dx;
                c.y += dy;
            }
        }
    }

    fn other_centers(&self, ignore: Option<u64>) -> Vec<(f32, f32)> {
        let hidden = self.hidden_set();
        self.nodes
            .iter()
            .filter(|(&id, _)| Some(id) != ignore && !hidden.contains(&id))
            .map(|(_, n)| (n.x, n.y))
            .collect()
    }

    fn boxes_intersect(a: (f32, f32), b: (f32, f32)) -> bool {
        let (aw, ah) = (
            layout::NODE_HALF_W * 2.0 + layout::MIN_GAP,
            layout::NODE_HALF_H * 2.0 + layout::MIN_GAP,
        );
        let (bw, bh) = (aw, ah);
        let (ax, ay) = (a.0 - aw / 2.0, a.1 - ah / 2.0);
        let (bx, by) = (b.0 - bw / 2.0, b.1 - bh / 2.0);
        !(ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay)
    }

    fn resolve_position(&self, desired: (f32, f32), ignore: Option<u64>) -> (f32, f32) {
        let others = self.other_centers(ignore);
        let mut p = desired;
        for _ in 0..24 {
            let mut moved = false;
            for o in &others {
                if !Self::boxes_intersect(p, *o) {
                    continue;
                }
                let dx = p.0 - o.0;
                let dy = p.1 - o.1;
                let min_dx = layout::NODE_HALF_W * 2.0 + layout::MIN_GAP;
                let min_dy = layout::NODE_HALF_H * 2.0 + layout::MIN_GAP;
                if dx.abs() < 1e-4 && dy.abs() < 1e-4 {
                    p.0 += min_dx;
                    moved = true;
                    continue;
                }
                let pen_x = min_dx - dx.abs();
                let pen_y = min_dy - dy.abs();
                if pen_x > 0.0 && pen_y > 0.0 {
                    if pen_x < pen_y {
                        p.0 += if dx >= 0.0 { pen_x } else { -pen_x };
                    } else {
                        p.1 += if dy >= 0.0 { pen_y } else { -pen_y };
                    }
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        p
    }

    /// Layout invariant checked by the tests: no two node boxes collide.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn has_overlapping_nodes(&self) -> bool {
        let pts: Vec<(f32, f32)> = self.nodes.values().map(|n| (n.x, n.y)).collect();
        for i in 0..pts.len() {
            for j in (i + 1)..pts.len() {
                if Self::boxes_intersect(pts[i], pts[j]) {
                    return true;
                }
            }
        }
        false
    }

    /// Create a child of `parent` in `direction`. Returns new node id.
    pub fn create_child(&mut self, parent: u64, direction: Direction, text: &str) -> Option<u64> {
        let parent_node = self.nodes.get(&parent)?.clone();
        let kids = self.children_of(parent);
        let same_axis = kids
            .iter()
            .filter(|id| {
                let c = &self.nodes[id];
                match direction {
                    Direction::Right => c.x > parent_node.x + 20.0,
                    Direction::Left => c.x < parent_node.x - 20.0,
                    Direction::Up => c.y < parent_node.y - 20.0,
                    Direction::Down => c.y > parent_node.y + 20.0,
                }
            })
            .count();

        let (ox, oy) = direction.offset();
        let mut cand = (parent_node.x + ox, parent_node.y + oy);
        match direction {
            Direction::Right | Direction::Left => {
                cand.1 += same_axis as f32 * (layout::NODE_HALF_H * 2.0 + layout::MIN_GAP);
            }
            Direction::Up | Direction::Down => {
                cand.0 += same_axis as f32 * (layout::NODE_HALF_W * 2.0 + layout::MIN_GAP);
            }
        }
        cand = self.resolve_position(cand, None);
        self.open_for_content(parent);

        // Both ids or neither: taking one and failing on the second would
        // leave a hole in the counter for no reason.
        let (Some(id), Some(eid)) = self.take_id_pair() else {
            // Out of ids. Refusing to create is the only safe answer: a wrapped
            // counter hands back an id that is already in use, and the insert
            // below would then silently replace an existing node — the root,
            // if the counter wrapped all the way to 1.
            return None;
        };
        self.nodes.insert(
            id,
            Node {
                text: text.to_string(),
                x: cand.0,
                y: cand.1,
                parent: Some(parent),
                collapsed: false,
                color: None,
                image: None,
            },
        );
        self.edges.insert(
            eid,
            Edge {
                from: parent,
                to: id,
                label: String::new(),
            },
        );
        self.focused_id = id;
        // The new node is what you are now holding, and the only thing.
        self.selected.clear();
        self.selected.insert(id);
        // Leave any prior edge selection.
        self.focused_edge = None;
        // Creating always drops into edit so the user can name the child.
        self.mode = Mode::Edit;
        self.caret = text.len();
        self.anchor = None;
        Some(id)
    }

    /// Make a child of `parent` and drop it at world `(x, y)` (nudged clear of
    /// any overlap), rather than at the usual stacked offset. For grow-from-ring.
    pub fn create_child_at(&mut self, parent: u64, x: f32, y: f32, text: &str) -> Option<u64> {
        let id = self.create_child(parent, Direction::Right, text)?;
        let placed = self.resolve_position((x, y), Some(id));
        if let Some(n) = self.nodes.get_mut(&id) {
            n.x = placed.0;
            n.y = placed.1;
        }
        Some(id)
    }

    pub fn create_from_focused(&mut self, direction: Direction, text: &str) -> Option<u64> {
        self.create_child(self.focused_id, direction, text)
    }

    /// A new node beside the focused one, under the same parent — the single
    /// most common thing anyone does in a mind map, and until now it took three
    /// keystrokes and a focus round-trip. On the root, where there is no parent
    /// to share, it falls back to a child so the key is never simply dead.
    pub fn create_sibling(&mut self, text: &str) -> Option<u64> {
        let parent = self.nodes.get(&self.focused_id).and_then(|n| n.parent);
        match parent {
            Some(pid) => self.create_child(pid, Direction::Down, text),
            None => self.create_child(self.focused_id, Direction::Right, text),
        }
    }

    /// Grow a branch from an indented text outline, hung under `parent`. Leading
    /// spaces (tab = 4) or `-`/`*`/`+`/`#` markers set the nesting; blank lines
    /// are skipped. Returns the first node made, or `None` for empty input — so
    /// a paste of ordinary prose still lands as a flat list of children.
    pub fn paste_outline(&mut self, parent: u64, text: &str) -> Option<u64> {
        let mut items: Vec<(usize, String)> = Vec::new();
        for raw in text.lines() {
            if raw.trim().is_empty() {
                continue;
            }
            let indent: usize = raw
                .chars()
                .take_while(|c| *c == ' ' || *c == '\t')
                .map(|c| if c == '\t' { 4 } else { 1 })
                .sum();
            let body = raw
                .trim_start()
                .trim_start_matches(|c| c == '-' || c == '*' || c == '+' || c == '#')
                .trim();
            if body.is_empty() {
                continue;
            }
            items.push((indent, body.to_string()));
        }
        // A stack of (indent, node) whose top is the parent for the next
        // deeper line; anything at or above the new indent has closed.
        let mut stack: Vec<(usize, u64)> = Vec::new();
        let mut first = None;
        for (indent, body) in items {
            while stack.last().is_some_and(|&(ind, _)| ind >= indent) {
                stack.pop();
            }
            let under = stack.last().map(|&(_, id)| id).unwrap_or(parent);
            let Some(id) = self.create_child(under, Direction::Right, &body) else {
                break;
            };
            first.get_or_insert(id);
            stack.push((indent, id));
        }
        first
    }

    /// Demo seed: a small but branchy map so pan/zoom/drag have something to chew on.
    pub fn demo() -> Self {
        let mut g = Self::new("release 0.3");
        let root = g.root_id;

        let scope = g.create_child(root, Direction::Right, "scope").unwrap();
        let a = g.create_child(scope, Direction::Right, "multi-line labels").unwrap();
        g.create_child(scope, Direction::Right, "edge editing");
        g.create_child(scope, Direction::Right, "cross-links");
        g.create_child(a, Direction::Right, "wrap at node width");
        g.create_child(a, Direction::Right, "grow, don't clip");

        let risk = g.create_child(root, Direction::Up, "risk").unwrap();
        g.create_child(risk, Direction::Right, "tidy is whole-map");
        let tests = g.create_child(risk, Direction::Right, "ui.rs has no tests").unwrap();
        g.create_child(tests, Direction::Right, "extract find.rs");
        g.create_child(tests, Direction::Right, "extract frame.rs");
        g.create_child(risk, Direction::Right, "exports drop images");

        let ship = g.create_child(root, Direction::Down, "ship").unwrap();
        g.create_child(ship, Direction::Right, "notarize");
        g.create_child(ship, Direction::Right, "write the notes");
        let later = g.create_child(ship, Direction::Right, "later").unwrap();
        g.create_child(later, Direction::Right, "sparkle updates");
        g.create_child(later, Direction::Right, "iCloud");

        let cut = g.create_child(root, Direction::Left, "cut").unwrap();
        g.create_child(cut, Direction::Left, "themes");
        g.create_child(cut, Direction::Left, "shape picker");
        g.create_child(cut, Direction::Left, "auto-layout");

        // One folded branch, because folding is the thing that makes a big map
        // readable and a demo that never shows it never explains it.
        if let Some(n) = g.nodes.get_mut(&later) {
            n.collapsed = true;
        }
        // Colour tags: risk reads hot, shipped reads cool.
        for (id, color) in [(risk, 1u8), (ship, 4u8), (cut, 6u8)] {
            if let Some(n) = g.nodes.get_mut(&id) {
                n.color = Some(color);
            }
        }

        g.tidy();
        g.focus(root);
        // Creating drops into Edit; a map is handed over ready to navigate.
        g.leave_edit();
        g
    }

    /// World-space extent of the map, padded. Text half-widths come from the
    /// caller so a fit-to-content never clips a long label.
    pub fn bounds_with(&self, half: impl Fn(u64, &Node) -> (f32, f32)) -> (f32, f32, f32, f32) {
        let mut min_x = f32::MAX;
        let mut max_x = f32::MIN;
        let mut min_y = f32::MAX;
        let mut max_y = f32::MIN;
        let hidden = self.hidden_set();
        for (&id, n) in &self.nodes {
            // Fit to what is on screen, not to branches that are folded away.
            if hidden.contains(&id) {
                continue;
            }
            let (hw, hh) = half(id, n);
            let hw = hw.max(layout::NODE_HALF_W * 0.5);
            let hh = hh.max(layout::NODE_HALF_H);
            min_x = min_x.min(n.x - hw);
            max_x = max_x.max(n.x + hw);
            min_y = min_y.min(n.y - hh);
            max_y = max_y.max(n.y + hh);
        }
        // An empty map — or one whose every position was NaN — leaves these at
        // their sentinels, which are finite and inverted. Check the shape, not
        // just finiteness, or the fit downstream is asked to frame a rectangle
        // that runs backwards.
        if !(min_x.is_finite() && max_x.is_finite() && min_x <= max_x)
            || !(min_y.is_finite() && max_y.is_finite() && min_y <= max_y)
        {
            return (-100.0, -100.0, 100.0, 100.0);
        }
        let pad = 60.0;
        (min_x - pad, min_y - pad, max_x + pad, max_y + pad)
    }

    /// Two ids at once, or none at all.
    fn take_id_pair(&mut self) -> (Option<u64>, Option<u64>) {
        let before = self.next_id;
        match (self.take_id(), self.take_id()) {
            (Some(a), Some(b)) => (Some(a), Some(b)),
            _ => {
                self.next_id = before;
                (None, None)
            }
        }
    }

    /// Hand out the next free id, or `None` once the counter is exhausted or
    /// would collide with something already in the map.
    fn take_id(&mut self) -> Option<u64> {
        let id = self.next_id;
        if id == u64::MAX || self.nodes.contains_key(&id) || self.edges.contains_key(&id) {
            return None;
        }
        self.next_id = id + 1;
        Some(id)
    }

    // ---- clipboard -------------------------------------------------------

    /// Lift the selected nodes, each with its whole subtree, into a portable
    /// clip. Ids are dropped: positions are stored relative to the clip's own
    /// top-left and parent links as indices, so a paste can mint fresh ids and
    /// land anywhere — including in a different map.
    pub fn copy_subtrees(&self, ids: &[u64]) -> Option<Clip> {
        let mut taken: Vec<u64> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        for &id in ids {
            for sid in self.subtree_ids(id) {
                if seen.insert(sid) {
                    taken.push(sid);
                }
            }
        }
        if taken.is_empty() {
            return None;
        }
        let index: HashMap<u64, usize> = taken.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let (ox, oy) = taken
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .fold((f32::MAX, f32::MAX), |(x, y), n| (x.min(n.x), y.min(n.y)));
        let label_from = |parent: u64, child: u64| {
            self.edges
                .values()
                .find(|e| e.from == parent && e.to == child)
                .map(|e| e.label.clone())
                .unwrap_or_default()
        };
        let nodes = taken
            .iter()
            .filter_map(|&id| {
                let n = self.nodes.get(&id)?;
                // A parent outside the clip is not carried: that node becomes a
                // root of the clip and attaches wherever it is pasted.
                let parent = n.parent.filter(|p| index.contains_key(p));
                Some(ClipNode {
                    text: n.text.clone(),
                    collapsed: n.collapsed,
                    color: n.color,
                    image: n.image.clone(),
                    dx: n.x - ox,
                    dy: n.y - oy,
                    parent: parent.map(|p| index[&p]),
                    label: parent.map(|p| label_from(p, id)).unwrap_or_default(),
                })
            })
            .collect();
        Some(Clip { nodes })
    }

    /// Drop a clip into the map at `(x, y)`, hanging its roots off `parent`.
    /// Returns the new ids, and leaves them selected — you almost always want
    /// to move or rename what you just pasted.
    pub fn paste(&mut self, clip: &Clip, x: f32, y: f32, parent: Option<u64>) -> Vec<u64> {
        let parent = parent.filter(|p| self.nodes.contains_key(p));
        if let Some(p) = parent {
            self.open_for_content(p);
        }
        let mut fresh: Vec<u64> = Vec::with_capacity(clip.nodes.len());
        for cn in &clip.nodes {
            let Some(id) = self.take_id() else { break };
            self.nodes.insert(
                id,
                Node {
                    text: cn.text.clone(),
                    x: x + cn.dx,
                    y: y + cn.dy,
                    parent: None,
                    collapsed: cn.collapsed,
                    color: cn.color,
                    image: cn.image.clone(),
                },
            );
            fresh.push(id);
        }
        // Second pass: links, now that every id in the clip exists.
        for (i, cn) in clip.nodes.iter().enumerate() {
            let Some(&id) = fresh.get(i) else { break };
            let link = match cn.parent {
                Some(pi) => fresh.get(pi).copied(),
                None => parent,
            };
            let Some(pid) = link else { continue };
            if let Some(n) = self.nodes.get_mut(&id) {
                n.parent = Some(pid);
            }
            let Some(eid) = self.take_id() else { continue };
            self.edges.insert(
                eid,
                Edge {
                    from: pid,
                    to: id,
                    label: cn.label.clone(),
                },
            );
        }
        if let Some(&first) = fresh.first() {
            self.focused_edge = None;
            self.focused_id = first;
            self.selected = fresh.iter().copied().collect();
            self.mode = Mode::Browse;
            self.caret_to_end();
            self.selected = fresh.iter().copied().collect();
        }
        fresh
    }

    /// Whether `id` may be hung under `parent`. Refuses the root, a node's own
    /// current parent (nothing to do), itself, and anything inside its own
    /// subtree — that last one is what would turn the tree into a ring and hang
    /// every traversal.
    pub fn can_reparent(&self, id: u64, parent: u64) -> bool {
        if id == self.root_id || id == parent {
            return false;
        }
        if !self.nodes.contains_key(&id) || !self.nodes.contains_key(&parent) {
            return false;
        }
        if self.nodes.get(&id).and_then(|n| n.parent) == Some(parent) {
            return false;
        }
        !self.subtree_ids(id).contains(&parent)
    }

    /// Hang `id` (and everything under it, which follows for free) under
    /// `parent`. Both halves of the tree are updated: the parent link and the
    /// edge that draws it.
    pub fn reparent(&mut self, id: u64, parent: u64) -> bool {
        if !self.can_reparent(id, parent) {
            return false;
        }
        // Drop the edge that described the old link before adding the new one,
        // or `sanitize` would later have to guess which of the two is real.
        let stale: Vec<u64> = self
            .edges
            .iter()
            .filter(|(_, e)| e.to == id)
            .map(|(&eid, _)| eid)
            .collect();
        let label = stale
            .first()
            .and_then(|eid| self.edges.get(eid))
            .map(|e| e.label.clone())
            .unwrap_or_default();
        for eid in stale {
            self.edges.remove(&eid);
        }
        self.open_for_content(parent);
        if let Some(n) = self.nodes.get_mut(&id) {
            n.parent = Some(parent);
        }
        if let Some(eid) = self.take_id() {
            self.edges.insert(
                eid,
                Edge {
                    from: parent,
                    to: id,
                    label,
                },
            );
        }
        true
    }

    /// Break a node's link to its parent, leaving it — and everything hanging
    /// off it — a floating root. Drops the incoming edge; every child edge
    /// stays. The map's own root has no parent to cut. Returns whether anything
    /// changed, so the caller can skip the undo step on a no-op.
    pub fn detach(&mut self, id: u64) -> bool {
        let had_parent = self.nodes.get(&id).is_some_and(|n| n.parent.is_some());
        if !had_parent {
            return false;
        }
        let stale: Vec<u64> = self
            .edges
            .iter()
            .filter(|(_, e)| e.to == id)
            .map(|(&eid, _)| eid)
            .collect();
        for eid in stale {
            self.edges.remove(&eid);
        }
        if let Some(n) = self.nodes.get_mut(&id) {
            n.parent = None;
        }
        true
    }

    /// `id`'s siblings top-to-bottom by screen position (y, then id to break a
    /// tie), `id` included. The order the arrows and reorder walk.
    pub fn siblings_ordered(&self, id: u64) -> Vec<u64> {
        let mut sibs = self.siblings_of(id);
        sibs.sort_by(|&a, &b| {
            let ay = self.nodes.get(&a).map(|n| n.y).unwrap_or(0.0);
            let by = self.nodes.get(&b).map(|n| n.y).unwrap_or(0.0);
            ay.partial_cmp(&by)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        sibs
    }

    /// Whether `id` has a sibling on the given side to trade places with.
    pub fn can_move_sibling(&self, id: u64, up: bool) -> bool {
        let ordered = self.siblings_ordered(id);
        match ordered.iter().position(|&x| x == id) {
            Some(pos) if up => pos > 0,
            Some(pos) => pos + 1 < ordered.len(),
            None => false,
        }
    }

    /// Trade places with the sibling just above (`up`) or below `id`, so their
    /// order changes. Each node carries its own subtree by the swap delta.
    pub fn move_sibling(&mut self, id: u64, up: bool) -> bool {
        let ordered = self.siblings_ordered(id);
        let Some(pos) = ordered.iter().position(|&x| x == id) else {
            return false;
        };
        let other = if up {
            if pos == 0 {
                return false;
            }
            ordered[pos - 1]
        } else {
            if pos + 1 >= ordered.len() {
                return false;
            }
            ordered[pos + 1]
        };
        let (Some(a), Some(b)) = (
            self.nodes.get(&id).map(|n| (n.x, n.y)),
            self.nodes.get(&other).map(|n| (n.x, n.y)),
        ) else {
            return false;
        };
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let ids_a = self.subtree_ids(id);
        let ids_b = self.subtree_ids(other);
        self.move_nodes_by(&ids_a, dx, dy);
        self.move_nodes_by(&ids_b, -dx, -dy);
        true
    }

    /// `id`'s grandparent, if any — the node an outdent promotes it to.
    pub fn can_outdent(&self, id: u64) -> bool {
        self.nodes
            .get(&id)
            .and_then(|n| n.parent)
            .and_then(|p| self.nodes.get(&p).and_then(|pn| pn.parent))
            .is_some()
    }

    /// Promote `id` to sit beside its parent — reparent it to its grandparent.
    pub fn outdent(&mut self, id: u64) -> bool {
        let Some(gp) = self
            .nodes
            .get(&id)
            .and_then(|n| n.parent)
            .and_then(|p| self.nodes.get(&p).and_then(|pn| pn.parent))
        else {
            return false;
        };
        self.reparent(id, gp)
    }

    /// Whether `id` has a preceding sibling to become a child of. The root is
    /// never re-parentable, so it can never indent even when detached branches
    /// give it top-level siblings.
    pub fn can_indent(&self, id: u64) -> bool {
        if id == self.root_id {
            return false;
        }
        let ordered = self.siblings_ordered(id);
        ordered.first().is_some_and(|&first| first != id) && ordered.len() > 1
    }

    /// Demote `id` under the sibling just above it — the outliner's Tab-indent.
    pub fn indent(&mut self, id: u64) -> bool {
        let ordered = self.siblings_ordered(id);
        let Some(pos) = ordered.iter().position(|&x| x == id) else {
            return false;
        };
        if pos == 0 {
            return false;
        }
        let prev = ordered[pos - 1];
        self.reparent(id, prev)
    }

    /// Nudge a just-dropped branch clear of whatever it landed on, carrying its
    /// descendants by the same delta. A drop lands where the pointer was, which
    /// is on top of the new parent — legible for the half-second you are aiming
    /// and unreadable once you let go.
    pub fn settle_after_drop(&mut self, id: u64) {
        let Some(n) = self.nodes.get(&id) else { return };
        let (x, y) = (n.x, n.y);
        let placed = self.resolve_position((x, y), Some(id));
        let (dx, dy) = (placed.0 - x, placed.1 - y);
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let ids = self.subtree_ids(id);
        self.move_nodes_by(&ids, dx, dy);
    }

    /// The nodes a drop would re-hang: the members of `ids` that are not
    /// already inside another member, so a branch and its own child dropped
    /// together do not fight over where the child ends up.
    pub fn reparent_roots(&self, ids: &[u64]) -> Vec<u64> {
        let picked: HashSet<u64> = ids.iter().copied().collect();
        let mut out: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|&id| {
                let mut cur = self.nodes.get(&id).and_then(|n| n.parent);
                let mut hops = 0;
                while let Some(p) = cur {
                    if picked.contains(&p) {
                        return false;
                    }
                    hops += 1;
                    if hops > MAX_TIDY_DEPTH {
                        break;
                    }
                    cur = self.nodes.get(&p).and_then(|n| n.parent);
                }
                true
            })
            .collect();
        out.sort_unstable();
        out
    }

    /// A node with no parent, placed where it was asked for — a floating topic.
    /// It is reachable: arrow navigation falls back to the nearest node in the
    /// half-plane, which does not care about the tree.
    pub fn create_free_node(&mut self, x: f32, y: f32) -> Option<u64> {
        let id = self.take_id()?;
        self.nodes.insert(
            id,
            Node {
                text: String::new(),
                x,
                y,
                parent: None,
                collapsed: false,
                color: None,
                image: None,
            },
        );
        self.focused_edge = None;
        self.focused_id = id;
        self.selected.clear();
        self.selected.insert(id);
        // Straight into Edit, the same as ⌘-arrow: a nameless node is not what
        // anyone wants to be left holding.
        self.mode = Mode::Edit;
        self.caret = 0;
        self.anchor = None;
        Some(id)
    }

    // ---- folding ---------------------------------------------------------
    //
    // One rule, applied everywhere: a node under a collapsed ancestor is
    // *hidden*, and hidden nodes are not drawn, not hit, not walked and not
    // laid out. They are still deleted, copied and moved with their branch —
    // folding is a view of the document, not a change to it.

    pub fn is_collapsed(&self, id: u64) -> bool {
        self.nodes.get(&id).is_some_and(|n| n.collapsed)
    }

    /// Fold or unfold `id`. Refuses a node with nothing under it: a fold that
    /// hides nothing is a control that does nothing.
    pub fn toggle_collapsed(&mut self, id: u64) -> bool {
        if self.children_of(id).is_empty() {
            return false;
        }
        match self.nodes.get_mut(&id) {
            Some(n) => n.collapsed = !n.collapsed,
            None => return false,
        }
        // A fold that swallows the cursor would leave the arrows walking around
        // inside a branch nobody can see, and typing into an invisible label.
        // Whatever the fold hid, you now stand on the thing that hid it.
        if self.is_collapsed(id) && (self.is_hidden(self.focused_id) || self.focused_id == id) {
            self.focus(id);
        }
        let stranded: Vec<u64> = self
            .selected
            .iter()
            .copied()
            .filter(|&s| self.is_hidden(s))
            .collect();
        for s in stranded {
            self.selected.remove(&s);
        }
        if self.selected.is_empty() {
            self.selected.insert(id);
            self.focused_id = id;
        }
        true
    }

    /// Unfold `id` so something can be put inside it and seen. Creating,
    /// pasting or dropping into a folded branch otherwise lands content —
    /// and the caret — somewhere with no way to look at it.
    fn open_for_content(&mut self, id: u64) {
        if let Some(n) = self.nodes.get_mut(&id) {
            n.collapsed = false;
        }
        self.reveal(id);
    }

    /// Whether an ancestor of `id` is folded. The node itself being folded does
    /// not hide it — you have to be able to see the thing you unfold.
    pub fn is_hidden(&self, id: u64) -> bool {
        let mut cur = self.nodes.get(&id).and_then(|n| n.parent);
        let mut hops = 0;
        while let Some(p) = cur {
            if self.is_collapsed(p) {
                return true;
            }
            hops += 1;
            if hops > MAX_TIDY_DEPTH {
                break;
            }
            cur = self.nodes.get(&p).and_then(|n| n.parent);
        }
        false
    }

    /// Everything hidden, in one downward pass — cheaper than asking per node
    /// when the caller needs the whole answer, as paint and layout do.
    pub fn hidden_set(&self) -> HashSet<u64> {
        self.hidden_set_from(&self.children_index())
    }

    /// The same, reusing a children index the caller already built — so a frame
    /// that needs both the hidden set and child counts pays for the index once.
    pub fn hidden_set_from(&self, kids: &HashMap<u64, Vec<u64>>) -> HashSet<u64> {
        let mut hidden = HashSet::new();
        let mut stack: Vec<(u64, bool)> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.parent.is_none())
            .map(|(&id, _)| (id, false))
            .collect();
        let mut seen = HashSet::new();
        while let Some((id, under_fold)) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if under_fold {
                hidden.insert(id);
            }
            let folded = under_fold || self.is_collapsed(id);
            for &k in kids.get(&id).into_iter().flatten() {
                stack.push((k, folded));
            }
        }
        hidden
    }

    /// Child counts for every node, in one pass. `children_of` is a full scan,
    /// so asking it per on-screen node frame is O(n²) on the caret-blink path.
    pub fn child_counts(&self) -> HashMap<u64, usize> {
        let mut counts: HashMap<u64, usize> = HashMap::with_capacity(self.nodes.len());
        for n in self.nodes.values() {
            if let Some(p) = n.parent {
                *counts.entry(p).or_insert(0) += 1;
            }
        }
        counts
    }

    /// How many nodes a fold is currently hiding under `id`.
    pub fn hidden_count(&self, id: u64) -> usize {
        self.subtree_ids(id).len().saturating_sub(1)
    }

    /// Nodes whose label contains `query`, case-insensitively, in a stable
    /// order. Folded-away nodes are included: not being able to find something
    /// because you folded it is exactly the case find exists for.
    pub fn find(&self, query: &str) -> Vec<u64> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<u64> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.text.to_lowercase().contains(&q))
            .map(|(&id, _)| id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Replace every case-insensitive occurrence of `query` with `replacement`
    /// across all labels. Returns how many labels changed. Matches on chars, so
    /// it is safe on any UTF-8; on the short labels here the cost is trivial.
    pub fn replace_all(&mut self, query: &str, replacement: &str) -> usize {
        let q_str = query.to_lowercase();
        let q_len = query.chars().count();
        if q_len == 0 {
            return 0;
        }
        let mut changed = 0;
        for node in self.nodes.values_mut() {
            let chars: Vec<char> = node.text.chars().collect();
            let mut out = String::with_capacity(node.text.len());
            let mut i = 0;
            while i < chars.len() {
                if i + q_len <= chars.len() {
                    let window: String = chars[i..i + q_len].iter().collect();
                    if window.to_lowercase() == q_str {
                        out.push_str(replacement);
                        i += q_len;
                        continue;
                    }
                }
                out.push(chars[i]);
                i += 1;
            }
            // Only a real change counts — replacing a term with itself is a no-op
            // and must not report a change or (upstream) push an undo step.
            if out != node.text {
                node.text = out;
                changed += 1;
            }
        }
        changed
    }

    /// Unfold whatever is hiding `id`, so going to it actually shows it.
    pub fn reveal(&mut self, id: u64) -> bool {
        let mut chain = Vec::new();
        let mut cur = self.nodes.get(&id).and_then(|n| n.parent);
        let mut hops = 0;
        while let Some(p) = cur {
            if self.is_collapsed(p) {
                chain.push(p);
            }
            hops += 1;
            if hops > MAX_TIDY_DEPTH {
                break;
            }
            cur = self.nodes.get(&p).and_then(|n| n.parent);
        }
        let any = !chain.is_empty();
        for p in chain {
            if let Some(n) = self.nodes.get_mut(&p) {
                n.collapsed = false;
            }
        }
        any
    }

    /// Whether every node sits where it sits in `other`. Lets a layout pass
    /// tell "this changed nothing" from "this moved things", without the caller
    /// having to reason about what tidy does.
    pub fn positions_match(&self, other: &Graph) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.nodes.iter().all(|(id, n)| {
                other
                    .nodes
                    .get(id)
                    .is_some_and(|o| o.x == n.x && o.y == n.y)
            })
    }

    /// Replace any NaN or infinite node position with the origin, so a bad
    /// coordinate cannot reach the save file as JSON `null` and make the
    /// document unloadable. Cheap enough to run on every write.
    pub fn drop_non_finite_positions(&mut self) {
        for n in self.nodes.values_mut() {
            if !n.x.is_finite() {
                n.x = 0.0;
            }
            // A picture's box is trusted by `Shaped::half`, and from there by
            // hit-testing, the marquee, the fit, edge trimming and the cull. A
            // hand-edited or truncated map with a zero or absurd size would
            // poison every one of them.
            if let Some(image) = n.image.as_ref() {
                let (w, h) = (image.w, image.h);
                if !w.is_finite() || !h.is_finite() || w < MIN_IMAGE_BOX || h < MIN_IMAGE_BOX
                    || w > MAX_IMAGE_BOX || h > MAX_IMAGE_BOX
                {
                    n.image = Some(Arc::new(NodeImage {
                        name: image.name.clone(),
                        w: w.clamp(MIN_IMAGE_BOX, MAX_IMAGE_BOX).max(MIN_IMAGE_BOX),
                        h: h.clamp(MIN_IMAGE_BOX, MAX_IMAGE_BOX).max(MIN_IMAGE_BOX),
                    }));
                }
            }
            if !n.y.is_finite() {
                n.y = 0.0;
            }
        }
    }

    /// Make a graph from an untrusted source (a hand-edited save file) safe to
    /// render: drop dangling edges, repair focus, keep `next_id` ahead of every id.
    pub fn sanitize(&mut self) {
        if !self.nodes.contains_key(&self.root_id) {
            // No root: adopt the lowest id, or rebuild an empty map.
            match self.nodes.keys().copied().min() {
                Some(id) => self.root_id = id,
                None => *self = Graph::new(""),
            }
        }
        if let Some(root) = self.nodes.get_mut(&self.root_id) {
            root.parent = None;
        }
        let ids: HashSet<u64> = self.nodes.keys().copied().collect();
        for (&id, n) in self.nodes.iter_mut() {
            if let Some(p) = n.parent {
                if !ids.contains(&p) || p == id {
                    n.parent = None;
                }
            }
            if !n.x.is_finite() {
                n.x = 0.0;
            }
            if !n.y.is_finite() {
                n.y = 0.0;
            }
        }
        // A parent chain that loops would hang traversals — break it at the
        // cycle. Only the nodes *on* the loop get reparented: a descendant
        // hanging off a cycle has a perfectly good parent link, and flattening
        // it to the root destroys structure the file was right about.
        let root = self.root_id;
        let mut cleared = Vec::new();
        for &id in &ids {
            // Floyd: if the walk from `id` meets itself, `id` is on the loop
            // exactly when the meeting point can walk back round to it.
            let parent_of = |n: u64| self.nodes.get(&n).and_then(|x| x.parent);
            let (mut slow, mut fast) = (Some(id), Some(id));
            let looped = loop {
                fast = fast.and_then(parent_of).and_then(parent_of);
                slow = slow.and_then(parent_of);
                match (slow, fast) {
                    (Some(a), Some(b)) if a == b => break true,
                    (_, None) => break false,
                    _ => {}
                }
            };
            if !looped {
                continue;
            }
            // Walk the loop from the meeting point; `id` is a member only if it
            // shows up on it.
            let start = slow;
            let mut cur = start;
            let mut on_loop = false;
            for _ in 0..=ids.len() {
                if cur == Some(id) {
                    on_loop = true;
                    break;
                }
                cur = cur.and_then(parent_of);
                if cur == start {
                    break;
                }
            }
            if on_loop {
                cleared.push(id);
            }
        }
        for id in cleared {
            if id != root {
                if let Some(n) = self.nodes.get_mut(&id) {
                    n.parent = Some(root);
                }
            }
        }
        // Get the counter ahead of every id in the file *before* reconciling
        // edges, since that step may have to mint some.
        let max_id = ids
            .iter()
            .chain(self.edges.keys())
            .copied()
            .max()
            .unwrap_or(0);
        self.next_id = self.next_id.max(max_id.saturating_add(1));

        // The tree is stored twice — as `Node.parent` and as `edges` — and a
        // hand-edited file can disagree with itself. `parent` is the authority:
        // it drives navigation, subtree deletion and tidy, so an edge without a
        // parent link is a line to a node the keyboard cannot reach, and a
        // parent link without an edge is a child drawn with nothing joining it.
        self.edges
            .retain(|_, e| ids.contains(&e.from) && ids.contains(&e.to) && e.from != e.to);
        let wanted: HashSet<(u64, u64)> = self
            .nodes
            .iter()
            .filter_map(|(&id, n)| n.parent.map(|p| (p, id)))
            .collect();
        // Keep at most one edge per link, lowest id first so the choice is
        // stable across runs rather than left to hash order.
        let mut kept: HashSet<(u64, u64)> = HashSet::new();
        let mut eids: Vec<u64> = self.edges.keys().copied().collect();
        eids.sort_unstable();
        let mut drop: Vec<u64> = Vec::new();
        for eid in eids {
            let Some(e) = self.edges.get(&eid) else { continue };
            let link = (e.from, e.to);
            if !wanted.contains(&link) || !kept.insert(link) {
                drop.push(eid);
            }
        }
        for eid in drop {
            self.edges.remove(&eid);
        }
        let mut missing: Vec<(u64, u64)> = wanted.difference(&kept).copied().collect();
        missing.sort_unstable();
        for (from, to) in missing {
            let Some(eid) = self.take_id() else { break };
            self.edges.insert(
                eid,
                Edge {
                    from,
                    to,
                    label: String::new(),
                },
            );
        }
        if !self.nodes.contains_key(&self.focused_id) {
            self.focused_id = self.root_id;
        }
        if let Some(eid) = self.focused_edge {
            if !self.edges.contains_key(&eid) {
                self.focused_edge = None;
            }
        }
        // A selection can outlive the nodes it named — deleting through undo,
        // or a load that dropped them.
        self.selected.retain(|id| ids.contains(id));
        // The caret and its anchor index the active text, which may have just
        // changed underneath them.
        self.clamp_caret();
        let len = self.active_text().len();
        if self.anchor.is_some_and(|a| a > len || !self.active_text().is_char_boundary(a)) {
            self.anchor = None;
        }
    }
}



/// Caret arithmetic over Unicode text. Byte offsets in, byte offsets out;
/// every boundary comes from `unicode-segmentation` rather than from guessing
/// which scalar values glue to which.
trait TextCursor {
    fn grapheme_start_before(&self, at: usize) -> Option<usize>;
    fn grapheme_end_after(&self, at: usize) -> Option<usize>;
    fn grapheme_start_at_or_before(&self, at: usize) -> usize;
    fn word_start_before(&self, at: usize) -> usize;
    fn word_end_after(&self, at: usize) -> usize;
    fn word_at(&self, at: usize) -> (usize, usize);
}

impl TextCursor for str {
    fn grapheme_start_at_or_before(&self, at: usize) -> usize {
        let at = at.min(self.len());
        self.grapheme_indices(true)
            .map(|(i, _)| i)
            .take_while(|&i| i <= at)
            .last()
            .unwrap_or(0)
    }

    fn grapheme_start_before(&self, at: usize) -> Option<usize> {
        self[..at.min(self.len())]
            .grapheme_indices(true)
            .next_back()
            .map(|(i, _)| i)
    }

    fn grapheme_end_after(&self, at: usize) -> Option<usize> {
        let at = at.min(self.len());
        self[at..]
            .grapheme_indices(true)
            .next()
            .map(|(i, g)| at + i + g.len())
    }

    /// Start of the word behind the caret: skip any run of separators, then
    /// the word itself — the macOS ⌥← behaviour.
    fn word_start_before(&self, at: usize) -> usize {
        let head = &self[..at.min(self.len())];
        let mut seen_word = false;
        for (i, w) in head.split_word_bound_indices().rev() {
            let is_word = w.chars().any(|c| !c.is_whitespace());
            if is_word {
                seen_word = true;
            } else if seen_word {
                return i + w.len();
            }
            if is_word {
                return i;
            }
        }
        0
    }

    /// The word `at` falls inside, as a range. Landing in the gap between two
    /// words takes the one behind, the way a double-click there does; landing
    /// before the first word takes the one ahead.
    fn word_at(&self, at: usize) -> (usize, usize) {
        let at = at.min(self.len());
        let is_word = |w: &str| w.chars().any(|c| !c.is_whitespace());
        let mut prev: Option<(usize, usize)> = None;
        for (i, w) in self.split_word_bound_indices() {
            let (s, e) = (i, i + w.len());
            if at < e {
                if is_word(w) {
                    return (s, e);
                }
                return prev.unwrap_or_else(|| {
                    // Nothing behind: take the first word ahead, so a click in
                    // leading space still selects something.
                    self[e..]
                        .split_word_bound_indices()
                        .find(|(_, w)| is_word(w))
                        .map(|(j, w)| (e + j, e + j + w.len()))
                        .unwrap_or((s, e))
                });
            }
            if is_word(w) {
                prev = Some((s, e));
            }
        }
        prev.unwrap_or((self.len(), self.len()))
    }

    /// End of the word ahead of the caret, skipping separators first.
    fn word_end_after(&self, at: usize) -> usize {
        let at = at.min(self.len());
        for (i, w) in self[at..].split_word_bound_indices() {
            if w.chars().any(|c| !c.is_whitespace()) {
                return at + i + w.len();
            }
        }
        self.len()
    }
}

/// Squared distance from point `(px, py)` to the segment `a`–`b`. Clamps to the
/// endpoints, so it degrades to point-to-point when the segment has no length.
fn dist2_point_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
    };
    let (cx, cy) = (ax + t * dx, ay + t * dy);
    let (ex, ey) = (px - cx, py - cy);
    ex * ex + ey * ey
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_from_typed_text() {
        let mut g = Graph::new("");
        g.enter_edit();
        g.set_focused_text("hello".into());
        assert_eq!(g.focused().text, "hello");
        g.insert_char('!');
        assert_eq!(g.focused().text, "hello!");
        g.backspace();
        assert_eq!(g.focused().text, "hello");
    }

    #[test]
    fn caret_inserts_and_deletes_in_the_middle() {
        let mut g = Graph::new("");
        g.enter_edit();
        for c in "hello".chars() {
            g.insert_char(c);
        }
        assert_eq!(g.caret(), 5);
        assert!(g.caret_left(false));
        assert!(g.caret_left(false));
        assert_eq!(g.caret(), 3);
        g.insert_char('X');
        assert_eq!(g.focused().text, "helXlo");
        assert_eq!(g.caret(), 4, "caret follows what it inserted");
        g.backspace();
        assert_eq!(g.focused().text, "hello");
        g.delete_forward();
        assert_eq!(g.focused().text, "helo", "forward delete takes what is ahead");
        assert_eq!(g.caret(), 3, "forward delete leaves the caret put");
    }

    #[test]
    fn caret_stops_at_both_ends() {
        let mut g = Graph::new("ab");
        g.enter_edit();
        assert_eq!(g.caret(), 2);
        assert!(!g.caret_right(false), "cannot walk off the end");
        assert!(g.caret_left(false) && g.caret_left(false));
        assert_eq!(g.caret(), 0);
        assert!(!g.caret_left(false), "cannot walk off the start");
        g.backspace();
        assert_eq!(g.focused().text, "ab", "backspace at 0 is a no-op");
        g.delete_forward();
        assert_eq!(g.focused().text, "b");
    }

    #[test]
    fn caret_moves_and_deletes_by_word() {
        let mut g = Graph::new("ship the whole thing");
        g.enter_edit();
        assert!(g.caret_word_left(false));
        assert_eq!(g.caret(), 15, "start of \"thing\"");
        assert!(g.caret_word_left(false));
        assert_eq!(g.caret(), 9, "start of \"whole\"");
        assert!(g.caret_word_right(false));
        assert_eq!(g.caret(), 14, "end of \"whole\"");
        g.caret_end(false);
        g.delete_word_left();
        assert_eq!(g.focused().text, "ship the whole ");
        g.caret_home(false);
        assert_eq!(g.caret(), 0);
        assert!(!g.caret_word_left(false));
    }

    #[test]
    fn shift_arrows_extend_and_unextended_ones_collapse() {
        let mut g = Graph::new("hello there");
        g.enter_edit();
        g.caret_home(false);
        assert_eq!(g.selected_range(), None);

        assert!(g.caret_right(true) && g.caret_right(true));
        assert_eq!(g.selected_text(), Some("he"), "⇧→ grows from the anchor");
        assert!(g.caret_left(true));
        assert_eq!(g.selected_text(), Some("h"), "⇧← shrinks from the same anchor");
        assert!(g.caret_left(true));
        assert_eq!(g.selected_range(), None, "back on the anchor is not a selection");

        // Grow it again, then step off it without ⇧.
        g.caret_word_right(true);
        assert_eq!(g.selected_text(), Some("hello"));
        assert!(g.caret_left(false));
        assert_eq!(g.caret(), 0, "← lands on the left edge, not one back from the caret");
        assert_eq!(g.selected_range(), None);

        g.caret_word_right(true);
        assert!(g.caret_right(false));
        assert_eq!(g.caret(), 5, "→ lands on the right edge");
    }

    #[test]
    fn a_selection_is_replaced_by_whatever_comes_next() {
        let mut g = Graph::new("hello there");
        g.enter_edit();
        g.caret_home(false);
        g.caret_word_right(true);

        g.insert_char('h');
        assert_eq!(g.focused().text, "h there", "typing replaces it");
        assert_eq!(g.caret(), 1);
        assert_eq!(g.selected_range(), None);

        g.select_all();
        g.insert_str("pasted");
        assert_eq!(g.focused().text, "pasted", "paste replaces it too");

        g.select_all();
        g.backspace();
        assert_eq!(g.focused().text, "", "one backspace takes the whole selection");
    }

    #[test]
    fn backspace_over_a_selection_takes_only_the_selection() {
        // The bug this guards: consuming the selection *and* the grapheme
        // before it, which eats a character the user never asked about.
        let mut g = Graph::new("abcd");
        g.enter_edit();
        g.set_caret(3);
        assert!(g.caret_left(true));
        assert_eq!(g.selected_text(), Some("c"));
        g.backspace();
        assert_eq!(g.focused().text, "abd");
        assert_eq!(g.caret(), 2);
    }

    #[test]
    fn double_click_takes_the_word_under_the_pointer() {
        let mut g = Graph::new("ship the whole thing");
        g.enter_edit();
        assert!(g.select_word_at(6));
        assert_eq!(g.selected_text(), Some("the"));
        // Inside the gap after a word: the word behind, as a double-click does.
        g.select_word_at(4);
        assert_eq!(g.selected_text(), Some("ship"));
        // Past the last character: the last word.
        g.select_word_at(20);
        assert_eq!(g.selected_text(), Some("thing"));
        // Leading space has nothing behind it, so take the word ahead.
        g.set_focused_text("  lead".into());
        g.select_word_at(0);
        assert_eq!(g.selected_text(), Some("lead"));
    }

    #[test]
    fn select_all_is_empty_on_an_empty_label() {
        let mut g = Graph::new("");
        g.enter_edit();
        assert!(!g.select_all(), "nothing to select");
        assert_eq!(g.selected_range(), None);
    }

    #[test]
    fn leaving_edit_drops_the_selection() {
        let mut g = Graph::new("hello");
        g.enter_edit();
        g.select_all();
        assert!(g.selected_range().is_some());
        g.leave_edit();
        assert_eq!(g.selected_range(), None, "no band behind a node you only browse");
    }

    #[test]
    fn moving_to_other_text_drops_the_selection() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        g.enter_edit();
        g.select_all();
        g.focus(g.root_id);
        assert_eq!(g.selected_range(), None, "the anchor belonged to the other node");
        g.focus(child);
        g.enter_edit();
        g.select_all();
        let eid = g.edge_to_focused().unwrap();
        g.focus_edge(eid);
        assert_eq!(g.selected_range(), None, "...and again crossing to an edge label");
    }

    #[test]
    fn set_caret_lands_on_a_grapheme_boundary() {
        // "e" + combining acute, then a flag: 3 and 8 bytes, one caret stop each.
        let mut g = Graph::new("e\u{301}\u{1F1F3}\u{1F1F1}x");
        g.enter_edit();
        assert!(g.set_caret(0));
        assert_eq!(g.caret(), 0);
        // Inside the accent cluster and inside the flag — both snap back.
        g.set_caret(1);
        assert_eq!(g.caret(), 0, "mid-accent snaps to the cluster start");
        g.set_caret(7);
        assert_eq!(g.caret(), 3, "mid-flag snaps to the cluster start");
        g.set_caret(11);
        assert_eq!(g.caret(), 11, "the start of the trailing \"x\"");
        assert!(g.set_caret(99), "past the end clamps to the end");
        assert_eq!(g.caret(), 12);
        // A caret set by a click must still be a legal place to type.
        g.insert_char('!');
        assert_eq!(g.focused().text, "e\u{301}\u{1F1F3}\u{1F1F1}x!");
    }

    #[test]
    fn set_caret_addresses_the_active_text() {
        let mut g = Graph::new("root");
        g.create_from_focused(Direction::Right, "child");
        let eid = g.edge_to_focused().unwrap();
        g.focus_edge(eid);
        g.enter_edit();
        g.set_focused_text("because".into());
        g.set_caret(3);
        assert_eq!(g.caret(), 3);
        g.insert_char('-');
        assert_eq!(g.edges[&eid].label, "bec-ause", "the edge label, not the node");
    }

    #[test]
    fn editing_is_grapheme_safe() {
        // A char-at-a-time backspace splits these into mojibake.
        let mut g = Graph::new("");
        g.enter_edit();
        // "e" + U+0301 COMBINING ACUTE — two scalars, one cluster on screen.
        let decomposed = "e\u{301}";
        for c in decomposed.chars() {
            g.insert_char(c);
        }
        g.insert_char('x');
        assert_eq!(g.focused().text, format!("{decomposed}x"));
        g.backspace();
        assert_eq!(g.focused().text, decomposed);
        g.backspace();
        assert_eq!(g.focused().text, "", "the accent went with its letter");

        for c in "👍🏽!".chars() {
            g.insert_char(c);
        }
        g.backspace();
        g.backspace();
        assert_eq!(g.focused().text, "", "skin tone went with the thumb");
    }

    #[test]
    fn caret_follows_the_active_text() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        g.enter_edit();
        g.caret_home(false);
        assert_eq!(g.caret(), 0);
        // Moving focus rebinds the caret to the newly active text.
        g.focus(g.root_id);
        assert_eq!(g.caret(), 4, "end of \"root\"");
        g.focus(child);
        assert_eq!(g.caret(), 5, "end of \"child\"");
    }

    #[test]
    fn directional_creates() {
        let mut g = Graph::new("root");
        let r = g.create_from_focused(Direction::Right, "R").unwrap();
        assert!(g.nodes[&r].x > 0.0);
        g.focus(g.root_id);
        let u = g.create_from_focused(Direction::Up, "U").unwrap();
        assert!(g.nodes[&u].y < 0.0);
        g.focus(g.root_id);
        let d = g.create_from_focused(Direction::Down, "D").unwrap();
        assert!(g.nodes[&d].y > 0.0);
        assert_eq!(g.nodes.len(), 4);
        assert_eq!(g.edges.len(), 3);
    }

    #[test]
    fn no_overlap_multi_child() {
        let mut g = Graph::new("root");
        g.create_from_focused(Direction::Right, "a");
        g.focus(g.root_id);
        g.create_from_focused(Direction::Right, "b");
        g.focus(g.root_id);
        g.create_from_focused(Direction::Up, "u");
        g.focus(g.root_id);
        g.create_from_focused(Direction::Down, "d");
        g.focus(g.root_id);
        g.create_from_focused(Direction::Right, "c");
        assert!(!g.has_overlapping_nodes());
    }

    #[test]
    fn demo_no_overlap() {
        let g = Graph::demo();
        assert!(!g.is_editing(), "a map is handed over in Browse");
        assert!(!g.has_overlapping_nodes());
        assert!(g.nodes.len() >= 5);
    }

    #[test]
    fn browse_mode_is_read_only() {
        let mut g = Graph::new("x");
        assert!(!g.is_editing(), "a map opens in Browse");
        g.insert_char('z');
        assert_eq!(g.focused().text, "x", "typing is gated on Edit");
        g.set_focused_text("wiped".into());
        assert_eq!(g.focused().text, "x");
        g.backspace();
        assert_eq!(g.focused().text, "x");
        g.delete_forward();
        assert_eq!(g.focused().text, "x");
        // Navigating must not drop into Edit by itself.
        g.focus(g.root_id);
        assert!(!g.is_editing());
        g.enter_edit();
        g.insert_char('z');
        assert_eq!(g.focused().text, "xz");
        g.leave_edit();
        g.insert_char('q');
        assert_eq!(g.focused().text, "xz", "leaving Edit closes the door again");
    }

    #[test]
    fn create_clears_edge_selection() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        let eid = g.edge_to_focused().unwrap();
        assert!(g.focus_edge(eid));
        assert_eq!(g.selection(), Selection::Edge(eid));
        // Model must clear edge focus even without UI pre-clear.
        let grand = g.create_from_focused(Direction::Right, "grand").unwrap();
        assert_eq!(g.selection(), Selection::Node(grand));
        assert!(g.focused_edge.is_none());
        g.insert_char('!');
        assert_eq!(g.nodes[&grand].text, "grand!");
        assert_eq!(g.edges[&eid].label, "");
        assert_eq!(g.nodes[&child].text, "child");
    }

    /// Nodes are keyed by id, so a test that knows a label looks it up.
    fn by_text(g: &Graph, text: &str) -> u64 {
        g.nodes
            .iter()
            .find(|(_, n)| n.text == text)
            .map(|(&id, _)| id)
            .unwrap_or_else(|| panic!("no node labelled {text:?}"))
    }

    /// The UI measures real shaped text; tests stand in a fixed half-width.
    fn hit(g: &Graph, wx: f32, wy: f32, half_w: f32) -> Option<u64> {
        g.hit_test_with(wx, wy, |_, _| (half_w, layout::NODE_HALF_H))
    }

    #[test]
    fn hit_box_follows_the_width_it_is_given() {
        let mut g = Graph::new("root");
        g.enter_edit();
        g.set_focused_text("a-very-long-mind-map-node-title".into());
        let id = g.focused_id;
        let n = &g.nodes[&id];
        let (x, y) = (n.x, n.y);
        assert_eq!(hit(&g, x + 40.0, y, 60.0), Some(id), "inside a wide box");
        assert_eq!(hit(&g, x + 40.0, y, 20.0), None, "outside a narrow one");
    }

    #[test]
    fn focus_parent_and_siblings_enable_branching() {
        // Shipped UI path: create → focus_parent (←/Esc) → create again = two siblings.
        let mut g = Graph::new("root");
        let root = g.root_id;
        let a = g.create_from_focused(Direction::Right, "a").unwrap();
        assert_eq!(g.focused_id, a);
        // Without focus_parent, stuck on a — cannot create sibling of a from root.
        assert!(g.focus_parent());
        assert_eq!(g.focused_id, root);
        let b = g.create_from_focused(Direction::Right, "b").unwrap();
        assert_eq!(g.focused_id, b);
        // Both children share the same parent (branching, not a single path).
        assert_eq!(g.nodes[&a].parent, Some(root));
        assert_eq!(g.nodes[&b].parent, Some(root));
        let root_kids = g.children_of(root);
        assert_eq!(root_kids.len(), 2);
        assert!(root_kids.contains(&a) && root_kids.contains(&b));
        // siblings under root: a, b
        assert!(g.focus_sibling(-1));
        assert_eq!(g.focused_id, a);
        assert!(g.focus_sibling(1));
        assert_eq!(g.focused_id, b);
        // Grow from a (deeper branch)
        g.focus(a);
        let c = g.create_from_focused(Direction::Right, "c").unwrap();
        assert_eq!(g.nodes[&c].parent, Some(a));
        assert!(!g.has_overlapping_nodes());
    }

    #[test]
    fn hit_test_selects_node() {
        let mut g = Graph::new("root");
        let r = g.create_from_focused(Direction::Right, "child").unwrap();
        let child = &g.nodes[&r];
        let (x, y) = (child.x, child.y);
        assert_eq!(hit(&g, x, y, 48.0), Some(r));
        assert_eq!(hit(&g, 9999.0, 9999.0, 48.0), None);
    }

    /// Built here rather than from `demo()` so a nicer demo seed can't silently
    /// change what "arrow keys navigate" is asserting.
    fn nav_fixture() -> Graph {
        let mut g = Graph::new("root");
        let root = g.root_id;
        g.create_child(root, Direction::Right, "ideas");
        g.create_child(root, Direction::Up, "goals");
        g.create_child(root, Direction::Down, "notes");
        let ideas = by_text(&g, "ideas");
        g.create_child(ideas, Direction::Right, "ship");
        g.focus(root);
        g
    }

    #[test]
    fn arrows_navigate_through_nodes() {
        let mut g = nav_fixture();
        let root = g.root_id;
        // Right → ideas (child to the right)
        assert!(g.navigate(Direction::Right));
        let ideas = g.focused_id;
        assert_eq!(g.nodes[&ideas].text, "ideas");
        // Right → ship
        assert!(g.navigate(Direction::Right));
        assert_eq!(g.focused().text, "ship");
        // Left → ideas, Left → root (parent)
        assert!(g.navigate(Direction::Left));
        assert_eq!(g.focused().text, "ideas");
        assert!(g.navigate(Direction::Left));
        assert_eq!(g.focused_id, root);
        // Up → goals (structural/spatial above root)
        assert!(g.navigate(Direction::Up));
        assert_eq!(g.focused().text, "goals");
        // Down from goals → another node below (sibling notes or ideas, or root via spatial)
        assert!(g.navigate(Direction::Down));
        assert_ne!(g.focused().text, "goals");
        // Full pass: from notes go left to root
        g.focus(root);
        assert!(g.navigate(Direction::Down));
        assert_eq!(g.focused().text, "notes");
        assert!(g.navigate(Direction::Left));
        assert_eq!(g.focused_id, root);
    }

    #[test]
    fn clicking_away_leaves_you_holding_nothing() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        g.focus(ideas);
        assert_eq!(g.acting_on(), vec![ideas], "a click picks that node");

        // Empty ground. "Nothing selected" has to be a state the model can be
        // in, or clicking away cannot mean anything.
        assert!(g.deselect_all());
        assert!(g.acting_on().is_empty(), "delete and drag now have no subject");
        assert!(!g.deselect_all(), "already empty");

        // The cursor stays, so the arrows still have somewhere to start.
        assert_eq!(g.focused_id, ideas);
        assert!(g.navigate(Direction::Left));
        assert_eq!(g.acting_on(), vec![g.root_id], "navigating picks up again");
    }

    #[test]
    fn shift_click_adds_and_removes() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        let goals = by_text(&g, "goals");
        g.focus(ideas);

        g.toggle_node_selected(notes);
        assert_eq!(g.acting_on(), sorted(vec![ideas, notes]));
        assert_eq!(g.focused_id, notes, "the cursor follows what you just added");

        g.toggle_node_selected(goals);
        assert_eq!(g.acting_on(), sorted(vec![ideas, notes, goals]));

        // Removing the cursor's own node hands the cursor to a survivor.
        g.toggle_node_selected(goals);
        assert_eq!(g.acting_on(), sorted(vec![ideas, notes]));
        assert!(g.nodes.contains_key(&g.focused_id));

        g.toggle_node_selected(notes);
        assert_eq!(g.acting_on(), vec![ideas], "one node is still a selection");
        g.toggle_node_selected(ideas);
        assert!(g.acting_on().is_empty(), "⇧-clicking the last one lets go of it");
    }

    #[test]
    fn a_plain_focus_resets_the_selection() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        g.focus(ideas);
        g.toggle_node_selected(notes);
        assert_eq!(g.acting_on().len(), 2);
        g.focus(ideas);
        assert_eq!(g.acting_on(), vec![ideas], "clicking one node picks one node");
    }

    #[test]
    fn a_marquee_replaces_the_selection_and_an_empty_one_clears_it() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        let goals = by_text(&g, "goals");
        g.set_node_selection(vec![ideas, notes, goals]);
        assert_eq!(g.acting_on(), sorted(vec![ideas, notes, goals]));
        g.set_node_selection(vec![notes]);
        assert_eq!(g.acting_on(), vec![notes]);
        assert_eq!(g.focused_id, notes, "the cursor moves into the selection");
        // A rectangle that caught nothing selects nothing.
        g.set_node_selection(Vec::new());
        assert!(g.acting_on().is_empty());
        assert_eq!(g.focused_id, notes, "but the cursor stays put");
    }

    #[test]
    fn a_marquee_ignores_ids_that_are_not_there() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        g.set_node_selection(vec![ideas, notes, 9999]);
        assert_eq!(g.acting_on(), sorted(vec![ideas, notes]));
    }

    #[test]
    fn deleting_a_selection_counts_each_node_once() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let notes = by_text(&g, "notes");
        // `ship` is inside `ideas`, so a naive sum would count it twice.
        g.set_node_selection(vec![ideas, ship, notes]);
        let n = g.nodes.len();
        assert_eq!(g.delete_selection_of_nodes(), 3);
        assert_eq!(g.nodes.len(), n - 3);
        assert!(g.selected_nodes().is_empty());
        assert!(
            g.edges.values().all(|e| g.nodes.contains_key(&e.from)
                && g.nodes.contains_key(&e.to)),
            "no dangling edges"
        );
    }

    #[test]
    fn deleting_a_selection_that_holds_the_root_keeps_the_root() {
        let mut g = nav_fixture();
        let notes = by_text(&g, "notes");
        g.set_node_selection(vec![g.root_id, notes]);
        assert_eq!(g.delete_selection_of_nodes(), 1);
        assert!(g.nodes.contains_key(&g.root_id));
        assert!(!g.nodes.contains_key(&notes));
    }

    #[test]
    fn a_rectangle_picks_up_the_nodes_inside_it() {
        let mut g = Graph::new("root");
        let a = g.create_child(g.root_id, Direction::Right, "a").unwrap();
        g.nodes.get_mut(&a).unwrap().x = 500.0;
        g.nodes.get_mut(&a).unwrap().y = 500.0;
        let hw = |_: u64, _: &Node| (40.0, layout::NODE_HALF_H);
        assert_eq!(g.nodes_in_rect(400.0, 400.0, 600.0, 600.0, hw), vec![a]);
        // Backwards corners describe the same rectangle.
        assert_eq!(g.nodes_in_rect(600.0, 600.0, 400.0, 400.0, hw), vec![a]);
        // Empty ground, well clear of both rings.
        assert!(g.nodes_in_rect(-500.0, -500.0, -450.0, -450.0, hw).is_empty());
        // A rectangle only touching the ring still counts — you aimed at it.
        assert!(g.nodes_in_rect(-10.0, -10.0, -5.0, -5.0, hw).contains(&g.root_id));
    }

    #[test]
    fn a_selection_cannot_outlive_the_nodes_it_names() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        g.set_node_selection(vec![ideas, notes]);
        g.nodes.remove(&notes);
        g.sanitize();
        assert_eq!(g.acting_on(), vec![ideas], "the dead id is gone, the live one stays");
        assert!(g.nodes.contains_key(&g.focused_id));
    }

    fn sorted(mut v: Vec<u64>) -> Vec<u64> {
        v.sort_unstable();
        v
    }

    #[test]
    fn a_nodes_hit_target_follows_its_own_ring_height() {
        // Paint sized the ring from the font while hit-testing used a fixed
        // constant, so the root — drawn at a larger size — stood taller than
        // the rectangle you could press. Both now come from one measurement,
        // which means this callback has to be believed.
        let g = Graph::new("root");
        let root = g.root_id;
        let tall = |_: u64, _: &Node| (40.0, 30.0);
        let short = |_: u64, _: &Node| (40.0, 8.0);
        let just_below = (0.0, 20.0);
        assert_eq!(
            g.hit_test_with(just_below.0, just_below.1, tall),
            Some(root),
            "inside a tall ring"
        );
        assert_eq!(
            g.hit_test_with(just_below.0, just_below.1, short),
            None,
            "outside a short one"
        );
        // The fit uses the same extents, so a tall node is framed, not clipped.
        let (_, y0, _, y1) = g.bounds_with(tall);
        assert!(y1 - y0 >= 60.0, "the fit accounts for the height it was given");
    }

    #[test]
    fn find_matches_on_any_part_of_a_label_ignoring_case() {
        let g = nav_fixture();
        let ship = by_text(&g, "ship");
        assert_eq!(g.find("SHIP"), vec![ship], "case does not matter");
        assert_eq!(g.find("hi"), vec![ship], "nor does matching mid-word");
        assert!(g.find("").is_empty(), "an empty query matches nothing, not everything");
        assert!(g.find("   ").is_empty());
        assert!(g.find("nothing here").is_empty());
        // Stable order, so stepping through matches is repeatable.
        assert_eq!(g.find("o"), {
            let mut v = g.find("o");
            v.sort_unstable();
            v
        });
    }

    #[test]
    fn find_reaches_into_a_folded_branch_and_reveals_it() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.toggle_collapsed(ideas);
        assert!(g.is_hidden(ship));

        // Being unable to find what you folded is the case find exists for.
        assert_eq!(g.find("ship"), vec![ship]);
        assert!(g.reveal(ship), "going to it unfolds what was hiding it");
        assert!(!g.is_hidden(ship));
        assert!(!g.is_collapsed(ideas));
        assert!(!g.reveal(ship), "nothing left to unfold");
    }

    #[test]
    fn folding_hides_the_branch_from_everything_that_looks_at_the_map() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");

        assert!(g.toggle_collapsed(ideas));
        assert!(g.is_collapsed(ideas));
        assert!(g.is_hidden(ship), "the child is folded away");
        assert!(!g.is_hidden(ideas), "but the node you fold stays visible");
        assert!(g.hidden_set().contains(&ship));
        assert_eq!(g.hidden_count(ideas), 1);

        // Hit-testing walks past it...
        let hw = |_: u64, _: &Node| (40.0, layout::NODE_HALF_H);
        let (sx, sy) = (g.nodes[&ship].x, g.nodes[&ship].y);
        assert_eq!(g.hit_test_with(sx, sy, hw), None);
        // ...as does the marquee...
        assert!(!g.nodes_in_rect(sx - 60.0, sy - 60.0, sx + 60.0, sy + 60.0, hw).contains(&ship));
        // ...and the fit, which would otherwise frame empty space.
        let (x0, _, x1, _) = g.bounds_with(hw);
        assert!(x1 - x0 > 0.0);

        // Unfolding puts it back exactly as it was.
        assert!(g.toggle_collapsed(ideas));
        assert!(!g.is_hidden(ship));
        assert_eq!(g.hit_test_with(sx, sy, hw), Some(ship));
    }

    #[test]
    fn tidy_does_not_tear_a_folded_branch_off_its_parent() {
        // ⌘R with anything folded used to park every hidden node in a column
        // at x=0: the branch was torn off its parent, edges crossing the whole
        // plate, and `bounds_with` skips hidden nodes so no test could see it.
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.toggle_collapsed(ideas);
        g.tidy();

        let (ix, iy) = (g.nodes[&ideas].x, g.nodes[&ideas].y);
        let (sx, sy) = (g.nodes[&ship].x, g.nodes[&ship].y);
        assert!(
            (sx - ix).abs() < layout::TIDY_COL * 2.0 && (sy - iy).abs() < layout::TIDY_ROW * 4.0,
            "the hidden child stayed with its parent: parent ({ix},{iy}) child ({sx},{sy})"
        );
        // Unfolding shows it in a sane place, not stranded at the origin.
        g.toggle_collapsed(ideas);
        assert!(!g.is_hidden(ship));
    }

    #[test]
    fn tidy_leaves_a_folded_root_and_free_nodes_alone() {
        let mut g = nav_fixture();
        // A folded root hides the whole map; it must not flatten into one column.
        g.toggle_collapsed(g.root_id);
        g.tidy();
        let xs: HashSet<i32> = g.nodes.values().map(|n| n.x as i32).collect();
        assert!(xs.len() > 1, "the folded map kept its shape, not one column");

        // A free node is deliberately parentless, not corruption to be parked.
        let mut g = Graph::new("root");
        let free = g.create_free_node(400.0, -300.0).expect("a node");
        g.leave_edit();
        g.tidy();
        assert_eq!(
            (g.nodes[&free].x, g.nodes[&free].y),
            (400.0, -300.0),
            "a node you placed by hand stays where you put it"
        );
    }

    #[test]
    fn moving_a_folded_node_carries_what_it_hides() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.toggle_collapsed(ideas);
        let before = g.nodes[&ship].x;
        // Only the folded node itself can be clicked or swept, so moving it has
        // to bring the branch — otherwise unfolding reveals it stranded.
        g.focus(ideas);
        assert!(g.move_selection_by(300.0, 0.0));
        assert_eq!(g.nodes[&ship].x, before + 300.0);
    }

    #[test]
    fn nothing_puts_the_cursor_inside_a_folded_branch() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");

        // Creating into a fold used to hide the new node *and* leave you
        // typing into it.
        g.toggle_collapsed(ideas);
        g.focus(ideas);
        let child = g.create_from_focused(Direction::Right, "").expect("a child");
        assert!(!g.is_hidden(child), "creating opened the fold");
        assert!(!g.is_collapsed(ideas));

        // Pasting into a fold, likewise.
        g.toggle_collapsed(ideas);
        let clip = g.copy_subtrees(&[by_text(&g, "notes")]).unwrap();
        g.focus(ideas);
        let fresh = g.paste(&clip, 0.0, 0.0, Some(ideas));
        assert!(fresh.iter().all(|&f| !g.is_hidden(f)), "pasting opened the fold");

        // And dropping a branch onto one.
        g.toggle_collapsed(ideas);
        let goals = by_text(&g, "goals");
        assert!(g.reparent(goals, ideas));
        assert!(!g.is_hidden(goals), "the dropped branch is visible where it landed");
    }

    #[test]
    fn folding_over_the_cursor_leaves_you_standing_on_the_fold() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.focus(ship);
        g.toggle_collapsed(ideas);
        assert!(!g.is_hidden(g.focused_id), "the cursor came out with the fold");
        assert_eq!(g.focused_id, ideas);
        assert!(
            g.acting_on().iter().all(|&s| !g.is_hidden(s)),
            "and so did the selection"
        );
        // From here the arrows cannot walk back in.
        g.navigate(Direction::Right);
        assert!(!g.is_hidden(g.focused_id));
    }

    #[test]
    fn a_folded_branch_copies_folded() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        g.toggle_collapsed(ideas);
        let clip = g.copy_subtrees(&[ideas]).unwrap();
        // Round-trip through text too — a folded map copied between windows
        // used to arrive exploded.
        let clip = Clip::from_text(&clip.to_text()).expect("parses back");
        let fresh = g.paste(&clip, 900.0, 900.0, None);
        assert!(g.is_collapsed(fresh[0]), "the copy arrived folded, like its original");
    }

    #[test]
    fn a_folded_branch_is_still_part_of_the_document() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.toggle_collapsed(ideas);

        // Folding is a view, not an edit: the branch still copies, moves and
        // deletes with its parent.
        assert_eq!(g.copy_subtrees(&[ideas]).unwrap().len(), 2);
        let before = g.nodes[&ship].x;
        g.focus(ideas);
        g.select_subtree_of(ideas);
        g.move_selection_by(50.0, 0.0);
        assert_eq!(g.nodes[&ship].x, before + 50.0);
        g.set_node_selection(vec![ideas]);
        assert_eq!(g.delete_selection_of_nodes(), 2, "the hidden child goes too");
    }

    #[test]
    fn folding_refuses_a_node_with_nothing_under_it() {
        let mut g = nav_fixture();
        let ship = by_text(&g, "ship");
        assert!(!g.toggle_collapsed(ship), "a fold that hides nothing is not a control");
        assert!(!g.is_collapsed(ship));
    }

    #[test]
    fn arrows_do_not_walk_into_a_folded_branch() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        g.toggle_collapsed(ideas);
        g.focus(ideas);
        g.navigate(Direction::Right);
        assert_ne!(g.focused_id, ship, "→ cannot step into what is folded away");
        assert!(!g.is_hidden(g.focused_id), "and never lands on a hidden node");
    }

    #[test]
    fn a_folded_branch_costs_one_row_in_tidy() {
        let mut g = Graph::new("root");
        let a = g.create_child(g.root_id, Direction::Right, "a").unwrap();
        for i in 0..6 {
            g.create_child(a, Direction::Right, &format!("k{i}"));
        }
        let b = g.create_child(g.root_id, Direction::Right, "b").unwrap();
        let _ = b;
        g.tidy();
        let height = |g: &Graph| {
            let (_, y0, _, y1) = g.bounds_with(|_, _| (40.0, layout::NODE_HALF_H));
            y1 - y0
        };
        let open = height(&g);
        g.toggle_collapsed(a);
        g.tidy();
        let folded = height(&g);
        assert!(
            folded < open,
            "folding has to buy back the height: {folded} vs {open}"
        );
    }

    #[test]
    fn reparenting_moves_a_branch_and_keeps_both_halves_of_the_tree_agreeing() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let notes = by_text(&g, "notes");

        assert!(g.reparent(ideas, notes));
        assert_eq!(g.nodes[&ideas].parent, Some(notes));
        assert_eq!(g.nodes[&ship].parent, Some(ideas), "the subtree came along");
        // Exactly one edge describes the new link, and none describes the old.
        let links: HashSet<(u64, u64)> = g.edges.values().map(|e| (e.from, e.to)).collect();
        let parents: HashSet<(u64, u64)> = g
            .nodes
            .iter()
            .filter_map(|(&id, n)| n.parent.map(|p| (p, id)))
            .collect();
        assert_eq!(links, parents);
        assert_eq!(g.subtree_ids(notes).len(), 3, "notes now owns the branch");
    }

    #[test]
    fn a_branch_cannot_be_dropped_inside_itself() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        // This is the move that would turn the tree into a ring and hang every
        // traversal, so it has to be refused before it is offered.
        assert!(!g.can_reparent(ideas, ship));
        assert!(!g.reparent(ideas, ship));
        assert!(!g.can_reparent(ideas, ideas), "nor onto itself");
        assert!(!g.can_reparent(g.root_id, ideas), "nor the root anywhere");
        assert!(
            !g.can_reparent(ship, ideas),
            "nor onto the parent it already has — nothing to do"
        );
        // The map is untouched, and still walkable.
        assert_eq!(g.subtree_ids(g.root_id).len(), g.nodes.len());
    }

    #[test]
    fn dropping_a_parent_and_its_child_together_re_hangs_only_the_parent() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let notes = by_text(&g, "notes");
        // Both are being carried; `ship` rides along inside `ideas` and must
        // not also be re-hung, or it would leave the branch it came with.
        let roots = g.reparent_roots(&[ideas, ship]);
        assert_eq!(roots, vec![ideas]);
        for r in roots {
            g.reparent(r, notes);
        }
        assert_eq!(g.nodes[&ship].parent, Some(ideas));
        assert_eq!(g.nodes[&ideas].parent, Some(notes));
    }

    #[test]
    fn a_reparented_edge_keeps_its_label() {
        let mut g = Graph::new("root");
        let a = g.create_child(g.root_id, Direction::Right, "a").unwrap();
        let b = g.create_child(g.root_id, Direction::Down, "b").unwrap();
        let eid = *g.edges.iter().find(|(_, e)| e.to == a).map(|(i, _)| i).unwrap();
        g.edges.get_mut(&eid).unwrap().label = "because".into();
        assert!(g.reparent(a, b));
        let e = g.edges.values().find(|e| e.to == a).expect("an edge");
        assert_eq!(e.label, "because", "the label describes the link, so it moves with it");
        assert_eq!(e.from, b);
    }

    #[test]
    fn a_sibling_lands_beside_you_under_the_same_parent() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        g.focus(ideas);
        let parent = g.nodes[&ideas].parent;
        let sib = g.create_sibling("beside").expect("a sibling");
        assert_eq!(g.nodes[&sib].parent, parent, "same parent, not a child");
        assert_eq!(g.focused_id, sib, "and you are on it, ready to type");
        assert!(g.is_editing());
        assert_eq!(g.acting_on(), vec![sib]);
    }

    #[test]
    fn a_sibling_of_the_root_becomes_a_child_of_it() {
        // The root has no parent to share, and a dead key is worse than a
        // sensible fallback.
        let mut g = Graph::new("root");
        let id = g.create_sibling("first").expect("a node");
        assert_eq!(g.nodes[&id].parent, Some(g.root_id));
    }

    #[test]
    fn a_no_op_edit_reports_that_it_would_do_nothing() {
        // Each of these guards a path that would otherwise snapshot the graph
        // and throw away the redo stack for an edit that changes nothing.
        let mut g = Graph::new("abc");
        g.enter_edit();
        g.caret_end(false);
        assert!(g.can_backspace());
        assert!(!g.can_delete_forward(), "nothing ahead of the caret");
        assert!(g.can_delete_word_left());
        g.caret_home(false);
        assert!(!g.can_backspace(), "nothing behind it either");
        assert!(g.can_delete_forward());
        assert!(!g.can_delete_word_left());
        // A selection makes all three live again, whatever the caret is on.
        g.select_all();
        assert!(g.can_backspace() && g.can_delete_forward() && g.can_delete_word_left());
    }

    #[test]
    fn a_drag_moves_the_set_it_started_with() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let notes = by_text(&g, "notes");
        g.focus(ideas);
        // What a press captures.
        let held = g.acting_on();
        // ...and then something re-points the cursor mid-gesture.
        g.focus(notes);
        let notes_before = (g.nodes[&notes].x, g.nodes[&notes].y);
        let ideas_before = (g.nodes[&ideas].x, g.nodes[&ideas].y);
        g.move_nodes_by(&held, 40.0, 0.0);
        assert_eq!(g.nodes[&ideas].x, ideas_before.0 + 40.0, "the grabbed node moved");
        assert_eq!(
            (g.nodes[&notes].x, g.nodes[&notes].y),
            notes_before,
            "and nothing else did, however the selection changed"
        );
    }

    #[test]
    fn an_empty_map_is_framed_rather_than_measured() {
        let mut g = Graph::new("root");
        // Every position NaN: the running min/max never move off their
        // sentinels, which are finite but describe a backwards rectangle.
        g.nodes.get_mut(&g.root_id).unwrap().x = f32::NAN;
        g.nodes.get_mut(&g.root_id).unwrap().y = f32::NAN;
        let (x0, y0, x1, y1) = g.bounds_with(|_, _| (40.0, layout::NODE_HALF_H));
        assert!(x0 < x1 && y0 < y1, "a usable rectangle: {x0},{y0} .. {x1},{y1}");
        assert!(x0.is_finite() && y1.is_finite());
    }

    #[test]
    fn a_copied_branch_pastes_as_a_new_branch() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let before = g.nodes.len();
        let clip = g.copy_subtrees(&[ideas]).expect("a clip");
        assert_eq!(clip.len(), 2, "the node and its child");

        g.focus(g.root_id);
        let fresh = g.paste(&clip, 500.0, 500.0, Some(g.root_id));
        assert_eq!(fresh.len(), 2);
        assert_eq!(g.nodes.len(), before + 2, "the original is untouched");
        // The copy is a real branch: its root hangs off the paste target and
        // its internal shape survived.
        let root_of_clip = fresh[0];
        assert_eq!(g.nodes[&root_of_clip].parent, Some(g.root_id));
        assert_eq!(g.nodes[&root_of_clip].text, "ideas");
        assert_eq!(g.subtree_ids(root_of_clip).len(), 2);
        // Edges mirror parents, which is the invariant `sanitize` enforces.
        let links: HashSet<(u64, u64)> = g.edges.values().map(|e| (e.from, e.to)).collect();
        let parents: HashSet<(u64, u64)> = g
            .nodes
            .iter()
            .filter_map(|(&id, n)| n.parent.map(|p| (p, id)))
            .collect();
        assert_eq!(links, parents);
        assert_eq!(g.acting_on(), sorted(fresh), "you are holding what you pasted");
    }

    #[test]
    fn a_clip_survives_a_round_trip_through_text() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        // The clipboard carries text, so the clip has to be able to be one.
        let text = g.copy_subtrees(&[ideas]).unwrap().to_text();
        let clip = Clip::from_text(&text).expect("parses back");
        assert_eq!(clip.len(), 2);
        assert_eq!(g.paste(&clip, 0.0, 0.0, None).len(), 2);
        assert!(Clip::from_text("not json").is_none());
        assert!(Clip::from_text("{\"nodes\":[]}").is_none(), "an empty clip is no clip");
    }

    #[test]
    fn pasting_relative_positions_keeps_the_shape() {
        let mut g = Graph::new("root");
        let a = g.create_child(g.root_id, Direction::Right, "a").unwrap();
        let b = g.create_child(a, Direction::Right, "b").unwrap();
        g.nodes.get_mut(&a).unwrap().x = 100.0;
        g.nodes.get_mut(&a).unwrap().y = 10.0;
        g.nodes.get_mut(&b).unwrap().x = 260.0;
        g.nodes.get_mut(&b).unwrap().y = 40.0;
        let clip = g.copy_subtrees(&[a]).unwrap();
        let fresh = g.paste(&clip, 1000.0, 1000.0, Some(g.root_id));
        let (na, nb) = (&g.nodes[&fresh[0]], &g.nodes[&fresh[1]]);
        assert_eq!((na.x, na.y), (1000.0, 1000.0), "the clip lands where asked");
        assert_eq!(
            (nb.x - na.x, nb.y - na.y),
            (160.0, 30.0),
            "the offset between them is preserved"
        );
    }

    #[test]
    fn copying_a_parent_and_its_child_does_not_duplicate_the_child() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        // `ship` is inside `ideas`; a naive pass would take it twice.
        let clip = g.copy_subtrees(&[ideas, ship]).unwrap();
        assert_eq!(clip.len(), 2);
    }

    #[test]
    fn a_double_click_on_empty_ground_makes_a_reachable_node() {
        let mut g = Graph::new("root");
        let id = g.create_free_node(400.0, -200.0).expect("a node");
        assert_eq!((g.nodes[&id].x, g.nodes[&id].y), (400.0, -200.0));
        assert_eq!(g.nodes[&id].parent, None, "a floating topic, not a child");
        assert!(g.is_editing(), "ready to be named");
        assert_eq!(g.acting_on(), vec![id]);
        // Reachable: navigation falls back to the nearest node in the
        // half-plane, which does not care about the tree.
        g.leave_edit();
        g.focus(g.root_id);
        assert!(g.navigate(Direction::Right));
        assert_eq!(g.focused_id, id);
    }

    #[test]
    fn drag_moves_only_the_node_it_grabbed() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let ideas_before = (g.nodes[&ideas].x, g.nodes[&ideas].y);
        let ship_before = (g.nodes[&ship].x, g.nodes[&ship].y);

        g.focus(ideas);
        assert!(g.move_selection_by(400.0, -120.0));
        assert_eq!(
            (g.nodes[&ideas].x, g.nodes[&ideas].y),
            (ideas_before.0 + 400.0, ideas_before.1 - 120.0)
        );
        assert_eq!(
            (g.nodes[&ship].x, g.nodes[&ship].y),
            ship_before,
            "children stay put on a plain drag"
        );
    }

    #[test]
    fn selecting_a_subtree_then_moving_carries_descendants() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let root_before = (g.nodes[&g.root_id].x, g.nodes[&g.root_id].y);
        let before: Vec<(f32, f32)> = [ideas, ship]
            .iter()
            .map(|id| (g.nodes[id].x, g.nodes[id].y))
            .collect();

        // "Drag the branch" is now "select the branch, then drag" — there is
        // no hidden subtree modifier for a drag to latch.
        g.focus(ideas);
        g.select_subtree_of(ideas);
        assert!(g.move_selection_by(60.0, -25.0));
        for (id, was) in [ideas, ship].iter().zip(before) {
            assert_eq!(g.nodes[id].x, was.0 + 60.0);
            assert_eq!(g.nodes[id].y, was.1 - 25.0);
        }
        assert_eq!(
            (g.nodes[&g.root_id].x, g.nodes[&g.root_id].y),
            root_before,
            "the parent was not selected, so it stayed"
        );
    }

    #[test]
    fn a_move_with_nothing_selected_moves_nothing() {
        let mut g = Graph::new("root");
        let start = (g.focused().x, g.focused().y);
        // A fresh map is holding its root, so the nudge lands.
        assert!(g.move_selection_by(layout::SNAP, 0.0));
        assert_eq!(g.focused().x, start.0 + layout::SNAP);

        // Click away, and the same nudge has nothing to act on.
        g.deselect_all();
        let now = (g.focused().x, g.focused().y);
        assert!(!g.move_selection_by(layout::SNAP, 0.0));
        assert_eq!((g.focused().x, g.focused().y), now, "the cursor is not a selection");
    }

    #[test]
    fn snap_rounds_to_the_grid() {
        assert_eq!(Graph::snap(0.0), 0.0);
        assert_eq!(Graph::snap(layout::SNAP * 2.4), layout::SNAP * 2.0);
        assert_eq!(Graph::snap(-layout::SNAP * 1.6), -layout::SNAP * 2.0);
    }

    #[test]
    fn delete_takes_the_subtree_and_its_edges() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        assert_eq!(g.subtree_ids(ideas), {
            let mut v = vec![ideas, ship];
            v.sort();
            v
        });

        g.focus(ship);
        assert!(g.delete_subtree(ideas));
        assert!(!g.nodes.contains_key(&ideas));
        assert!(!g.nodes.contains_key(&ship));
        assert_eq!(g.focused_id, g.root_id, "focus lands on the parent");
        assert!(
            g.edges.values().all(|e| g.nodes.contains_key(&e.from)
                && g.nodes.contains_key(&e.to)),
            "no dangling edges"
        );
        assert_eq!(g.nodes.len(), 3);
    }

    #[test]
    fn root_is_never_deleted() {
        let mut g = Graph::new("root");
        assert!(!g.delete_subtree(g.root_id));
        assert_eq!(g.delete_selection_of_nodes(), 0, "the root is never deleted");
        assert_eq!(g.nodes.len(), 1);
    }

    #[test]
    fn delete_clears_a_focused_edge_that_went_with_it() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        let eid = g.edge_to_focused().unwrap();
        assert!(g.focus_edge(eid));
        assert!(g.delete_subtree(child));
        assert!(g.focused_edge.is_none());
        assert_eq!(g.selection(), Selection::Node(g.root_id));
    }

    #[test]
    fn tidy_lays_out_a_readable_tree() {
        let mut g = Graph::new("root");
        let root = g.root_id;
        let a = g.create_child(root, Direction::Right, "a").unwrap();
        let b = g.create_child(root, Direction::Right, "b").unwrap();
        g.create_child(a, Direction::Right, "a1");
        g.create_child(a, Direction::Right, "a2");
        g.create_child(b, Direction::Right, "b1");
        // Scramble first — tidy has to fix a genuinely messy map.
        for (i, id) in g.nodes.keys().copied().collect::<Vec<_>>().iter().enumerate() {
            if let Some(n) = g.nodes.get_mut(id) {
                n.x = i as f32 * 3.0;
                n.y = i as f32 * 2.0;
            }
        }
        g.tidy();

        assert!(!g.has_overlapping_nodes(), "tidy must not stack nodes");
        let col = |id: u64| g.nodes[&id].x;
        assert_eq!((col(root), g.nodes[&root].y), (0.0, 0.0), "the root anchors it");

        // Two-sided: with two branches, one goes each way.
        assert!(col(a) * col(b) < 0.0, "branches split left and right");
        // Depth drives the column, outward from the root on whichever side.
        let a1 = by_text(&g, "a1");
        let a2 = by_text(&g, "a2");
        assert!(col(a1).abs() > col(a).abs());
        assert_eq!(col(a1), col(a2), "siblings on one side share a column");
        assert_eq!(col(a1).signum(), col(a).signum(), "and stay on that side");
        // Parents sit between their children.
        let ya = g.nodes[&a].y;
        let (y1, y2) = (g.nodes[&a1].y, g.nodes[&a2].y);
        assert!(ya > y1.min(y2) && ya < y1.max(y2));
    }

    #[test]
    fn tidy_balances_the_two_sides() {
        let mut g = Graph::new("root");
        let root = g.root_id;
        // One fat branch and three thin ones: the split has to weigh them,
        // not just deal them out alternately.
        let fat = g.create_child(root, Direction::Right, "fat").unwrap();
        for i in 0..6 {
            g.create_child(fat, Direction::Right, &format!("f{i}"));
        }
        for i in 0..3 {
            g.create_child(root, Direction::Right, &format!("t{i}"));
        }
        g.tidy();

        let side = |id: u64| g.nodes[&id].x.signum();
        let thin: Vec<u64> = (0..3).map(|i| by_text(&g, &format!("t{i}"))).collect();
        assert!(
            thin.iter().all(|&t| side(t) == -side(fat)),
            "the three thin branches balance the fat one, so they share the other side"
        );
        // And the result is squarer than a one-sided layout would be.
        let (x0, y0, x1, y1) = g.bounds_with(|_, _| (40.0, layout::NODE_HALF_H));
        assert!(
            (y1 - y0) < (x1 - x0) * 2.5,
            "not a ribbon: {}x{}",
            x1 - x0,
            y1 - y0
        );
    }

    #[test]
    fn tidy_survives_a_deep_chain() {
        let mut g = Graph::new("root");
        for _ in 0..60 {
            g.create_from_focused(Direction::Right, "x");
        }
        g.tidy();
        assert!(!g.has_overlapping_nodes());
        let depths = g.depths_for(&g.nodes.keys().copied().collect());
        assert_eq!(depths[&g.focused_id], 60);
    }

    #[test]
    fn sanitize_repairs_a_hand_edited_map() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        // Corrupt it the way a text editor would: a cycle, a ghost parent,
        // a dangling edge, a NaN position and a stale focus.
        g.nodes.get_mut(&g.root_id).unwrap().parent = Some(child);
        g.nodes.get_mut(&child).unwrap().x = f32::NAN;
        let eid = *g.edges.keys().next().unwrap();
        g.edges.get_mut(&eid).unwrap().to = 4242;
        g.focused_id = 999;
        g.focused_edge = Some(777);

        g.sanitize();

        assert!(g.nodes[&g.root_id].parent.is_none());
        assert!(g.nodes[&child].x.is_finite());
        // The edge to a ghost node goes, and the child's real parent link gets
        // the line it was missing — the two halves of the tree end up agreeing.
        assert_eq!(g.edges.len(), 1);
        let e = g.edges.values().next().unwrap();
        assert_eq!((e.from, e.to), (g.root_id, child));
        assert_eq!(g.focused_id, g.root_id);
        assert!(g.focused_edge.is_none());
        // Traversals must terminate now that the cycle is gone.
        assert_eq!(g.subtree_ids(g.root_id).len(), 2);
        let depths = g.depths_for(&g.nodes.keys().copied().collect());
        assert_eq!(depths[&child], 1);
    }

    #[test]
    fn sanitize_breaks_a_cycle_without_flattening_what_hangs_off_it() {
        let mut g = Graph::new("root");
        let a = g.create_child(g.root_id, Direction::Right, "a").unwrap();
        let b = g.create_child(a, Direction::Right, "b").unwrap();
        let c = g.create_child(b, Direction::Right, "c").unwrap();
        let d = g.create_child(c, Direction::Right, "d").unwrap();
        // a ↔ b is the loop; c and d merely hang off it and are innocent.
        g.nodes.get_mut(&a).unwrap().parent = Some(b);

        g.sanitize();

        assert_eq!(g.nodes[&c].parent, Some(b), "a descendant keeps its parent");
        assert_eq!(g.nodes[&d].parent, Some(c));
        // Traversal terminates, which is the point of the repair.
        assert_eq!(g.subtree_ids(g.root_id).len(), g.nodes.len());
    }

    #[test]
    fn sanitize_makes_edges_and_parents_agree() {
        let mut g = Graph::new("root");
        let byparent = g.create_child(g.root_id, Direction::Right, "by parent").unwrap();
        let byedge = g.create_child(g.root_id, Direction::Down, "by edge").unwrap();
        // Strip the edge off one child and the parent off the other: one would
        // be drawn with no line into it, the other reachable only by line.
        let stray = *g
            .edges
            .iter()
            .find(|(_, e)| e.to == byparent)
            .map(|(id, _)| id)
            .unwrap();
        g.edges.remove(&stray);
        g.nodes.get_mut(&byedge).unwrap().parent = None;

        g.sanitize();

        let links: HashSet<(u64, u64)> = g.edges.values().map(|e| (e.from, e.to)).collect();
        let parents: HashSet<(u64, u64)> = g
            .nodes
            .iter()
            .filter_map(|(&id, n)| n.parent.map(|p| (p, id)))
            .collect();
        assert_eq!(links, parents, "every parent link has exactly one edge");
        assert!(links.contains(&(g.root_id, byparent)), "the missing line is drawn");
        assert!(
            !links.iter().any(|&(_, to)| to == byedge),
            "a line to a node with no parent is not a link"
        );
    }

    #[test]
    fn sanitize_keeps_one_edge_per_link_and_picks_it_deterministically() {
        let mut g = Graph::new("root");
        let child = g.create_child(g.root_id, Direction::Right, "child").unwrap();
        let (from, to) = (g.root_id, child);
        // Two more lines describing the same link, as a merge conflict might.
        for eid in [900u64, 901] {
            g.edges.insert(eid, Edge { from, to, label: String::new() });
        }
        g.sanitize();
        assert_eq!(g.edges.len(), 1);
        g.focus(child);
        let first = g.edge_to_focused();
        assert!(first.is_some());
        // The same answer every time, not whichever the hash yields first.
        for _ in 0..20 {
            assert_eq!(g.edge_to_focused(), first);
        }
    }

    #[test]
    fn ids_are_never_reused_when_the_counter_is_exhausted() {
        let mut g = Graph::new("root");
        // A hand-edited file can park the counter at the very top.
        g.next_id = u64::MAX;
        let before = g.nodes.clone();
        assert!(
            g.create_child(g.root_id, Direction::Right, "x").is_none(),
            "refuse rather than wrap"
        );
        assert_eq!(g.nodes.len(), before.len(), "the root was not overwritten");
        assert_eq!(g.nodes[&g.root_id].text, "root");
    }

    #[test]
    fn sanitize_survives_an_id_at_the_top_of_the_range() {
        let mut g = Graph::new("root");
        g.nodes.insert(
            u64::MAX,
            Node { text: "far".into(), x: 0.0, y: 0.0, parent: Some(g.root_id), collapsed: false, color: None, image: None },
        );
        // `max_id + 1` here used to wrap to 0 and leave the counter behind
        // every id in the map.
        g.sanitize();
        assert_eq!(g.next_id, u64::MAX, "saturated, not wrapped");
    }

    #[test]
    fn a_far_flung_map_never_yields_a_non_finite_position() {
        let mut g = Graph::new("root");
        g.nodes.get_mut(&g.root_id).unwrap().x = 3.0e38;
        let child = g.create_child(g.root_id, Direction::Right, "b").unwrap();
        g.nodes.get_mut(&child).unwrap().x = f32::INFINITY;
        g.drop_non_finite_positions();
        assert!(g.nodes.values().all(|n| n.x.is_finite() && n.y.is_finite()));
    }

    #[test]
    fn deleting_a_subtree_takes_its_ids_out_of_the_selection() {
        let mut g = nav_fixture();
        let ideas = by_text(&g, "ideas");
        let ship = by_text(&g, "ship");
        let notes = by_text(&g, "notes");
        g.set_node_selection(vec![ideas, ship, notes]);
        // The invariant belongs to `delete_subtree`, not to one caller of it.
        assert!(g.delete_subtree(ideas));
        assert!(
            g.acting_on().iter().all(|id| g.nodes.contains_key(id)),
            "no dead ids survive in what a bulk operation would act on"
        );
    }

    #[test]
    fn batched_depths_match_the_one_at_a_time_definition() {
        // The batched walk memoizes, and an off-by-one on the cache-hit path
        // is invisible until it changes how edges are drawn.
        let mut g = Graph::new("root");
        let root = g.root_id;
        let mut leaves = vec![root];
        for i in 0..40 {
            let parent = leaves[i % leaves.len()];
            g.focus(parent);
            if let Some(id) = g.create_from_focused(Direction::Right, "n") {
                leaves.push(id);
            }
        }
        let all: HashSet<u64> = g.nodes.keys().copied().collect();
        let batched = g.depths_for(&all);
        assert_eq!(batched.len(), g.nodes.len());
        assert_eq!(batched[&root], 0);
        // Independent definition: count the parent links up to the root.
        for id in &all {
            let mut walked = 0;
            let mut cur = *id;
            while let Some(p) = g.nodes[&cur].parent {
                cur = p;
                walked += 1;
            }
            assert_eq!(batched[id], walked, "node {id}");
        }

        // Asking for a subset must give the same answers, memo hits and all.
        let some: HashSet<u64> = all.iter().copied().filter(|id| id % 3 == 0).collect();
        for (id, d) in g.depths_for(&some) {
            assert_eq!(d, batched[&id]);
        }
    }

    #[test]
    fn tidy_caps_recursion_on_a_pathological_chain() {
        // Deeper than MAX_TIDY_DEPTH: must terminate without overflowing, and
        // every node must still land somewhere finite.
        let mut g = Graph::new("root");
        for _ in 0..(MAX_TIDY_DEPTH + 200) {
            g.create_from_focused(Direction::Right, "x");
        }
        g.tidy();
        assert!(g.nodes.values().all(|n| n.x.is_finite() && n.y.is_finite()));
        let placed_columns = g.nodes.values().filter(|n| n.x > 0.0).count();
        assert!(placed_columns > 100, "the tree was actually laid out");
    }

    #[test]
    fn sanitize_keeps_new_ids_unique() {
        let mut g = Graph::new("root");
        g.nodes.insert(
            500,
            Node {
                text: "smuggled in".into(),
                x: 10.0,
                y: 10.0,
                parent: Some(g.root_id),
                collapsed: false,
                color: None,
                image: None,
            },
        );
        g.sanitize();
        let fresh = g.create_child(g.root_id, Direction::Down, "new").unwrap();
        assert!(fresh > 500, "next_id caught up: {fresh}");
    }

    #[test]
    fn bounds_grow_with_long_labels() {
        // Width comes from the caller, so a longer label only widens the
        // bounds if the caller says it is wider.
        let mut g = Graph::new("");
        g.enter_edit();
        g.set_focused_text("a-really-quite-long-node-label-here".into());
        let narrow = g.bounds_with(|_, _| (20.0, layout::NODE_HALF_H));
        let wide = g.bounds_with(|_, n| (n.text.len() as f32 * 4.0, layout::NODE_HALF_H));
        assert!(wide.2 - wide.0 > narrow.2 - narrow.0);
    }

    #[test]
    fn edge_label_edit_on_midline() {
        let mut g = Graph::new("root");
        let child = g.create_from_focused(Direction::Right, "child").unwrap();
        let eid = g.edge_to_focused().expect("edge to child");
        let mid = g.edge_mid(eid).unwrap();
        assert_eq!(g.hit_test_edge_r(mid.0, mid.1, layout::EDGE_HIT_R), Some(eid));
        assert!(g.focus_edge(eid));
        assert_eq!(g.selection(), Selection::Edge(eid));
        g.insert_char('w');
        g.insert_char('h');
        g.insert_char('y');
        assert_eq!(g.edges[&eid].label, "why");
        g.backspace();
        assert_eq!(g.edges[&eid].label, "wh");
        // Node text unchanged while edge selected
        assert_eq!(g.nodes[&child].text, "child");
        g.clear_edge_focus();
        g.enter_edit();
        g.insert_char('!');
        assert_eq!(g.nodes[&child].text, "child!");
    }
}
