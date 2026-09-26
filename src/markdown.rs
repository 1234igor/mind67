//! Lightweight, dependency-free markdown highlighter.
//!
//! The renderer shapes **one line at a time** and turns the output of
//! [`highlight_line`] directly into `gpui::TextRun`s, so this module's contract
//! is load-bearing. For any `line` and any `in_code_block`, the returned
//! `Vec<Span>` satisfies:
//!
//! 1. **Empty line ⇒ empty vec.** `line.is_empty()` yields `[]`.
//! 2. **Exact contiguous cover.** Otherwise the spans are sorted by `start`,
//!    non-overlapping and gapless: `spans[0].start == 0`,
//!    `spans[i].end == spans[i + 1].start`, and
//!    `spans.last().end == line.len()`.
//! 3. **Char boundaries.** Every `start` and `end` satisfies
//!    `line.is_char_boundary(x)`, so slicing `&line[s.start..s.end]` is always
//!    safe.
//! 4. **No empty spans.** `start < end` for every span.
//! 5. **Total.** The function never panics, on any byte string that is valid
//!    UTF-8: malformed markdown, lone `*`, unterminated backticks, unterminated
//!    `[`, nested emphasis, emoji, CJK, combining marks, gigantic lines.
//!
//! Additionally, adjacent spans always have *different* styles (runs with equal
//! styles are merged) which keeps `TextRun` counts low.
//!
//! Offsets are byte offsets **into the line**, never into the document.
//!
//! Fenced-code state is not inferable from a single line, so the caller threads
//! it: pass `in_code_block = true` when previous lines opened a fence. The fence
//! line itself is passed with the state that was active *before* it, and
//! [`is_fence`] tells the caller when to toggle. [`highlight_document`] does
//! that bookkeeping for whole buffers.
//!
//! This is deliberately *not* a CommonMark implementation. It is a syntax
//! highlighter: it never fails, never reflows, and prefers "looks right in an
//! editor" over spec conformance.

// Ported whole from the GravityNote highlighter; a few probes (link_at,
// list_marker_end, the list-continuation helpers) are kept for the features
// that will use them next, so silence dead-code noise for the module.
#![allow(dead_code)]

/// The visual role of a byte range on a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MdStyle {
    /// Ordinary prose.
    Text,
    /// Heading body text; the u8 is the level 1..=6.
    Heading(u8),
    /// The `###` sigil and the space after it, for a heading of that level.
    HeadingMarker(u8),
    Bold,
    Italic,
    BoldItalic,
    Strikethrough,
    /// Inline `code` body (between the backticks).
    Code,
    /// Text inside a fenced code block.
    CodeBlock,
    /// The ``` fence line itself.
    Fence,
    /// Punctuation that is markup, not content: `*`, `_`, `` ` ``, `~`, `[`, `]`,
    /// `(`, `)`. Rendered dim.
    Marker,
    /// The visible text of a `[text](url)` link.
    LinkText,
    /// The url part of a `[text](url)`, and bare autolinks.
    LinkUrl,
    /// The `- `, `* `, `+ `, `1. ` bullet at the head of a list item.
    ListMarker,
    /// An unchecked `[ ]` task box.
    TaskOpen,
    /// A checked `[x]` task box.
    TaskDone,
    /// The `>` blockquote sigil.
    QuoteMarker,
    /// Text inside a blockquote.
    Quote,
    /// A thematic-break line: `---`, `***`, `___`.
    Separator,
}

/// A styled byte range of a single line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte offset into the LINE (not the document).
    pub start: usize,
    /// Byte offset into the LINE, exclusive.
    pub end: usize,
    pub style: MdStyle,
}

impl Span {
    #[inline]
    fn new(start: usize, end: usize, style: MdStyle) -> Self {
        Span { start, end, style }
    }
}

// ---------------------------------------------------------------------------
// Block-level probes
// ---------------------------------------------------------------------------

