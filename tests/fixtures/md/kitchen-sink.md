---
title: Kitchen Sink
author: emde
tags: [markdown, test]
---

# Kitchen Sink

A paragraph with *emphasis*, **strong**, ***both***, ~~strikethrough~~,
`inline code`, a [link](https://example.com "Example"), an autolink
<https://example.org/path>, and a bare URL https://www.rust-lang.org/learn.
Hard break at the end of this line  
and a backslash hard break\
then more text. Footnote reference[^note] and another[^2].

## Headings

### Third level

#### Fourth level

##### Fifth level

###### Sixth level

## Lists

- Tight item one
- Tight item with **bold**
  - Nested item
    - Deeper item
- [ ] Unchecked task
- [x] Checked task

1. First
2. Second

   With a second paragraph.
3. Third

## Quotes and alerts

> A plain quote with a [reference link][ref].
>
> > A nested quote.

> [!NOTE]
> Notes carry an icon and a title.

> [!TIP]
> A tip.

> [!IMPORTANT]
> Something important.

> [!WARNING]
> Careful: this is a *warning*.

> [!CAUTION]
> Mind the risks.

## Code

```rust title="hello.rs"
fn main() {
	println!("Hello, {}!", "world");
}
```

    indented code block

```diff
-old line
+new line
```

## Math

Inline math $e^{i\pi} + 1 = 0$ and \(x^2\), and money: $5 and $10, or $5-$10.

$$
\sum_{i=1}^{n} i^2 = \frac{n(n+1)(2n+1)}{6}
$$

```math
A = \begin{pmatrix} a & b \\ c & d \end{pmatrix}
```

## Table

| Left | Center | Right |
|:-----|:------:|------:|
| `a`  | **b**  | $c$   |
| long cell with a [link](#lists) | x[^note] | 3 |

## Other blocks

Term
: Its definition.

---

<details>
<summary>Click to expand</summary>

Hidden content with <kbd>Ctrl</kbd>+<kbd>C</kbd> and H<sub>2</sub>O and x<sup>2</sup>.

</details>

<div align="center">

Centred text, <mark>marked</mark> and <u>underlined</u>.

</div>

![A figure](figure.png "Figure caption")

Text with an ![inline image](chip.png) chip and a [![badge](badge.svg)](https://ci.example.com) badge.

日本語のテキストと
English mixed with emoji 👨‍👩‍👧 and ❤️.

[ref]: https://example.net
[^note]: The footnote text, with *emphasis*.
[^2]: Second footnote.
[^unused]: Never referenced, so dropped.
