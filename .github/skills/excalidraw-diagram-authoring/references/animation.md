# Animation And Checkpoints

Use this reference when continuing a prior inline `create_view` diagram, deleting or replacing elements, or building animation-style transformations during streaming.

## Checkpoints

Every `create_view` call returns a `checkpointId` in its response. To continue from a previous diagram state, start the elements array with a `restoreCheckpoint` element:

```json
[{"type":"restoreCheckpoint","id":"<checkpointId>"}, ...additional new elements...]
```

The saved state, including user edits made in fullscreen, is loaded from the client and your new elements are appended on top. This saves tokens because you do not need to resend the full diagram.

## Deleting Elements

Remove elements by id with the `delete` pseudo-element:

```json
{"type":"delete","ids":"b2,a1,t3"}
```

This works in two modes:

- With `restoreCheckpoint`: restore a saved state, then surgically remove specific elements before adding new ones.
- Inline animation mode: draw elements, then delete and replace them later in the same array to create transformation effects.

Place delete entries after the elements you want to remove. The final render filters them out.

Every element id must be unique. Never reuse an id after deleting it.

## Animation Mode

Instead of building strictly left to right, animate by deleting elements and replacing them at the same position. Combined with slight camera moves, this creates smooth transformations during streaming.

Pattern:

1. Draw the initial elements.
2. Add a `cameraUpdate` with a slight shift or zoom.
3. Add `{"type":"delete","ids":"old1,old2"}`.
4. Draw the replacements at the same coordinates with different ids.
5. Repeat.

Example prompt: `Pixel snake eats apple`.

```json
[
  {"type":"cameraUpdate","width":400,"height":300,"x":0,"y":0},
  {"type":"ellipse","id":"ap","x":260,"y":78,"width":20,"height":20,"backgroundColor":"#ef4444","fillStyle":"solid","strokeColor":"#ef4444"},
  {"type":"rectangle","id":"s0","x":60,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"rectangle","id":"s1","x":88,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"rectangle","id":"s2","x":116,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"rectangle","id":"s3","x":144,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"cameraUpdate","width":400,"height":300,"x":1,"y":0},
  {"type":"rectangle","id":"s4","x":172,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s0"},
  {"type":"cameraUpdate","width":400,"height":300,"x":0,"y":1},
  {"type":"rectangle","id":"s5","x":200,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s1"},
  {"type":"cameraUpdate","width":400,"height":300,"x":1,"y":0},
  {"type":"rectangle","id":"s6","x":228,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s2"},
  {"type":"cameraUpdate","width":400,"height":300,"x":0,"y":0},
  {"type":"rectangle","id":"s7","x":256,"y":130,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s3"},
  {"type":"cameraUpdate","width":400,"height":300,"x":1,"y":1},
  {"type":"rectangle","id":"s8","x":256,"y":102,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s4"},
  {"type":"cameraUpdate","width":400,"height":300,"x":0,"y":0},
  {"type":"rectangle","id":"s9","x":256,"y":74,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"ap"},
  {"type":"cameraUpdate","width":400,"height":300,"x":1,"y":0},
  {"type":"rectangle","id":"s10","x":256,"y":46,"width":28,"height":28,"backgroundColor":"#22c55e","fillStyle":"solid","strokeColor":"#15803d","strokeWidth":1},
  {"type":"delete","ids":"s5"}
]
```

## Key Techniques

- Add head plus delete tail each frame for the movement illusion.
- On eat, delete the apple instead of the tail so the snake grows.
- Resume normal add-head and delete-tail after growth.
- Use tiny camera nudges such as `(0,0) -> (1,0) -> (0,1)` for extra motion.
- Always use new ids for added segments.