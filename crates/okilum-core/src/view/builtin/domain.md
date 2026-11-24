---
type: view
params:
  domain: area
  context: {type: context, default: "-"}
---
# $domain

## Projects and contexts

```view
id: contexts
from: notes
let: {selected_context: "lower($context)"}
where: area = $domain and layer = state and kind in (project, area) and (selected_context = "-" or context = $context)
group: status
order: {status: [active, parked, done]}
sort: [-updated]
cols: [title, context]
show: list
```

## Recent decisions, reports and handovers

```view
id: recent_log
from: notes
let: {selected_context: "lower($context)"}
where: area = $domain and layer = log and kind in (decisions, reports, handovers) and date >= today-30d and (selected_context = "-" or context = $context)
sort: [-date]
cols: [title, kind, context, date]
limit: 20
show: list
```

## Open tasks

```view
id: open_tasks
from: tasks
let: {selected_context: "lower($context)"}
where: checked = false and note.area = $domain and note.layer in (state, log) and (selected_context = "-" or note.context = $context)
sort: [due, priority]
cols: [text, note.title, due, priority]
limit: 25
show: list
```

## Inbox

```view
id: inbox
from: notes
where: layer = inbox and tags = $domain
sort: [-updated]
cols: [title, updated]
limit: 20
show: list
```

## Sources

```view
id: sources
from: notes
where: layer = sources and area = $domain
sort: [-updated]
cols: [title, channel, updated]
limit: 20
show: list
```
