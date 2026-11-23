---
type: view
view: query
title: Areas
sidebar: true
params: {context: context, layer: layer}
---
```view
from: notes
show: tree
levels: [area, context, layer]
order: {layer: [state, log, knowledge, sources, inbox]}
node_view: "[[@views/domain]]"
```
