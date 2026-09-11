---
status: accepted
---

# Use GPUI for the desktop UI

Rusidian uses GPUI as its sole GUI framework because its editor-oriented GPU rendering, text layout control, macOS/Linux support, and Apache-2.0 license fit the Neovim grid and character-addressable Markdown reading view better than the alternatives. GPUI owns the application lifecycle, windows, controls, and document surfaces; Rusidian does not add WebView or maintain its own AppKit/SwiftUI adapter.

## Considered options

- AppKit shell with a custom Rust rendering surface offered the strongest macOS-native appearance but created two platform shells and too much low-level UI work.
- Iced was the closest fallback but had weaker evidence for editor workloads and native macOS integration.
- egui/eframe optimized prototype speed over the retained long-document architecture; Floem was less mature; Slint and Qt added a UI language or heavier licensing and runtime trade-offs; Tauri violated the no-WebView constraint.

## Consequences

- Pin an exact, build-verified Zed Git revision; upgrade only for a concrete fix or capability.
- Start with raw GPUI rather than `gpui-component` or copied Zed UI code.
- Liquid Glass is a visual preference, not a compatibility promise.
- The technical prototype must enforce the product's Chinese input, package-size, idle-memory, startup-time, and input-latency gates.
- If a bounded investigation cannot make GPUI meet those gates, reopen GUI selection with Iced as the first alternative instead of maintaining a long-lived GPUI fork or weakening the requirements.
