# Sequence Diagrams

Use this reference when the user wants a UML-style interaction flow, actor-based request and response diagram, or any sequence-oriented inline `create_view` diagram.

## Layout Pattern

- Use evenly spaced vertical actor columns.
- Put actor headers in a top row.
- Draw dashed lifelines below the headers.
- Place message arrows on separate y-levels with enough vertical gap for labels.
- Avoid crossing arrows unless the diagram is very small.
- Keep arrow labels short enough to fit comfortably on the line.

## Camera Pattern

Sequence diagrams are much easier to read when the camera reveals them in stages.

- Start with a title view.
- Pan across actor columns one by one, usually right to left so the camera can snake back toward the first message flow.
- Zoom back out for the first exchange.
- Pan downward as later messages appear.
- Finish with a wider overview.

## Worked Example

Example prompt: `show a sequence diagram explaining tool-enabled apps`.

This demonstrates a UML-style sequence diagram with four actors, dashed lifelines, and labeled arrows showing the request and response flow. The camera pans progressively across the diagram.

- Camera 1: `600x450` draws the title.
- Cameras 2-5: `400x300` zoom into each actor column from right to left.
- Camera 6: `400x300` zooms into the user actor to draw the stick figure.
- Camera 7: `600x450` zooms out for the first message arrows.
- Camera 8: `600x450` pans down for user interaction and app callbacks.
- Camera 9: `600x450` pans further down for forwarded tool calls and fresh data.
- Camera 10: `800x600` shows the completed sequence.