/// True when `line` opens or closes a fenced code block (``` or ~~~, 3+ chars,
/// optionally indented up to 3 spaces, optionally followed by an info string).
pub fn is_fence(line: &str) -> bool {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && i < 3 && b[i] == b' ' {
        i += 1;
    }
    if i >= b.len() {
        return false;
    }
    let c = b[i];
    if c != b'`' && c != b'~' {
        return false;
    }
    let run = run_len(b, i, c);
    if run < 3 {
        return false;
    }
    // A backtick fence's info string may not itself contain a backtick.
    if c == b'`' && b[i + run..].contains(&b'`') {
        return false;
    }
    true
}

/// True when `line` is a thematic break (3+ of `-`, `*`, or `_`, possibly
/// space-separated, after trimming).
pub fn is_separator(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let b = t.as_bytes();
    let c = b[0];
    if c != b'-' && c != b'*' && c != b'_' {
        return false;
    }
    let mut count = 0usize;
    for &x in b {
        if x == c {
            count += 1;
        } else if x != b' ' && x != b'\t' {
            return false;
        }
    }
    count >= 3
}

/// `(level, marker_end)` when the line opens an ATX heading.
fn heading_at(line: &str) -> Option<(u8, usize)> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && i < 3 && b[i] == b' ' {
        i += 1;
    }
    let hash_start = i;
    while i < b.len() && b[i] == b'#' {
        i += 1;
    }
    let level = i - hash_start;
    if level == 0 || level > 6 {
        return None;
    }
    if i == b.len() {
        return Some((level as u8, i));
    }
    if b[i] == b' ' || b[i] == b'\t' {
        return Some((level as u8, i + 1));
    }
    None
}

/// End offset of the blockquote sigil run (`>`, `> `, `>> `, `> > `), if any.
fn quote_at(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && i < 3 && b[i] == b' ' {
        i += 1;
    }
    if i >= b.len() || b[i] != b'>' {
        return None;
    }
    while i < b.len() && b[i] == b'>' {
        i += 1;
        if i < b.len() && b[i] == b' ' {
            i += 1;
        }
    }
    Some(i)
}

/// End offset of a list bullet including its trailing space, if any.
///
/// Public as [`list_marker_end`] so the list-editing commands work from the
/// same idea of what a bullet is as the highlighter draws.
pub fn list_marker_end(line: &str) -> Option<usize> {
    list_at(line)
}

fn list_at(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    if i >= b.len() {
        return None;
    }
    let after = match b[i] {
        b'-' | b'*' | b'+' => i + 1,
        b'0'..=b'9' => {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j - i > 9 {
                return None;
            }
            if j < b.len() && (b[j] == b'.' || b[j] == b')') {
                j + 1
            } else {
                return None;
            }
        }
        _ => return None,
    };
    if after < b.len() && (b[after] == b' ' || b[after] == b'\t') {
        Some(after + 1)
    } else {
        None
    }
}

/// `(end, style)` for a `[ ]` / `[x]` task box at the head of `rest`.
fn task_at(rest: &str) -> Option<(usize, MdStyle)> {
    let b = rest.as_bytes();
    if b.len() < 3 || b[0] != b'[' || b[2] != b']' {
        return None;
    }
    let style = match b[1] {
        b' ' => MdStyle::TaskOpen,
        b'x' | b'X' => MdStyle::TaskDone,
        _ => return None,
    };
    let end = if b.len() > 3 && b[3] == b' ' { 4 } else { 3 };
    Some((end, style))
}

/// The prefix that carries a line's structure onto the next one, and the byte
/// offset where that structure ends.
fn marker(line: &str) -> Option<(String, usize)> {
    if let Some(end) = quote_at(line) {
        // A quote can hold a list, so carry both. The remainder no longer
        // starts with `>`, so this recurses at most one level.
        return Some(match marker(&line[end..]) {
            Some((inner, inner_end)) => (format!("{}{inner}", &line[..end]), end + inner_end),
            None => (line[..end].to_string(), end),
        });
    }

    let end = list_at(line)?;
    let mut prefix = next_bullet(&line[..end]);
    let mut marker_end = end;
    if let Some((task_len, _)) = task_at(&line[end..]) {
        marker_end += task_len;
        // A carried-over task starts unticked whether or not this one is.
        prefix.push_str("[ ] ");
    }
    Some((prefix, marker_end))
}

