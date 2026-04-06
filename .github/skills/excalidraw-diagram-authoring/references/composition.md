# Composition Rules

This skill supports two output targets: inline `create_view` element arrays and standard `.excalidraw` scene JSON.

## Inline `create_view` Rules

- Use the element array format documented in [../SKILL.md](../SKILL.md#excalidraw-element-format).
- `cameraUpdate`, `restoreCheckpoint`, and `delete` are valid pseudo-elements in inline mode.
- Prefer shorthand `label` on rectangles, ellipses, and diamonds.
- Start with `cameraUpdate`, unless dark mode requires the large background rectangle first.
- Emit in drawing order: background, shape, label, arrows, then the next shape.
- Use only the exact supported 4:3 camera sizes.
- Never reuse ids after a `delete`.

## Standard Scene Rules

- Wrap diagrams in a normal Excalidraw scene object with top-level `type`, `version`, `source`, `elements`, `appState`, and `files` fields.
- When editing an existing scene, preserve top-level fields you are not changing.
- Excalidraw elements are verbose and versioned. Prefer adapting nearby elements from an existing scene when possible.

Do not copy inline-only pseudo-elements into saved scene files. `cameraUpdate`, `restoreCheckpoint`, `delete`, and shorthand `label` are for inline `create_view` mode, not stored `.excalidraw` scene JSON.

## Layout Heuristics

- Start with a title zone when the diagram needs one.
- Group related nodes into clear rows or columns.
- Keep 20-30px gaps between neighboring elements at minimum.
- Prefer 120x60 or larger for labeled boxes.
- Keep arrow labels short so they do not dominate the canvas.

## Text Placement

- In inline `create_view` mode, prefer shape `label` unless you need a title or annotation.
- In raw scene JSON, labels usually need explicit text elements.
- To approximately center a single-line label in a shape, estimate text width as `text.length * fontSize * 0.5` and place the text element at:

```text
x = shape.x + (shape.width - estimatedWidth) / 2
y = shape.y + (shape.height - fontSize) / 2
```

- Use that estimate as a starting point only. If editing an existing scene, mirror whatever text-placement pattern the file already uses.

## Arrows And Bindings

- Prefer simple left-to-right or top-to-bottom flows.
- Bind arrows to shapes when the surrounding file already uses bindings.
- Keep polyline complexity low unless the diagram genuinely needs bends.

## Editing Existing Scenes

- Reuse nearby element structure instead of inventing fields from memory.
- Preserve stable scene metadata and unrelated elements.
- Change only what the user asked for.