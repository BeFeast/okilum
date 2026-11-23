---
type: view
view: query
title: Okilum
---
# Okilum

## Now
```view
from: sections
where: note.context = okilum and note.layer = state and heading in (Now, Next, Blockers)
show: sections
```

## Decisions
```view
from: notes
where: context = okilum and kind = decisions
cols: [title, date, decision_status]
sort: -date
limit: 10
show: table
```

## Tasks
```view
from: tasks
where: note.context = okilum and checked = false
sort: [due, priority]
cols: [text, due, owner]
show: list
actions: [check]
```
