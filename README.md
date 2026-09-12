# rusidian

A local-first native Markdown app powered by real Neovim.

## Development build on macOS

With Command Line Tools but no standalone Metal compiler, the default build
uses GPUI's upstream runtime shader compilation path through the system Metal
API. Run the current shell with `cargo run --locked`, or open a text file with
`cargo run --locked -- path/to/note.md`. Runtime shader compilation may
increase startup time; release performance has not been validated with this
option. Building with `--no-default-features` requires `xcrun metal` and
`xcrun metallib`.

> **Direction update (2026-09-11):** [`PRODUCT.md`](PRODUCT.md) is the current product decision
> record. GPUI is the selected GUI framework; see
> [`docs/adr/0001-use-gpui-for-gui.md`](docs/adr/0001-use-gpui-for-gui.md). The renderer-first
> proposal below is retained as historical design input and is not the current product scope.

> **Status: pre-implementation.** The repository contains only a minimal Cargo bootstrap; no
> product architecture is implemented. The historical 26-week roadmap lives in `PLAN.html`.

## The problem

Every existing way to render math and diagrams inline in Neovim depends on a terminal
graphics protocol: [image.nvim](https://github.com/3rd/image.nvim),
[mdmath.nvim](https://github.com/Thiago4532/mdmath.nvim),
[diagram.nvim](https://github.com/3rd/diagram.nvim). That means a specific terminal
(Kitty, WezTerm, Ghostty), fragile tmux passthrough, and breakage over SSH.

Obsidian avoids the terminal problem by not being a terminal, and pays for it with Electron,
a MathJax subset instead of real TeX, and an emulated Vim mode instead of your actual Neovim.

## The approach

**Carry the graphics protocol over Neovim RPC instead of terminal escape sequences.**

A Lua plugin uses treesitter to locate TikZ and `$$` blocks and reserves space with extmark
virtual lines. A Rust engine hashes each block, compiles it with a real TeX engine, and caches
the result. The renderer draws into the reserved rectangle and recomputes position as the
viewport moves.

Two things fall out of this for free:

- **Any legal TikZ.** A real TeX engine means real packages: `tikz-cd`, `circuitikz`,
  `pgfplots`, and your own preamble.
- **Preview and PDF export cannot disagree**, because they share one engine and one preamble.
  Obsidian previews with MathJax and exports through Chromium's print pipeline; that mismatch
  is why its exports do not look like what you saw.

## Non-goals

Deliberate, not deferred:

- **Not an editor.** Your Neovim, your `init.lua`, your plugins, your LSP.
- **No Markdown style rendering.** Bold, italics, and headings stay with your existing plugins.
- **No inline `$...$`.** Block-level only. Inline math needs sub-cell baseline alignment, which
  is a different problem from placing a block.
- **No PDF exporter.** pandoc handles document structure; rusidian owns the preamble.
- **No sync.** Git and Syncthing already work.

## Your notes stay yours

Plain Markdown in plain folders. Obsidian-flavored wikilinks and YAML frontmatter, which is the
de facto interchange format that obsidian.nvim, Logseq, Foam, and Quartz already read. Caches
live in the XDG cache directory and never inside your vault. If this project is abandoned, your
notes lose nothing.

## Roadmap

| | Week | Deliverable |
|---|---|---|
| **M1** | 8 | CLI: Markdown in, SVG out. Content-hash cache, precompiled preamble, TeX errors mapped back to source lines. |
| **M2** | 16 | Lua plugin and Rust backend speaking RPC. Correct position while scrolling. Indexes a real 248-note vault. |
| **M3** | 26 | Usable release, install docs, demo. |

If M1 is not finished by week 12, the project stops rather than continuing on a foundation that
will not hold.

## Stack

Rust. [tectonic](https://github.com/tectonic-typesetting/tectonic) for TeX, so there is no 5 GB
TeX Live prerequisite. Neovim RPC for the protocol. GPUI is the selected GUI framework; the
renderer and protocol remain isolated from its implementation details.
