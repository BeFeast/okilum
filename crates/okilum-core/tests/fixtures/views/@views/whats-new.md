---
type: view
view: query
title: What's new
materialize: true
---
# What's new

Notes in state and knowledge changed in the last 7 days, newest first.

```view
id: changed
from: notes
where: layer in (state, knowledge) and mtime >= today-7d
let: {day: "day(mtime)"}
group: -day
sort: -mtime
cols: [title, area, layer]
show: list
```

## What the gardener did
```view
from: "@views/changes.jsonl"
where: ts >= today-7d
sort: -ts
show: log
```
