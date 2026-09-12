# AGENTS.md

## Current state

The repository is bootstrapped with a minimal Cargo binary. There is no implemented product architecture, test suite, or CI yet.

Use these sources of truth:

- `PRODUCT.md` records confirmed scope, prototype acceptance, and deferred decisions.
- `CONTEXT.md` defines project language.
- `README.md` and `PLAN.html` still contain a historical renderer-first proposal. Where they conflict with `PRODUCT.md`, follow `PRODUCT.md`.

## Design constraints

- Rust and GPUI are fixed. Read `docs/adr/0001-use-gpui-for-gui.md` before changing GUI dependencies, window ownership, platform integration, or performance gates.
- GPUI owns the application lifecycle, windows, controls, and document rendering. Keep the technical prototype on raw GPUI without WebView, Rusidian-owned AppKit/SwiftUI adapters, `gpui-component`, or copied Zed UI code.
- Pin GPUI to an exact, build-verified Zed Git revision and upgrade it only for a concrete need.
- The source view must run the user's real Neovim rather than emulate Vim behavior or parse configuration files.
- The technical prototype uses Tectonic for TikZ. Surface its compatibility and package trade-offs instead of expanding the prototype silently.
- Local processing, low disk use, and low memory use are product requirements and tiebreakers.
- Keep generated caches and application state outside vaults.

## Working style

Codex may implement production code autonomously. Optimize for delivery, maintainability, tests, and clear handoff.

After finishing a module, the owner writes a short design note in their own words with AI off. If they cannot write it, the module is not understood yet.

## Commands

- `cargo build`
- `cargo test`
- `cargo test <name>`
