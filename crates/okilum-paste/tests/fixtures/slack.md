Deploy is **done** 🎉 — details in https://ci.example.com/run/42  
Checklist:

- migrations
- smoke tests
  - login
- docs

```bash
kubectl rollout status deploy/api
```

Ping `@oncall` if *anything* breaks.