```json
[
  {"type":"cameraUpdate","width":600,"height":450,"x":80,"y":-10},
  {"type":"text","id":"title","x":150,"y":15,"text":"Tool-Enabled Apps - Sequence Flow","fontSize":24,"strokeColor":"#1e1e1e"},

  {"type":"cameraUpdate","width":400,"height":300,"x":450,"y":-5},
  {"type":"rectangle","id":"sHead","x":600,"y":60,"width":130,"height":40,"backgroundColor":"#ffd8a8","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#f59e0b","strokeWidth":2,"label":{"text":"Tool Server","fontSize":16}},
  {"type":"arrow","id":"sLine","x":665,"y":100,"width":0,"height":490,"points":[[0,0],[0,490]],"strokeColor":"#b0b0b0","strokeWidth":1,"strokeStyle":"dashed","endArrowhead":null},

  {"type":"cameraUpdate","width":400,"height":300,"x":250,"y":-5},
  {"type":"rectangle","id":"appHead","x":400,"y":60,"width":130,"height":40,"backgroundColor":"#b2f2bb","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#22c55e","strokeWidth":2,"label":{"text":"App iframe","fontSize":16}},
  {"type":"arrow","id":"appLine","x":465,"y":100,"width":0,"height":490,"points":[[0,0],[0,490]],"strokeColor":"#b0b0b0","strokeWidth":1,"strokeStyle":"dashed","endArrowhead":null},

  {"type":"cameraUpdate","width":400,"height":300,"x":80,"y":-5},
  {"type":"rectangle","id":"aHead","x":230,"y":60,"width":100,"height":40,"backgroundColor":"#d0bfff","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#8b5cf6","strokeWidth":2,"label":{"text":"Agent","fontSize":16}},
  {"type":"arrow","id":"aLine","x":280,"y":100,"width":0,"height":490,"points":[[0,0],[0,490]],"strokeColor":"#b0b0b0","strokeWidth":1,"strokeStyle":"dashed","endArrowhead":null},

  {"type":"cameraUpdate","width":400,"height":300,"x":-10,"y":-5},
  {"type":"rectangle","id":"uHead","x":60,"y":60,"width":100,"height":40,"backgroundColor":"#a5d8ff","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#4a9eed","strokeWidth":2,"label":{"text":"User","fontSize":16}},
  {"type":"arrow","id":"uLine","x":110,"y":100,"width":0,"height":490,"points":[[0,0],[0,490]],"strokeColor":"#b0b0b0","strokeWidth":1,"strokeStyle":"dashed","endArrowhead":null},

  {"type":"cameraUpdate","width":400,"height":300,"x":-40,"y":50},
  {"type":"ellipse","id":"uh","x":58,"y":110,"width":20,"height":20,"backgroundColor":"#a5d8ff","fillStyle":"solid","strokeColor":"#4a9eed","strokeWidth":2},
  {"type":"rectangle","id":"ub","x":57,"y":132,"width":22,"height":26,"backgroundColor":"#a5d8ff","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#4a9eed","strokeWidth":2},

  {"type":"cameraUpdate","width":600,"height":450,"x":-20,"y":-30},
  {"type":"arrow","id":"m1","x":110,"y":135,"width":170,"height":0,"points":[[0,0],[170,0]],"strokeColor":"#1e1e1e","strokeWidth":2,"endArrowhead":"arrow","label":{"text":"display a chart","fontSize":14}},
  {"type":"rectangle","id":"note1","x":130,"y":162,"width":310,"height":26,"backgroundColor":"#fff3bf","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#f59e0b","strokeWidth":1,"opacity":50,"label":{"text":"Interactive app rendered in chat","fontSize":14}},

  {"type":"cameraUpdate","width":600,"height":450,"x":170,"y":25},
  {"type":"arrow","id":"m2","x":280,"y":210,"width":385,"height":0,"points":[[0,0],[385,0]],"strokeColor":"#8b5cf6","strokeWidth":2,"endArrowhead":"arrow","label":{"text":"tools/call","fontSize":16}},
  {"type":"arrow","id":"m3","x":665,"y":250,"width":-385,"height":0,"points":[[0,0],[-385,0]],"strokeColor":"#f59e0b","strokeWidth":2,"endArrowhead":"arrow","strokeStyle":"dashed","label":{"text":"tool input/result","fontSize":16}},
  {"type":"arrow","id":"m4","x":280,"y":290,"width":185,"height":0,"points":[[0,0],[185,0]],"strokeColor":"#8b5cf6","strokeWidth":2,"endArrowhead":"arrow","strokeStyle":"dashed","label":{"text":"result -> app","fontSize":16}},

  {"type":"cameraUpdate","width":600,"height":450,"x":-10,"y":135},
  {"type":"arrow","id":"m5","x":110,"y":340,"width":355,"height":0,"points":[[0,0],[355,0]],"strokeColor":"#4a9eed","strokeWidth":2,"endArrowhead":"arrow","label":{"text":"user interacts","fontSize":16}},
  {"type":"arrow","id":"m6","x":465,"y":380,"width":-185,"height":0,"points":[[0,0],[-185,0]],"strokeColor":"#22c55e","strokeWidth":2,"endArrowhead":"arrow","label":{"text":"tools/call request","fontSize":16}},

  {"type":"cameraUpdate","width":600,"height":450,"x":170,"y":235},
  {"type":"arrow","id":"m7","x":280,"y":420,"width":385,"height":0,"points":[[0,0],[385,0]],"strokeColor":"#8b5cf6","strokeWidth":2,"endArrowhead":"arrow","label":{"text":"tools/call (forwarded)","fontSize":16}},
  {"type":"arrow","id":"m8","x":665,"y":460,"width":-385,"height":0,"points":[[0,0],[-385,0]],"strokeColor":"#f59e0b","strokeWidth":2,"endArrowhead":"arrow","strokeStyle":"dashed","label":{"text":"fresh data","fontSize":16}},
  {"type":"arrow","id":"m9","x":280,"y":500,"width":185,"height":0,"points":[[0,0],[185,0]],"strokeColor":"#8b5cf6","strokeWidth":2,"endArrowhead":"arrow","strokeStyle":"dashed","label":{"text":"fresh data","fontSize":16}},

  {"type":"cameraUpdate","width":600,"height":450,"x":50,"y":327},
  {"type":"rectangle","id":"note2","x":130,"y":522,"width":310,"height":26,"backgroundColor":"#d3f9d8","fillStyle":"solid","roundness":{"type":3},"strokeColor":"#22c55e","strokeWidth":1,"opacity":50,"label":{"text":"App updates with new data","fontSize":14}},
  {"type":"arrow","id":"m10","x":465,"y":570,"width":-185,"height":0,"points":[[0,0],[-185,0]],"strokeColor":"#22c55e","strokeWidth":2,"endArrowhead":"arrow","strokeStyle":"dashed","label":{"text":"context update","fontSize":16}},

  {"type":"cameraUpdate","width":800,"height":600,"x":-5,"y":2}
]
```