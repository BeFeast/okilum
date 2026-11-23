---
type: view
view: query
title: Waiting for Oleg
materialize: true
---
# Waiting for Oleg

Ticking a box approves the proposal; the gardener applies it. After 14 days the `default` applies.

```view
from: tasks
where: checked = false and owner = oleg and note.type = questions
let: {age: "days(created, today)"}
group: note.context
sort: [due, -age]
cols: [text, default, due, age]
show: list
actions: [check, {open: thread}]
```
