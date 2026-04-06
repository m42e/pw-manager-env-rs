# Examples

The examples below are mainly inline `create_view` references. They can be used directly as element-array patterns for chat rendering. If you are saving a `.excalidraw` file instead, convert them into full scene JSON and remove inline-only pseudo-elements.

See [photosynthesis-example.md](./photosynthesis-example.md) for a full multi-camera explanatory example.

## Two Connected Boxes

Use when the diagram only needs a single relationship.

```json
[
  { "type": "cameraUpdate", "width": 800, "height": 600, "x": 40, "y": 40 },
  { "type": "text", "id": "title", "x": 255, "y": 40, "text": "Simple Flow", "fontSize": 24 },
  { "type": "rectangle", "id": "start", "x": 100, "y": 130, "width": 200, "height": 90, "roundness": { "type": 3 }, "backgroundColor": "#a5d8ff", "fillStyle": "solid", "label": { "text": "Start", "fontSize": 20 } },
  { "type": "rectangle", "id": "end", "x": 430, "y": 130, "width": 200, "height": 90, "roundness": { "type": 3 }, "backgroundColor": "#b2f2bb", "fillStyle": "solid", "label": { "text": "End", "fontSize": 20 } },
  { "type": "arrow", "id": "flow", "x": 300, "y": 175, "width": 130, "height": 0, "points": [[0,0],[130,0]], "endArrowhead": "arrow", "label": { "text": "passes to", "fontSize": 16 }, "start": { "id": "start" }, "end": { "id": "end" } }
]
```

## Revise From Checkpoint

Use when the user wants to continue or revise an existing inline diagram without resending the whole scene.

See [animation.md](./animation.md) for the full checkpoint and animation workflow.

```json
[
  { "type": "restoreCheckpoint", "id": "<checkpointId>" },
  { "type": "cameraUpdate", "width": 800, "height": 600, "x": 40, "y": 40 },
  { "type": "delete", "ids": "flow,end" },
  { "type": "rectangle", "id": "review", "x": 430, "y": 130, "width": 220, "height": 90, "roundness": { "type": 3 }, "backgroundColor": "#fff3bf", "fillStyle": "solid", "label": { "text": "Review", "fontSize": 20 } },
  { "type": "arrow", "id": "flow2", "x": 300, "y": 175, "width": 130, "height": 0, "points": [[0,0],[130,0]], "endArrowhead": "arrow", "label": { "text": "routes to", "fontSize": 16 }, "start": { "id": "start" }, "end": { "id": "review" } }
]
```

## Layered Architecture

Use three horizontal zones.

- Top row: user-facing surfaces.
- Middle row: application or agent logic.
- Bottom row: data stores, external systems, or supporting services.
- In inline mode, draw the zone background rectangle first, then the zone title, then the nodes inside it.
- Use opacity `30` for zone backgrounds so arrows and labels remain readable.

Good patterns:

- Use pale background rectangles for each zone.
- Put titles inside the zone near the upper-left corner.
- Keep arrows mostly vertical between layers and horizontal within a layer.
- For larger diagrams, move the camera to each layer before drawing it, then zoom out for the final overview.

## Sequence Diagram Layout

Use evenly spaced vertical columns.

See [sequence-diagrams.md](./sequence-diagrams.md) for a full worked example.

- Put actor headers in a top row.
- Keep lifelines aligned below headers.
- Place arrows on separate y-levels with enough vertical gap for labels.
- Avoid crossing arrows unless the diagram is very small.
- In inline mode, a good camera pattern is: title view, actor columns right-to-left, then message flow top-to-bottom.

## Timeline Layout

Use a single horizontal spine with milestone markers.

- Keep milestone spacing roughly even.
- Put short labels above or below alternating markers.
- Group dense sub-steps into one milestone plus a note instead of many tiny stops.
- If the timeline is long, reveal it in sections with multiple `cameraUpdate` entries instead of shrinking everything into one unreadable frame.