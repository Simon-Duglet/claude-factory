# Patched egui 0.36.2

Copy of egui 0.36.2 with changes (search for "claude-factory patch") so that label text selection can cover more
than the part of a scroll area that is on screen, which the chat log needs to drag-select and Ctrl+A a long
conversation. Stock egui drops the selection as soon as one end of it scrolls out of view.

- `widgets/label.rs`: a selectable label out of view still goes through the selection while one is active.
- `text_selection/label_text_selection.rs`:
  - `LabelSelectionState::is_active(ctx)` and `LabelSelectionState::select_all(ctx, on)` (every label drawn while on is fully selected and copied).
  - The drag logic no longer clips label rects to the view, and a pointer dragged out of the scroll area counts as over the label it lines up with.

When bumping egui, re-apply these changes (or drop the vendor directory and the `[patch.crates-io]` entry if upstream supports it).
