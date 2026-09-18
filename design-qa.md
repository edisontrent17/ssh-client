# Light appearance — option 1

final result: passed

The selected reference is the **first light mockup** from the latest three-image set. The earlier dark-workspace concepts are not the target.

## Evidence

- Source: [selected-mockup.png](docs/design/selected-mockup.png), 1487 × 1058 pixels.
- Implementation: [modal.png](docs/design/modal.png), [workspace.png](docs/design/workspace.png), and [small-window.png](docs/design/small-window.png).
- Full comparison: [comparison.png](docs/design/comparison.png), reference left, implementation right.
- Focused form comparison: [modal-comparison.png](docs/design/modal-comparison.png), reference left, implementation right.
- Native egui framebuffer captures, using `examples/visual_preview.rs`. This is the real application renderer with disposable profiles, an in-memory file tree, and a local shell. No remote host or personal profile was used for the fixture.
- Main content viewport: 1440 × 977.5 logical points; actual framebuffer 1152 × 782 at 0.8 scale to fit the display. Normalized to 1440 × 978 for comparison. Source title bar (48 pixels) excluded and content normalized to the same dimensions. Native window chrome is outside the framebuffer comparison.
- Minimum viewport: 760 × 480 logical points, captured at 608 × 384 and normalized to 760 × 480.
- Matched state: two saved connections, Production selected, terminal open, remote file tree, New connection modal with Name focused. The closed-modal workspace was also inspected.

## Findings and corrections

1. Initial native capture exposed inline Username/Port labels and a clipped Open terminal action. Explicit vertical field layouts and a bounded, compact sidebar inspector corrected these issues.
2. The first comparison exposed insufficient heading weight, narrow sidebars, a dark focus outline, and a faded terminal behind the modal. Applied native font variation weights, adjusted panel widths, used blue focus strokes, and preserved terminal opacity while blocking its input.
3. Minimum-size capture exposed a modal taller than the viewport. A bounded horizontal footer and scrollable form now keep the entire modal within the window. The measured modal bounds at 760 × 480 are approximately x=115–646, y=50–430. A regression test checks these bounds.
4. Closed-workspace capture exposed a clipped file-transfer helper paragraph. Replaced it with one concise line and preserved the detailed explanation in a tooltip. The final workspace screenshot shows all footer controls and helper text within bounds.

No remaining actionable P0/P1/P2 findings in the inspected states.

## Fidelity review

- **Fonts:** local macOS SF UI and SF Mono, with bundled egui fallbacks on other platforms; 16-point body/buttons, 24-point modal heading, explicit semibold labels. Text remains legible and aligned. Native rasterization differs slightly from the generated image.
- **Layout:** connections left, terminal center, files right; centered light dialog; Name and Host full width, Username and Port sharing a row, Private key full width; trailing Cancel and Save actions. Smaller windows scroll the form while keeping its title and actions visible.
- **Colors:** white content and terminal, pale neutral gray sidebar/modal, dark text, blue focus and primary actions. The app requests light window chrome independently of the OS theme. The primary blue is slightly deeper than the mockup for white-label contrast.
- **Assets:** embedded raster masks from [Tabler Icons](https://github.com/tabler/tabler-icons), with license and provenance in `src/assets/icons/`. No illustrative assets were required. Native fonts are loaded locally, not redistributed.
- **Copy:** preserves the chosen form labels and helper text. Existing Edit, Remove, Up, Refresh, Disconnect, Files toggle, and transfer guidance remain available because they control existing functionality. Redundant mockup group labels and decorative status dots were omitted.

## Validation and limits

- `cargo test --locked`: 10 passed, one existing live SFTP integration test ignored because it requires its SSH fixture.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo fmt --check` and `git diff --check`: passed.
- Rendered-input tests cover opening the modal, initial field focus and typing, validation, saving/persistence, Cancel/Escape, blocking file drops during a modal, and keeping controls visible with many hosts in a small window.
- Actual f.lux display filtering is not part of framebuffer capture. No live remote SSH/SFTP server was exercised for this appearance change.

## Follow-up polish

P3: native font rasterization, icon silhouettes, and minor row spacing differ from the generated mockup. The existing operational controls add a little density to the sidebar and file toolbar.

Implementation checklist: complete for the selected light design and inspected states.
