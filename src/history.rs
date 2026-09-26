//! Undo/redo over whole-graph snapshots.
//!
//! Maps are small (hundreds of nodes at most), so a snapshot is cheaper than
//! maintaining an inverse for every mutation — and it can never drift.
//! Consecutive edits that share a [`Tag`] coalesce, so a burst of typing is one
//! undo step rather than one per keystroke.

use crate::graph::Graph;

/// What kind of edit a snapshot precedes. Equal tags coalesce into one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// Typing into node `id`.
    Text(u64),
    /// Typing into the label of edge `id`. Separate from `Text` because a
    /// hand-edited file can give a node and an edge the same id, and one ⌘Z
    /// must not then revert an edit to both.
    EdgeText(u64),
    /// Moving a set of nodes, keyed on the set rather than on any one member.
    Move(u64),
    /// Anything structural — never coalesces.
    Structural,
}

const CAP: usize = 200;

#[derive(Debug, Default)]
pub struct History {
    past: Vec<Graph>,
    future: Vec<Graph>,
    /// Tag of the most recent push, for coalescing.
    last: Option<Tag>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `before` as the state to return to. Call this *prior* to mutating.
    pub fn record(&mut self, before: &Graph, tag: Tag) {
        if tag != Tag::Structural && self.last == Some(tag) && !self.past.is_empty() {
            // Same continuous gesture — the snapshot already on the stack is the
            // one the user means to come back to.
            self.future.clear();
            return;
        }
        self.past.push(before.clone());
        if self.past.len() > CAP {
            self.past.remove(0);
        }
        self.future.clear();
        self.last = Some(tag);
    }

    /// Break coalescing, so the next edit starts a fresh undo step.
    pub fn seal(&mut self) {
        self.last = None;
    }

    /// Take back the most recent snapshot without offering it as a redo — the
    /// abandoned half of a gesture the user cancelled. Undo would leave the
    /// cancelled drag sitting on the redo stack, so ⌘⇧Z would perform a move
    /// that was explicitly called off.
    pub fn discard_last(&mut self) -> Option<Graph> {
        let prev = self.past.pop()?;
        self.last = None;
        Some(prev)
    }

    /// How many undo steps are stacked. A caller that pushed a snapshot uses
    /// this to tell whether that snapshot is still the one on top.
    pub fn depth(&self) -> usize {
        self.past.len()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Returns the graph to install, pushing `current` onto the redo stack.
    pub fn undo(&mut self, current: &Graph) -> Option<Graph> {
        let prev = self.past.pop()?;
        self.future.push(current.clone());
        self.last = None;
        Some(prev)
    }

    pub fn redo(&mut self, current: &Graph) -> Option<Graph> {
        let next = self.future.pop()?;
        self.past.push(current.clone());
        self.last = None;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Direction;

    #[test]
    fn a_discarded_step_is_not_offered_as_a_redo() {
        // The cancel path: Esc during a drag takes back the snapshot the drag
        // pushed. If that went through `undo`, ⌘⇧Z would reapply a move the
        // user explicitly called off.
        let mut h = History::new();
        let mut g = Graph::new("root");
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Right, "moved");

        let restored = h.discard_last().expect("a step to discard");
        assert_eq!(restored.nodes.len(), 1);
        assert!(!h.can_redo(), "a cancelled gesture is not redoable");
        assert!(!h.can_undo());
    }

    #[test]
    fn a_node_edit_and_an_edge_edit_are_separate_steps() {
        // Ids collide only in a hand-edited file, but then one ⌘Z used to
        // revert both.
        let mut h = History::new();
        let g = Graph::new("root");
        h.record(&g, Tag::Text(7));
        h.record(&g, Tag::EdgeText(7));
        assert!(h.can_undo());
        h.undo(&g);
        assert!(h.can_undo(), "the node edit is still its own step");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut h = History::new();
        let mut g = Graph::new("root");
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Right, "child");
        assert_eq!(g.nodes.len(), 2);

        let g = h.undo(&g).expect("undo");
        assert_eq!(g.nodes.len(), 1);
        assert!(!h.can_undo());
        assert!(h.can_redo());

        let g = h.redo(&g).expect("redo");
        assert_eq!(g.nodes.len(), 2);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn typing_run_is_one_undo_step() {
        let mut h = History::new();
        let mut g = Graph::new("");
        g.enter_edit();
        let id = g.focused_id;
        for c in "hello".chars() {
            h.record(&g, Tag::Text(id));
            g.insert_char(c);
        }
        assert_eq!(g.focused().text, "hello");
        let g = h.undo(&g).expect("undo");
        assert_eq!(g.focused().text, "");
        assert!(!h.can_undo(), "the whole word should be one step");
    }

    #[test]
    fn seal_splits_a_run() {
        let mut h = History::new();
        let mut g = Graph::new("");
        g.enter_edit();
        let id = g.focused_id;
        h.record(&g, Tag::Text(id));
        g.insert_char('a');
        h.seal();
        h.record(&g, Tag::Text(id));
        g.insert_char('b');

        let g = h.undo(&g).expect("undo b");
        assert_eq!(g.focused().text, "a");
        let g = h.undo(&g).expect("undo a");
        assert_eq!(g.focused().text, "");
    }

    #[test]
    fn structural_never_coalesces() {
        let mut h = History::new();
        let mut g = Graph::new("root");
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Right, "a");
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Right, "b");
        assert_eq!(g.nodes.len(), 3);

        let g = h.undo(&g).unwrap();
        assert_eq!(g.nodes.len(), 2);
        let g = h.undo(&g).unwrap();
        assert_eq!(g.nodes.len(), 1);
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut h = History::new();
        let mut g = Graph::new("root");
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Right, "a");
        let mut g = h.undo(&g).unwrap();
        assert!(h.can_redo());
        h.record(&g, Tag::Structural);
        g.create_from_focused(Direction::Down, "b");
        assert!(!h.can_redo(), "branching off history drops the redo tail");
    }

    #[test]
    fn undo_stack_is_bounded() {
        let mut h = History::new();
        let g = Graph::new("root");
        for _ in 0..(CAP + 50) {
            h.record(&g, Tag::Structural);
        }
        assert_eq!(h.past.len(), CAP);
    }
}