/// The bullet that follows `bullet`: the same one, except that an ordered list
/// counts up.
fn next_bullet(bullet: &str) -> String {
    let indent_end = bullet.len() - bullet.trim_start_matches([' ', '\t']).len();
    let (indent, rest) = bullet.split_at(indent_end);
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let Ok(number) = rest[..digits_end].parse::<u64>() else {
        return bullet.to_string();
    };
    format!("{indent}{}{}", number.saturating_add(1), &rest[digits_end..])
}

// ---------------------------------------------------------------------------
// Line entry point
// ---------------------------------------------------------------------------

/// Highlight a single line. `in_code_block` is true when the PREVIOUS lines put
/// us inside a fenced code block (the fence line itself is passed with the state
/// that was active BEFORE it).
pub fn highlight_line(line: &str, in_code_block: bool) -> Vec<Span> {
    if line.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Span> = Vec::new();
    build_line(line, in_code_block, &mut out);
    merge(&mut out);
    debug_assert!(covers(line, &out));
    out
}

fn build_line(line: &str, in_code_block: bool, out: &mut Vec<Span>) {
    let len = line.len();

    if is_fence(line) {
        out.push(Span::new(0, len, MdStyle::Fence));
        return;
    }
    if in_code_block {
        out.push(Span::new(0, len, MdStyle::CodeBlock));
        return;
    }
    if is_separator(line) {
        out.push(Span::new(0, len, MdStyle::Separator));
        return;
    }
    if let Some((level, marker_end)) = heading_at(line) {
        out.push(Span::new(0, marker_end, MdStyle::HeadingMarker(level)));
        if marker_end < len {
            // Inline markup nests inside a heading: `# **bold** and `code`` is
            // parsed like any other line, and only its plain prose takes the
            // heading style. Without this the `**`/`` ` `` show through as
            // literal punctuation at heading size.
            highlight_wrapped(&line[marker_end..], marker_end, MdStyle::Heading(level), 0, out);
        }
        return;
    }
    if let Some(marker_end) = quote_at(line) {
        out.push(Span::new(0, marker_end, MdStyle::QuoteMarker));
        if marker_end < len {
            // The remainder no longer starts with `>`, so this recurses at most
            // one level.
            let inner = highlight_line(&line[marker_end..], false);
            for mut s in inner {
                s.start += marker_end;
                s.end += marker_end;
                if s.style == MdStyle::Text {
                    s.style = MdStyle::Quote;
                }
                out.push(s);
            }
        }
        return;
    }
    if let Some(marker_end) = list_at(line) {
        out.push(Span::new(0, marker_end, MdStyle::ListMarker));
        let mut at = marker_end;
        let done = matches!(task_at(&line[at..]), Some((_, MdStyle::TaskDone)));
        if let Some((task_len, style)) = task_at(&line[at..]) {
            out.push(Span::new(at, at + task_len, style));
            at += task_len;
        }
        if at < len {
            let from = out.len();
            highlight_inline(&line[at..], at, 0, out);
            // A ticked box changed four characters and left the sentence
            // identical to an open one, so the result of ⌘⏎ was invisible while
            // scanning. The whole content is struck, not only its plain prose:
            // `- [x] fix `bug`` reads as done end to end, its code and emphasis
            // included. Only the bullet and the box keep their own styling, so
            // the line still reads as a task. The flat style set has no
            // "struck code", so the sub-styles collapse into one strike — the
            // struck-out look wins, which is the point of completing.
            if done {
                for span in &mut out[from..] {
                    span.style = MdStyle::Strikethrough;
                }
            }
        }
        return;
    }
    highlight_inline(line, 0, 0, out);
}

