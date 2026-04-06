---
name: excalidraw-diagram-authoring
description: 'Author, edit, animate, publish, fetch, or self-host Excalidraw diagrams. Use when users ask to draw, sketch, visualize, revise, stream a diagram with create_view, continue from a checkpoint, use cameraUpdate or restoreCheckpoint or delete, share or fetch scene payloads, or wire Excalidraw services. Supports both inline create_view element arrays and standard .excalidraw scene JSON.'
argument-hint: 'Describe the diagram goal, whether you need inline create_view drawing or a .excalidraw file, target file if any, and whether you need authoring, editing, publishing, fetching, or self-hosting help.'
user-invocable: true
---

# Excalidraw Diagram Authoring

## What This Skill Produces

- Inline `create_view` element arrays for chat-rendered Excalidraw diagrams, including camera moves, checkpoints, deletes, and animation-style transformations.
- New `.excalidraw` scene files using [scene-template.json](./assets/scene-template.json).
- Targeted edits to existing `.excalidraw` scenes or to prior `create_view` checkpoints.
- Share links created through [publish-scene-link.mjs](./scripts/publish-scene-link.mjs).
- Restored scene JSON from share links through [fetch-scene.mjs](./scripts/fetch-scene.mjs).
- Self-hosting and store API guidance through the bundled references.

## When To Use

- A user wants a diagram drawn inline with `create_view` instead of prose or Mermaid.
- A user wants a prior inline diagram continued with a returned `checkpointId` and `restoreCheckpoint`.
- A user wants a diagram represented as `.excalidraw` scene data in a file.
- A repository already contains `.excalidraw` files that need edits.
- A user wants to publish a local scene to an Excalidraw store endpoint and receive a share URL.
- A user has an Excalidraw share URL and wants the underlying scene JSON restored.
- A user needs help wiring the Excalidraw web app, collaboration room, and store service.

## References

- [composition.md](./references/composition.md)
- [style-guide.md](./references/style-guide.md)
- [examples.md](./references/examples.md)
- [photosynthesis-example.md](./references/photosynthesis-example.md)
- [sequence-diagrams.md](./references/sequence-diagrams.md)
- [animation.md](./references/animation.md)
- [theming.md](./references/theming.md)
- [api-workflows.md](./references/api-workflows.md)
- [self-hosting.md](./references/self-hosting.md)
- [api-env.example](./assets/api-env.example)

## Output Modes

### Inline `create_view`

Use this mode when the user wants the diagram rendered directly in chat, wants animated reveal steps, or wants to continue from a prior `checkpointId`.

- Call `read_me` once to load the current tool contract. Do not call it again in the same conversation.
- Use `create_view` to draw.
- Follow the full element format in the `Excalidraw Element Format` section below.
- Use `restoreCheckpoint` when continuing or revising an earlier inline diagram.

### Standard `.excalidraw` Scene JSON

Use this mode when the user wants a durable file in the repository, wants to publish or fetch a stored scene, or wants edits made directly to an existing `.excalidraw` file.

- Use [scene-template.json](./assets/scene-template.json) for new files.
- Preserve existing top-level scene metadata when editing in place.
- Do not copy `cameraUpdate`, `restoreCheckpoint`, `delete`, or shorthand `label` fields into saved `.excalidraw` scene JSON.

## Procedure

1. Classify the task first.
   - Inline `create_view` diagram.
   - Continue or revise a prior inline checkpoint.
   - New scene file.
   - Edit an existing scene file.
   - Publish a scene to a store endpoint.
   - Fetch a scene back from a share URL.
   - Explain or validate a self-hosted stack.
2. For inline `create_view` tasks, use the chat drawing workflow.
   - Call `read_me` once, then do not call it again in the same conversation.
   - Use `create_view` with the full element array format below.
   - Start with a `cameraUpdate` as the first element, unless dark mode requires the large background rectangle before it.
   - Emit elements progressively in drawing order: background, shape, its label, its arrows, then the next shape.
   - Prefer `label` on rectangles, ellipses, and diamonds instead of separate text elements.
   - Use `restoreCheckpoint` plus `delete` when continuing or surgically revising prior states.
3. For new or edited `.excalidraw` files, work from standard scene JSON.
   - Use [scene-template.json](./assets/scene-template.json) for new files.
   - Preserve existing top-level scene metadata when editing in place.
   - Follow [composition.md](./references/composition.md) and [style-guide.md](./references/style-guide.md).
   - Convert any inline planning features into real Excalidraw scene elements before saving.
