---
title: Obsidian syntax
---
# Obsidian syntax

A corpus note for Reader compatibility (#651). Every construct below has a
code example next to it that must stay literal.

## Callouts

> [!note] Plain note
> Always open. Holds **Markdown** and a [[obsidian-syntax#Footnotes|link]].

> [!warning]- Folded warning
> Starts closed; the title row opens it.

> [!tip]+ Open tip
> Starts open; the title row closes it.

> [!faq] Default question style

## Highlights and comments

Text with ==a highlight== and %%an inline comment%% that is hidden.

%%
A block comment.
It spans several lines and is hidden.
%%

`==not a highlight==` and `%%not a comment%%` stay literal.

## Footnotes

A claim that needs a source.[^1] A second point.[^long] An inline
footnote.^[Written in place, numbered with the rest.]

A literal `[^1]` in code is not a reference.

[^1]: The source of the claim.
[^long]: A footnote with
    a continuation line.

## Math

Inline $e^{i\pi} + 1 = 0$ next to money: $5 and $10.

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

Literal: `$x$`.

## Block references

A paragraph that can be linked. ^para-1

- First item
- Second item ^item-2

| Column | Value |
| ------ | ----- |
| a      | 1     |

^table-1

Links: [[#^para-1]] and [[obsidian-syntax#^item-2|the second item]].

`^not-an-id` inside code.