/// Highlight a whole document, tracking fence state across lines.
/// `out[i]` corresponds to `lines[i]`.
pub fn highlight_document(lines: &[&str]) -> Vec<Vec<Span>> {
    let mut in_code_block = false;
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        out.push(highlight_line(line, in_code_block));
        if is_fence(line) {
            in_code_block = !in_code_block;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Inline scanner
// ---------------------------------------------------------------------------

#[inline]
fn run_len(b: &[u8], i: usize, c: u8) -> usize {
    let mut j = i;
    while j < b.len() && b[j] == c {
        j += 1;
    }
    j - i
}

#[inline]
fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

/// Is the char immediately before byte offset `i` alphanumeric?
fn alnum_before(s: &str, i: usize) -> bool {
    s[..i]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric())
}

/// Is the char starting at byte offset `i` alphanumeric?
fn alnum_at(s: &str, i: usize) -> bool {
    if i >= s.len() {
        return false;
    }
    s[i..].chars().next().is_some_and(|c| c.is_alphanumeric())
}

/// Find a run of exactly `need` copies of `c` at or after `from`.
fn find_exact_run(b: &[u8], from: usize, c: u8, need: usize) -> Option<usize> {
    let mut j = from;
    while j < b.len() {
        if b[j] == c {
            let r = run_len(b, j, c);
            if r == need {
                return Some(j);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}

/// Find a closing emphasis run of at least `need` copies of `c` at or after
/// `from`. `underscore` enables the intra-word guard.
fn find_emph_close(s: &str, from: usize, c: u8, need: usize, underscore: bool) -> Option<usize> {
    let b = s.as_bytes();
    let mut j = from;
    while j < b.len() {
        if b[j] == c {
            let r = run_len(b, j, c);
            if r >= need && j > from && !is_space(b[j - 1]) && (!underscore || !alnum_at(s, j + r))
            {
                return Some(j);
            }
            j += r;
        } else {
            j += 1;
        }
    }
    None
}

/// `(close_bracket, close_paren)` for a `[text](url)` starting at `open`.
fn find_link(b: &[u8], open: usize) -> Option<(usize, usize)> {
    let mut rb = open + 1;
    while rb < b.len() && b[rb] != b']' {
        // Bail on a nested `[` rather than mis-pairing across it.
        if b[rb] == b'[' {
            return None;
        }
        rb += 1;
    }
    if rb >= b.len() || rb + 1 >= b.len() || b[rb + 1] != b'(' {
        return None;
    }
    let mut rp = rb + 2;
    while rp < b.len() && b[rp] != b')' {
        rp += 1;
    }
    if rp >= b.len() {
        return None;
    }
    Some((rb, rp))
}

/// End offset of a bare `http://` / `https://` run starting at `i`.
fn autolink_end(s: &str, i: usize) -> Option<usize> {
    let rest = &s[i..];
    let scheme = if rest.starts_with("http://") {
        7
    } else if rest.starts_with("https://") {
        8
    } else {
        return None;
    };
    if alnum_before(s, i) {
        return None;
    }
    let b = s.as_bytes();
    let mut end = i + scheme;
    while end < b.len() {
        let c = b[end];
        if c == b' ' || c == b'\t' || c == b'<' || c == b'>' || c == b'"' || c == b'`' {
            break;
        }
        end += 1;
    }
    // Don't swallow sentence punctuation that trails the url.
    while end > i + scheme
        && matches!(
            b[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?' | b')' | b']' | b'}' | b'\''
        )
    {
        end -= 1;
    }
    Some(end)
}

/// The URL to open for a click at byte `offset` within `line`, if the offset
/// lands on a link. Covers a `[text](url)` anywhere from the opening `[` through
/// the closing `)`, and a bare `http(s)://…` autolink. It mirrors the same
/// parsers [`highlight_line`] paints with, so what reads as a link is exactly
/// what opens. `None` when the offset is on ordinary text.
pub fn link_at(line: &str, offset: usize) -> Option<String> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // An inline `[text](url)`: clickable across the whole construct.
        if b[i] == b'[' {
            if let Some((rb, rp)) = find_link(b, i) {
                if (i..=rp).contains(&offset) {
                    let url = line[rb + 2..rp].trim();
                    if !url.is_empty() {
                        return Some(url.to_string());
                    }
                }
                i = rp + 1;
                continue;
            }
        }
        // A bare autolink: the run itself is the URL. `end` is exclusive (and
        // trailing punctuation is already trimmed off it), so the range is
        // half-open — a click on the space or period just past the URL is not on
        // the link.
        if b[i] == b'h' {
            if let Some(end) = autolink_end(line, i) {
                if (i..end).contains(&offset) {
                    return Some(line[i..end].to_string());
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    None
}

fn push_text(out: &mut Vec<Span>, base: usize, start: usize, end: usize) {
    if start < end {
        out.push(Span::new(base + start, base + end, MdStyle::Text));
    }
}

/// How deep inline markup may nest before the scanner stops recursing and draws
/// the remaining content flat. Real markdown never nests near this; the cap
/// only exists so a pathological line — `*_*_…_*_*` thousands of levels deep —
/// cannot recurse the stack into an overflow, keeping [`highlight_line`] total
/// on any input (invariant 5).
const MAX_INLINE_NEST: u8 = 8;

/// Highlight `inner` (a substring at byte offset `base` within the line) and
/// append its spans, recolouring only its plain [`MdStyle::Text`] to `wrap`.
///
/// This is how inline markup nests: code, links and further emphasis found
/// inside a heading, a link label or an emphasis run keep their own style,
/// while the surrounding prose takes the wrapper's. It is the inline-scanner
/// twin of the whole-line restyling the quote and done-task branches already
/// do. `inner` is non-empty at every call site, so it always contributes at
/// least one span and the caller's cover stays gapless.
///
/// At [`MAX_INLINE_NEST`] it stops recursing and draws `inner` as one flat span
/// — the pre-nesting behaviour — which still tiles.
fn highlight_wrapped(inner: &str, base: usize, wrap: MdStyle, depth: u8, out: &mut Vec<Span>) {
    if depth >= MAX_INLINE_NEST {
        out.push(Span::new(base, base + inner.len(), wrap));
        return;
    }
    let from = out.len();
    highlight_inline(inner, base, depth + 1, out);
    for span in &mut out[from..] {
        if span.style == MdStyle::Text {
            span.style = wrap;
        }
    }
}

/// Scan `s` left to right, appending contiguous spans covering
/// `base..base + s.len()`. `depth` is the inline-nesting level, threaded so
/// [`highlight_wrapped`] can cap recursion (see [`MAX_INLINE_NEST`]).
fn highlight_inline(s: &str, base: usize, depth: u8, out: &mut Vec<Span>) {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut text_start = 0usize;

    while i < n {
        match b[i] {
            // ---- inline code -------------------------------------------------
            b'`' => {
                let run = run_len(b, i, b'`');
                if let Some(j) = find_exact_run(b, i + run, b'`', run) {
                    push_text(out, base, text_start, i);
                    // Backticks included: a chip that starts after the opening
                    // tick and stops before the closing one is the ragged edge
                    // the fenced block was fixed for.
                    out.push(Span::new(base + i, base + j + run, MdStyle::Code));
                    i = j + run;
                    text_start = i;
                } else {
                    i += run;
                }
            }

            // ---- emphasis ----------------------------------------------------
            c @ (b'*' | b'_' | b'~') => {
                let run = run_len(b, i, c);
                let underscore = c == b'_';
                let max = if c == b'~' { 2 } else { run.min(3) };
                let min = if c == b'~' { 2 } else { 1 };
                let opener_ok = run >= min
                    && i + run < n
                    && !is_space(b[i + run])
                    && !(underscore && alnum_before(s, i));

                let mut matched = false;
                if opener_ok {
                    let mut need = max.min(run);
                    while need >= min {
                        if let Some(j) = find_emph_close(s, i + need, c, need, underscore) {
                            let style = match (c, need) {
                                (b'~', _) => MdStyle::Strikethrough,
                                (_, 3) => MdStyle::BoldItalic,
                                (_, 2) => MdStyle::Bold,
                                _ => MdStyle::Italic,
                            };
                            push_text(out, base, text_start, i);
                            out.push(Span::new(base + i, base + i + need, MdStyle::Marker));
                            // The content nests: `**`code`**` keeps its code
                            // chip, `*a `b` c*` styles the code and italicises
                            // the rest. The close is strictly after the open, so
                            // this inner slice is never empty.
                            highlight_wrapped(&s[i + need..j], base + i + need, style, depth, out);
                            out.push(Span::new(base + j, base + j + need, MdStyle::Marker));
                            i = j + need;
                            text_start = i;
                            matched = true;
                            break;
                        }
                        need -= 1;
                    }
                }
                if !matched {
                    i += run;
                }
            }

            // ---- links and images --------------------------------------------
            b'!' | b'[' => {
                let bang = b[i] == b'!';
                let open = if bang { i + 1 } else { i };
                let link = if !bang || (open < n && b[open] == b'[') {
                    find_link(b, open)
                } else {
                    None
                };
                if let Some((rb, rp)) = link {
                    push_text(out, base, text_start, i);
                    // `!` + `[`
                    out.push(Span::new(base + i, base + open + 1, MdStyle::Marker));
                    if rb > open + 1 {
                        // The label nests too, so `[*a*](b)` italicises its `a`.
                        // `find_link` bars a nested `[`, so no link recurses
                        // inside a link.
                        highlight_wrapped(
                            &s[open + 1..rb],
                            base + open + 1,
                            MdStyle::LinkText,
                            depth,
                            out,
                        );
                    }
                    // `](`
                    out.push(Span::new(base + rb, base + rb + 2, MdStyle::Marker));
                    if rp > rb + 2 {
                        out.push(Span::new(base + rb + 2, base + rp, MdStyle::Marker));
                    }
                    out.push(Span::new(base + rp, base + rp + 1, MdStyle::Marker));
                    i = rp + 1;
                    text_start = i;
                } else {
                    i += 1;
                }
            }

            // ---- bare urls -----------------------------------------------------
            b'h' => {
                if let Some(end) = autolink_end(s, i) {
                    push_text(out, base, text_start, i);
                    out.push(Span::new(base + i, base + end, MdStyle::LinkUrl));
                    i = end;
                    text_start = i;
                } else {
                    i += 1;
                }
            }

            _ => i += 1,
        }
    }

    push_text(out, base, text_start, n);
}

// ---------------------------------------------------------------------------
// Post-processing
// ---------------------------------------------------------------------------

/// Collapse adjacent spans that share a style.
fn merge(spans: &mut Vec<Span>) {
    if spans.len() < 2 {
        return;
    }
    let mut write = 0usize;
    for read in 1..spans.len() {
        let cur = spans[read];
        if spans[write].style == cur.style && spans[write].end == cur.start {
            spans[write].end = cur.end;
        } else {
            write += 1;
            spans[write] = cur;
        }
    }
    spans.truncate(write + 1);
}

/// Debug-only invariant check used by `debug_assert!`.
fn covers(line: &str, spans: &[Span]) -> bool {
    if line.is_empty() {
        return spans.is_empty();
    }
    if spans.is_empty() || spans[0].start != 0 || spans[spans.len() - 1].end != line.len() {
        return false;
    }
    for (i, s) in spans.iter().enumerate() {
        if s.start >= s.end || !line.is_char_boundary(s.start) || !line.is_char_boundary(s.end) {
            return false;
        }
        if i + 1 < spans.len() && s.end != spans[i + 1].start {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