4. For publishing, use the bundled script instead of inventing ad hoc crypto logic.
   - Run [publish-scene-link.mjs](./scripts/publish-scene-link.mjs) with a file path, JSON string, or stdin.
   - Prefer endpoint values from environment variables or explicit script flags.
5. For fetching, use the reverse workflow.
   - Run [fetch-scene.mjs](./scripts/fetch-scene.mjs) with a full share URL or explicit `id` and `key`.
   - Write the result to a `.excalidraw` file when the user wants a durable artifact.
6. For self-hosting requests, explain the stack clearly.
   - Web app URL is distinct from store API URLs.
   - Collaboration room URL is distinct from store URLs.
   - Store health and v2 API endpoints should be called directly, not through intermediary tooling.
7. Validate before finishing.
   - Scene ids are unique.
   - Inline `create_view` arrays use only valid element or pseudo-element types.
   - Bound arrows use `start` and `end`, not `startBinding` or `endBinding`.
   - `cameraUpdate` sizes use one of the exact supported 4:3 dimensions.
   - Deleted ids are never reused.
   - Text contrast and spacing are readable.
   - Saved `.excalidraw` files contain valid scene JSON and no inline-only pseudo-elements.
   - API scripts have the minimum arguments they need.

## Branching Rules

- If the user wants an inline diagram, streaming reveal, animation, or checkpoint continuation, use `create_view`.
- If the user wants a durable repo artifact, prefer writing or editing a `.excalidraw` file directly.
- If the user explicitly wants a share URL, publish the scene with [publish-scene-link.mjs](./scripts/publish-scene-link.mjs).
- If the user starts from a share URL, recover the source scene with [fetch-scene.mjs](./scripts/fetch-scene.mjs) before editing.
- If the user is wiring infrastructure, consult [self-hosting.md](./references/self-hosting.md) before making assumptions about app, room, and store URLs.

## Excalidraw Element Format

Call `read_me` once. Do not call it again in the same conversation; it will not return anything new. After that, use `create_view` to draw.

### Color Palette

Use the same palette consistently across all tools.

#### Primary Colors

| Name | Hex | Use |
|------|-----|-----|
| Blue | `#4a9eed` | Primary actions, links, data series 1 |
| Amber | `#f59e0b` | Warnings, highlights, data series 2 |
| Green | `#22c55e` | Success, positive, data series 3 |
| Red | `#ef4444` | Errors, negative, data series 4 |
| Purple | `#8b5cf6` | Accents, special items, data series 5 |
| Pink | `#ec4899` | Decorative, data series 6 |
| Cyan | `#06b6d4` | Info, secondary, data series 7 |
| Lime | `#84cc16` | Extra, data series 8 |

#### Excalidraw Fills

Pastel fills for shape backgrounds.

| Color | Hex | Good For |
|-------|-----|----------|
| Light Blue | `#a5d8ff` | Input, sources, primary nodes |
| Light Green | `#b2f2bb` | Success, output, completed |
| Light Orange | `#ffd8a8` | Warning, pending, external |
| Light Purple | `#d0bfff` | Processing, middleware, special |
| Light Red | `#ffc9c9` | Error, critical, alerts |
| Light Yellow | `#fff3bf` | Notes, decisions, planning |
| Light Teal | `#c3fae8` | Storage, data, memory |
| Light Pink | `#eebefa` | Analytics, metrics |

#### Background Zones

Use opacity `30` for layered diagrams.

| Color | Hex | Good For |
|-------|-----|----------|
| Blue zone | `#dbe4ff` | UI / frontend layer |
| Purple zone | `#e5dbff` | Logic / agent layer |
| Green zone | `#d3f9d8` | Data / tool layer |

### Excalidraw Elements

#### Required Fields

All elements need `type`, `id` (unique string), `x`, `y`, `width`, and `height`.

#### Defaults

Skip these unless you need to override them:

- `strokeColor="#1e1e1e"`
- `backgroundColor="transparent"`
- `fillStyle="solid"`
- `strokeWidth=2`
- `roughness=1`
- `opacity=100`
- Canvas background is white.

#### Element Types

**Rectangle**

```json
{ "type": "rectangle", "id": "r1", "x": 100, "y": 100, "width": 200, "height": 100 }
```

- Use `roundness: { type: 3 }` for rounded corners.
- Use `backgroundColor: "#a5d8ff"` and `fillStyle: "solid"` for filled rectangles.

**Ellipse**

```json
{ "type": "ellipse", "id": "e1", "x": 100, "y": 100, "width": 150, "height": 150 }
```

