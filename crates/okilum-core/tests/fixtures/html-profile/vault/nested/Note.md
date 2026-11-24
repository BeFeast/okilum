---
private: frontmatter-secret
---
# A *Mixed* `Heading`!

[[#a mixed heading!|Jump here]] · [Target](../Other%20Note.md#Setext%20title) · [[Other Note#Setext title|Open target]]

[[Missing|Missing label]] · [[Twin|Choose a twin]] · [[Other Note#No such heading|Wrong heading]] · [[Other Note#Repeated|Repeated heading]]

![[diagram.png]] ![Local **image**](diagram.png) [PDF](../files/Guide%20%231.pdf)

![[Other Note#Excerpt]]

> [!warning]- Custom title
> Read [[Other Note]] first.

| Link | Value |
| --- | --- |
| [[Other Note\|Table alias]] | **Bold** |

- [x] Done
- [ ] Pending

Inline $x^2 + y$ and `%% literal %%`. %%comment-secret%%

$$
x^2 + y^2 = z^2
$$

```rust
let markup = "<script>"; // %% literal %%
```

[Site](https://example.com/?a=1&b=2) · [Script](javascript:alert%281%29) · [Email](mailto:user@example.com) · [Forged](okilum-note:Other%20Note.md) · [Network path](//example.com/a.pdf)

![Remote](https://example.com/pixel.png) ![Missing image](missing.png) ![Local file URI](file:///outside.png)

<script>html-secret</script>

Text <img src="https://example.com/pixel.png" onerror="bad()"> survives.

## Local target

Text with a block ID. ^sample

[Block](#%5Esample)
