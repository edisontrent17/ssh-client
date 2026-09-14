# Vendored terminal widget

Source: https://github.com/kemokempo/egui_term

Revision: `31bbc7ab8503c9518fcee5717cfa29011e59f451`.

The upstream MIT license is preserved alongside this file. Local changes:

- Standalone dependency manifest; examples are omitted.
- End the event subscription thread when the channel closes or its consumer disappears.
- Preserve keyboard focus independently of pointer position; mouse events still require a pointer over the terminal.
- Acquire focus on click or explicit focus request, without stealing it every frame.
- Bound scrollback to 2,000 lines per session.
- Honor bracketed-paste mode, strip embedded escape bytes from pasted text, and normalize newline handling.
- Apply Rust formatting.