**Diamond**

```json
{ "type": "diamond", "id": "d1", "x": 100, "y": 100, "width": 150, "height": 150 }
```

**Labeled shape (preferred)**

Add `label` to any shape for auto-centered text. Do not add a separate text element unless you need a title or annotation.

```json
{ "type": "rectangle", "id": "r1", "x": 100, "y": 100, "width": 200, "height": 80, "label": { "text": "Hello", "fontSize": 20 } }
```

- Works on rectangle, ellipse, and diamond.
- Text auto-centers and the container auto-resizes to fit.
- This saves tokens versus a separate text element.

**Labeled arrow**

Use `"label": { "text": "connects" }` on an arrow element.

**Standalone text**

Use this only for titles and annotations.

```json
{ "type": "text", "id": "t1", "x": 150, "y": 138, "text": "Hello", "fontSize": 20 }
```

- `x` is the left edge of the text.
- To center text at position `cx`, set `x = cx - estimatedWidth / 2`.
- Estimate width as `text.length * fontSize * 0.5`.
- Do not rely on `textAlign` or `width` for positioning. They only affect multi-line wrapping.

**Arrow**

```json
{ "type": "arrow", "id": "a1", "x": 300, "y": 150, "width": 200, "height": 0, "points": [[0,0],[200,0]], "endArrowhead": "arrow" }
```

- `points` are `[dx, dy]` offsets from the element `x` and `y`.
- `endArrowhead` can be `null`, `"arrow"`, `"bar"`, `"dot"`, or `"triangle"`.

#### Arrow Bindings

```json
{ "start": { "id": "left" }, "end": { "id": "right" } }
```

For `create_view`, bind arrows with `start` and `end` on the arrow element.

- Give every target shape a stable `id`.
- Draw the shapes before the arrow so the binding can be resolved cleanly.
- Keep normal arrow geometry such as `x`, `y`, `width`, `height`, and `points`.
- Do not author `startBinding` or `endBinding` in `create_view` input. Those belong to Excalidraw's expanded internal element model and are generated later.

Correct pattern:

```json
{
   "type": "arrow",
   "id": "link",
   "x": 300,
   "y": 260,
   "width": 200,
   "height": 0,
   "points": [[0, 0], [200, 0]],
   "endArrowhead": "arrow",
   "start": { "id": "left" },
   "end": { "id": "right" },
   "label": { "text": "bound", "fontSize": 16 }
}
```

Incorrect pattern for `create_view`:

```json
{
   "type": "arrow",
   "id": "link",
   "x": 300,
   "y": 260,
   "width": 200,
   "height": 0,
   "points": [[0, 0], [200, 0]],
   "endArrowhead": "arrow",
   "startBinding": { "elementId": "left", "fixedPoint": [1, 0.5] },
   "endBinding": { "elementId": "right", "fixedPoint": [0, 0.5] }
}
```

- Prefer binding by `id`. It is the most reliable way to connect arrows to existing shapes.
- Keep ids stable if you plan to extend the diagram with checkpoints.
- Use the same ids again with `restoreCheckpoint` when adding more connected arrows later.

#### `cameraUpdate`

This is a pseudo-element that controls the viewport and is not drawn.

```json
{ "type": "cameraUpdate", "width": 800, "height": 600, "x": 0, "y": 0 }
```

- `x` and `y` define the top-left corner of the visible area in scene coordinates.
- `width` and `height` define the visible area and must use a 4:3 ratio.
- Supported sizes are `400x300`, `600x450`, `800x600`, `1200x900`, and `1600x1200`.
- Use multiple `cameraUpdate` entries to guide attention while drawing.
- No `id` is needed because it is not a drawn element.

#### `delete`

This pseudo-element removes elements by id.

```json
{ "type": "delete", "ids": "b2,a1,t3" }
```

- Use a comma-separated list of element ids.
- This also removes bound text elements that match `containerId`.
- Place it after the elements you want to remove.
- Never reuse a deleted id. Always create new ids for replacements.

### Drawing Order

Array order is z-order: first is back, last is front.

- Emit progressively: background, then shape, then its label, then its arrows, then the next shape.
- Bad: all rectangles, then all texts, then all arrows.
- Good: background, shape 1, text 1, arrow 1, shape 2, text 2.

#### Example: Two connected labeled boxes

