Here’s a quick way to read a file in **Python**:

```python
with open("notes.md") as f:
    text = f.read()
```

Key points:

1. `with` closes the file for you.
2. Use `encoding="utf-8"` for non-ASCII text, e.g. *naïve* café.
   - Works on Windows too
