# Vendored terminal widget

Source: https://github.com/kemokempo/egui_term

Revision: `31bbc7ab8503c9518fcee5717cfa29011e59f451`.

The upstream MIT license is preserved alongside this file. Local changes:

- Standalone dependency manifest; examples are omitted.
- End the event subscription thread when the channel closes or its consumer disappears.
- Preserve keyboard focus independently of pointer position; mouse events still require a pointer over the terminal.
- Acquire focus on click or explicit focus request, without stealing it every frame.
- Ignore focus requests and terminal input while disabled, including behind a connection modal.
- Bound scrollback to 2,000 lines per session.
- Honor bracketed-paste mode, strip embedded escape bytes from pasted text, and normalize newline handling.
- Apply Rust formatting.
- Resolve palette colors once and share default palette/key bindings; custom bindings use copy-on-write.
- Snapshot only visible terminal cells, reuse the cell buffer, and retain unchanged snapshots by output generation.
- Cache unchanged paint commands and up to 2,048 glyph layouts per terminal; invalidate for content, selection, geometry, font, theme, and DPI changes. These caches are owned by the backend and released on tab close.
- Coalesce content notifications for the visible tab and suppress content notifications from hidden tabs, while retaining output and forwarding lifecycle/protocol events.
- Add the opt-in `test-support` in-memory ANSI fixture and regression tests for rendering, terminal state, input, and notification behavior.
- Forward wheel/trackpad steps using live terminal mouse modes and viewport coordinates, including tmux pane scrolling; retain local scrollback and alternate-screen fallback when mouse reporting is off. Encode Control independently from macOS Command.
- Drain final PTY output when a child process exits so terminal error messages remain available alongside the exit status.
- Release viewport snapshots, paint commands, and glyph caches while a tab is hidden; rebuild them on activation while preserving the emulator, scrollback, selection, and output.
