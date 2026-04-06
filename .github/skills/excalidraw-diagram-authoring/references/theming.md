# Theming

Use this reference when the user asks for a dark diagram, themed variant, or any non-default appearance treatment.

## Dark Mode

If the user asks for a dark diagram, add a large dark background rectangle as the first element before the first `cameraUpdate`:

```json
{"type":"rectangle","id":"darkbg","x":-4000,"y":-3000,"width":10000,"height":7500,"backgroundColor":"#1e1e2e","fillStyle":"solid","strokeColor":"transparent","strokeWidth":0}
```

Make it much larger than the camera so it still covers the viewport while panning.

### Text colors on dark backgrounds

| Color | Hex | Use |
|-------|-----|-----|
| White | `#e5e5e5` | Primary text, titles |
| Muted | `#a0a0a0` | Secondary text, annotations |
| Never | `#555555` or darker | Too dim on dark backgrounds |

### Shape fills on dark backgrounds

| Color | Hex | Good For |
|-------|-----|----------|
| Dark Blue | `#1e3a5f` | Primary nodes |
| Dark Green | `#1a4d2e` | Success, output |
| Dark Purple | `#2d1b69` | Processing, special |
| Dark Orange | `#5c3d1a` | Warning, pending |
| Dark Red | `#5c1a1a` | Error, critical |
| Dark Teal | `#1a4d4d` | Storage, data |

### Stroke and arrow colors on dark backgrounds

Use the primary colors from the main palette. For subtle borders, use slightly lighter variants or `#555555`.

## Theme Rules

- Keep text contrast high enough to remain readable at inline display scale.
- Reuse the normal semantic palette when possible so meaning does not change between light and dark variants.
- Apply theme changes consistently across background, nodes, arrows, and labels.