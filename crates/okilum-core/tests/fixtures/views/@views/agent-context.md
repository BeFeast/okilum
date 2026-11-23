---
type: view
view: query
title: Agent context
params: {context: {type: context, default: okilum}, task: {type: text, default: ""}}
budget: 12kb
---
# Context: $context

Read top to bottom. A decision newer than the state wins. Cite the row's path and rev.

## State
```view
from: sections
where: note.context = $context and note.layer = state
show: sections
```

## Decisions
```view
from: notes
where: context = $context and kind = decisions
sort: -date
limit: 5
show: sections
```

## For the task
```view
from: notes
near: $task
where: layer in (knowledge, state)
limit: 8
show: list
```
