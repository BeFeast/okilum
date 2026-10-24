You can do this with a small `match` expression:

```rust
match value {
    Some(x) => x,
    None => 0,
}
```

This works because:

- **Exhaustiveness** – the compiler checks every case
- Each arm returns an `i32`
