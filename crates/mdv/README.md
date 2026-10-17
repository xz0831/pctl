# mdv

A terminal document reader and Markdown renderer with source-linked comments,
questions, and answers. The renderer preserves source ranges and block
fingerprints, wraps Unicode text, and produces ANSI text or an HTML fragment.
Raw HTML, math, and diagram sources remain inert text; remote images become links.

Build with Rust (edition 2024) and Cargo. Run these commands from this folder:

```bash
cargo build --release
cargo run -- view tests/fixtures/gallery/headings.md
cargo run -- view tests/fixtures/gallery
cargo run -- render tests/fixtures/gallery/nested.md --width 40
cargo run -- render tests/fixtures/gallery/nested.md --html
cargo run -- answer <request-id> 'Answer text'
printf 'Answer from stdin' | cargo run -- answer <request-id> -
```

`mdv <file>` also opens the reader. Rendering defaults to 80 columns.
`answer` appends a reply to an existing request; unknown request IDs are rejected.

## Reader keys

| Keys | Action |
| --- | --- |
| `q`, Ctrl-C | Quit |
| `j` / `k`, arrows, Page Up / Page Down, space, `g` / `G`, wheel | Scroll |
| Left / Right | Scroll wide tables |
| `T` | Cycle side column: off, Tree, Inbox |
| `o`, `f` | Show Outline; move focus to/from the side column |
| Enter, click | Open a side entry or the focused block's action menu |
| `h`, Backspace | Go up in Tree |
| Tab / Shift-Tab | Focus the next / previous visible block |
| `/`, `n` / `N` | Search; repeat forward / backward |
| Drag | Select text |
| `y` | Copy selection or focused block through OSC 52 |
| `m` | Toggle plain text / original block Markdown for copying |
| `p`, `r` | Pin the current rendering; refresh after a file change |
| `d` | Jump to the next changed passage |
| Escape | Close a menu, search, or overlay |

The action menu offers copying, source mode, links, comments, and questions.
Enter submits a comment or question. Escape twice discards a nonempty draft.
Clipboard delivery depends on terminal OSC 52 support; verify by pasting.

Markdown renders as formatted text; supported text and code files display as
literal text. Office and PDF conversion requires an optional `docconv` executable.
Converted content has no original-source discussion mapping. The reader restores
terminal modes on exit and supports Unix signal and suspend/resume handling.

## Discussions

The default append-only discussion store is `~/.local/state/mdv/threads.jsonl`.
Replies retain the question's original quote and context. If a passage changes,
the discussion remains visible with its original source and a stale-target warning.
Agent delivery requires a compatible local session and `msg` integration.
A saved question or transport status does not prove that an agent read it.

For isolated integration checks, set `MDV_THREADS_FILE` to a temporary store and
`MDV_ASK_DRYRUN` to a temporary JSON file that records delivery arguments instead
of sending them. Inbox offers come from `~/.local/state/mdv/feed.jsonl`.

## Tests

All fixtures and snapshots are inside this crate:

```bash
cargo test
cargo test --test pty
cargo test --test gallery
```

The Unix PTY suite checks terminal interaction and restoration. Gallery tests
compare ANSI and HTML snapshots and exercise source mapping, Unicode, controls,
links, tables, and CLI errors. Two manual observation tests are ignored by default.
Real clipboard paste, drag selection, and IME composition still need manual checks.

Only when intentionally changing expected renderer output:

```bash
MDV_UPDATE_GOLDEN=1 cargo test --test gallery golden_gallery
```

The library exposes parsing, ANSI/HTML rendering, document and block types, and
discussion storage. Pass the same source text to parsing and rendering. Block
fingerprints locate content; they are not authentication. The HTML output is a
fragment whose styling and resource access policy belong to its caller.

Licensed under MIT; see [LICENSE](LICENSE).
