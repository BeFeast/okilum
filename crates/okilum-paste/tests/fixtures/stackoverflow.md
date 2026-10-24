Use [`Vec::retain`](https://doc.rust-lang.org/std/vec/struct.Vec.html#method.retain); it keeps the order:

```rust
let mut v = vec![1, 2, 3];
v.retain(|&x| x != 2);
```

> Retains only the elements specified by the predicate.

See also [this question](https://stackoverflow.com/questions/26243025/remove-an-element-from-a-vector) and the *\*\_mut* variants.

---

Edited for Rust 2024.