```json
[
  { "type": "cameraUpdate", "width": 800, "height": 600, "x": 50, "y": 50 },
  { "type": "rectangle", "id": "b1", "x": 100, "y": 100, "width": 200, "height": 100, "roundness": { "type": 3 }, "backgroundColor": "#a5d8ff", "fillStyle": "solid", "label": { "text": "Start", "fontSize": 20 } },
  { "type": "rectangle", "id": "b2", "x": 450, "y": 100, "width": 200, "height": 100, "roundness": { "type": 3 }, "backgroundColor": "#b2f2bb", "fillStyle": "solid", "label": { "text": "End", "fontSize": 20 } },
   { "type": "arrow", "id": "a1", "x": 300, "y": 150, "width": 150, "height": 0, "points": [[0,0],[150,0]], "endArrowhead": "arrow", "start": { "id": "b1" }, "end": { "id": "b2" } }
]
```

### Camera And Sizing

The diagram displays inline at about `700px` width. Design for that constraint.

#### Recommended camera sizes

- Camera S: `width 400`, `height 300` for a close-up on a small group.
- Camera M: `width 600`, `height 450` for a medium section view.
- Camera L: `width 800`, `height 600` for a standard full diagram. This is the default.
- Camera XL: `width 1200`, `height 900` for a large overview. Font sizes below `18` become unreadable.
- Camera XXL: `width 1600`, `height 1200` for a panorama. Minimum readable font size is `21`.

Always use one of those exact sizes. Non-4:3 viewports distort the output.

#### Font size rules

- Minimum `fontSize` is `16` for body text, labels, and descriptions.
- Minimum `fontSize` is `20` for titles and headings.
- Minimum `fontSize` is `14` for secondary annotations, and use that sparingly.
- Never use `fontSize` below `14`.

#### Element sizing rules

- Minimum shape size is `120x60` for labeled rectangles and ellipses.
- Leave at least `20-30px` between elements.
- Prefer fewer larger elements over many tiny ones.

In normal mode, start with a `cameraUpdate` as the first element. In dark mode, place the large background rectangle first and the `cameraUpdate` immediately after it.

```json
{ "type": "cameraUpdate", "width": 800, "height": 600, "x": 0, "y": 0 }
```

- Emit the camera move before the content it frames.
- Leave padding around the content instead of matching camera size exactly.

Examples:

- `{ "type": "cameraUpdate", "width": 800, "height": 600, "x": 0, "y": 0 }`
- `{ "type": "cameraUpdate", "width": 400, "height": 300, "x": 200, "y": 100 }`
- `{ "type": "cameraUpdate", "width": 1600, "height": 1200, "x": -50, "y": -50 }`

For large diagrams, move the camera to focus on each section as it appears.

### Worked Example Reference

- For the full multi-camera photosynthesis walkthrough and example-specific pitfalls, see [photosynthesis-example.md](./references/photosynthesis-example.md).

### Advanced Inline Patterns

- Use `restoreCheckpoint` to continue a prior inline diagram by starting the array with `{ "type": "restoreCheckpoint", "id": "<checkpointId>" }`.
- Use `delete` after the elements you want to remove, and never reuse a deleted id.
- For full checkpoint workflows, delete-driven transforms, and the complete animation example, see [animation.md](./references/animation.md).
- For sequence-specific layout guidance, camera choreography, and the full tool-enabled apps example, see [sequence-diagrams.md](./references/sequence-diagrams.md).

### Theming Reference

- For dark-mode background, text, fill, and stroke values, see [theming.md](./references/theming.md).
- Use that reference whenever the user explicitly asks for a dark or specially themed diagram.

### Tips

- Do not call `read_me` again after the first successful call.
- Use the color palette consistently.
- Text contrast is critical. Do not use light gray such as `#b0b0b0` or `#999999` on white backgrounds. Minimum safe text color on white is `#757575`.
- For colored text on light fills, use dark variants such as `#15803d` instead of `#22c55e`, and `#2563eb` instead of `#4a9eed`.
- White text needs genuinely dark backgrounds.
- Do not use emoji in text because they do not render well in Excalidraw's font.
- `cameraUpdate` is one of the best readability tools in this workflow. Use it frequently to guide attention while the diagram is being drawn.

## Completion Checklist

- The output mode matches the user request: inline `create_view` array or standard `.excalidraw` scene JSON.
- Existing scenes keep unrelated metadata unchanged.
- Inline diagrams use the element format above with readable cameras, spacing, and contrast.
- Share and fetch operations use the bundled scripts rather than custom crypto logic.
- Any endpoint assumptions are called out explicitly.
- The result is understandable with or without an inline Excalidraw UI.