---
type: view
---

## Task rows

```view
from: tasks
show: table
actions: [{set: due}]
```

## Section rows

```view
from: sections
show: table
actions: [{set: owner}]
```

## JSONL rows

```view
from: "@views/changes.jsonl"
show: table
actions: [{set: status}]
```
