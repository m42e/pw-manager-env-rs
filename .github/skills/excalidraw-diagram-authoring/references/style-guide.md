# Style Guide

This guide applies to both inline `create_view` diagrams and saved `.excalidraw` scene files.

## Output Modes

### Inline `create_view`

- Call `read_me` once, then use `create_view`.
- Start with `cameraUpdate`, unless dark mode needs the large background rectangle first.
- Prefer shape `label` over separate text for normal node labels.
- Emit in drawing order: background, shape, label, arrows, then the next shape.
- `restoreCheckpoint` and `delete` are valid for revisions and animation-style transformations.
- Never reuse an id after a `delete`.

### Saved `.excalidraw` Scene Files

- Convert any inline-only planning features into standard scene JSON before saving.
- Do not leave `cameraUpdate`, `restoreCheckpoint`, or `delete` in a stored scene file.
- Expect to use explicit text elements more often in saved scene JSON.

## Palette

### Primary Colors

- Blue: `#4a9eed`
- Amber: `#f59e0b`
- Green: `#22c55e`
- Red: `#ef4444`
- Purple: `#8b5cf6`
- Pink: `#ec4899`
- Cyan: `#06b6d4`
- Lime: `#84cc16`

### Light Fills

- Light Blue: `#a5d8ff`
- Light Green: `#b2f2bb`
- Light Orange: `#ffd8a8`
- Light Purple: `#d0bfff`
- Light Red: `#ffc9c9`
- Light Yellow: `#fff3bf`
- Light Teal: `#c3fae8`
- Light Pink: `#eebefa`

### Background Zones

- Blue zone: `#dbe4ff`
- Purple zone: `#e5dbff`
- Green zone: `#d3f9d8`

Use zone backgrounds with opacity `30` so foreground content stays readable.

## Readability Rules

- Body text: 16px minimum.
- Titles: 20px minimum.
- Secondary annotations: 14px minimum and use sparingly.
- Inline diagrams should use one of the supported 4:3 camera sizes: `400x300`, `600x450`, `800x600`, `1200x900`, or `1600x1200`.
- Do not use emoji in diagram text.
- Keep text contrast high on light fills.

## Layout Rules

- Favor a few large shapes over many tiny shapes.
- Use `120x60` or larger for labeled boxes and ellipses.
- Leave at least `20-30px` between neighboring elements.
- Keep labels concise.
- Use consistent spacing within the same row or column.
- Reserve decorative elements for the end and only when they add meaning.
- For large inline diagrams, move the camera section by section instead of trying to show everything at once.

## Labels And Text

- In inline `create_view` mode, use shape `label` for centered node text whenever possible.
- Use standalone `text` elements for titles, subtitles, and annotations.
- Keep arrow labels short so they fit comfortably on the line.
- If you need to center standalone text manually, estimate width as `text.length * fontSize * 0.5`.

## Contrast Guidance

- On white or pale fills, avoid washed-out gray text.
- For green-tinted fills, darker green text works better than the bright fill color itself.
- For blue-tinted fills, darker blue text works better than the bright fill color itself.
- White text should be used only on genuinely dark fills.
- Minimum safe text color on white is `#757575`.

## Theming

See [theming.md](./theming.md) for dark-mode values and theme-specific color guidance.