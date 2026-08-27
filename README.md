# rusidian

Real TikZ and LaTeX rendering inside Neovim, without the terminal graphics protocol.

> **Status: pre-implementation.** There is no code yet. This README describes what is being
> built and, just as importantly, what is not. The 26-week roadmap lives in `PLAN.html`.

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
TeX Live prerequisite. Neovim RPC for the protocol. The GUI framework decision is deliberately
deferred until M2: the engine and the protocol are the same whichever way it goes.
