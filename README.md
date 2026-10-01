# mind67

A macOS mind-mapping app built in Rust with [GPUI](https://gpui.rs). Create
branches with the keyboard, arrange them on the canvas, and add text or images.
Maps save as local JSON files.

## Features

- A zoomable canvas with a layout command, branch folding, and a minimap.
- Text labels with line breaks, image nodes, edge labels, and node colors.
- Keyboard editing, multiple-node selection, drag selection, and undo/redo.
- Find and replace, plus a shortcut panel with customizable commands.
- Light and dark themes.
- Markdown and OPML import; Markdown, OPML, text outline, and SVG export.
- Autosave, recent documents, and restored camera and window positions.

## Build and run

Use macOS with Rust stable and Xcode, including the Metal compiler, installed.
From the repository root:

```sh
./run.sh
```

This builds and opens `dist/mind67.app`. To build without launching, run
`./scripts/build-app.sh`. Add `--install` to copy the app into `/Applications`.
These local builds use an ad-hoc signature.

To try a temporary map, run `./run.sh --demo` or `./run.sh --empty`. These sessions
do not overwrite your saved map. For development, use `./dev.sh`; see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Using the map

In **Browse** mode, select and move nodes. Start typing or press Return to edit
the focused node. Press Return or Escape to return to Browse; Shift-Return adds
a line break while editing.

| Default shortcut | Action |
|---|---|
| Tab / Shift-Tab, in Browse | Create a child / sibling |
| Command-Return / Command-Shift-Return | Create a child / sibling from either mode |
| Arrow keys, in Browse | Move selected nodes |
| Shift-arrow keys, in Browse | Move selected nodes farther |
| Option-arrow keys, in Browse | Navigate between nodes |
| Arrow keys, in Edit | Move the text caret |
| Command-R | Tidy the layout |
| Command-period | Fold or unfold a branch |
| Command-F | Find |
| Command-0 | Fit the map in the window |
| Command-T | Switch light/dark theme |
| Command-N / Command-O | New map / open map |
| Command-S / Command-Shift-S | Save / save as |
| Command-Z / Command-Shift-Z | Undo / redo |
| Command-slash | Open the shortcut panel |

Click a node to select it. Shift-click adds or removes a node from the selection;
Command-click selects its branch. Drag on empty canvas to select a group.
Right-click a node, edge, or empty canvas for its available commands.

## Files and saving

On first launch, choose a folder for your maps. Maps save automatically as JSON
files in that folder, alongside the shared `images/` directory. When you reopen
the app, it returns to your last document. Keep the images directory with your
maps when moving them between computers.

Older maps and images in Application Support are copied into the chosen folder
without deleting the originals or replacing different files. Preferences,
recent-document settings and bookmarks remain in Application Support.

Closing the window keeps the app running. **Window → Show Main Window** or
clicking the Dock icon brings it back with your current map intact.

Use **File → Open** to switch documents and **Save As** to choose a file location.
See the [privacy policy](PRIVACY.md) for data handling.

## Import and export

Use the File menu to import Markdown or OPML, or export to Markdown, OPML, a
text outline, or SVG. Exports include text and structure; images are omitted.
Importing an exported outline can change the order of sibling nodes.

Maps use a tree structure: each node has one parent, with no cross-links between
branches.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for development builds and checks.

## License

The code is [MIT licensed](LICENSE). Dependencies and fonts retain their own
licenses; see [THIRD-PARTY.md](THIRD-PARTY.md).

The mind67 name and app icon are excluded from the code license. Use your own
name and icon when distributing a fork.
