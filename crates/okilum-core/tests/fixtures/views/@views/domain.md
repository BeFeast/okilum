---
type: view
view: query
title: Domain
params: {domain: area}
materialize: true
---
# $domain

Active projects first, then the last 30 days of dated entries, then reading material.

## Projects
```view
from: notes
where: area = $domain and kind = project and status = active
let: {idle: "days(updated, today)"}
cols: [title, context, idle, tasks_open]
sort: -idle
show: table
```

## Log
```view
from: notes
where: area = $domain and layer = log and date >= today-30d
group: kind
at: date
show: timeline
```

## Knowledge and sources
```view
from: notes
where: area = $domain and layer in (knowledge, sources)
sort: -updated
limit: 20
show: list
```
