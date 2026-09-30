| Left | Center | Right | None |
|:-----|:------:|------:|------|
| a | b | c | d |
| longer cell | with **bold** | 123 | `code span` |

| Wide | Table |
|------|-------|
| This cell has quite a lot of text in it, enough to force wrapping even at eighty columns | short |
| x | Another long cell that should wrap as well, because the table is wider than the terminal |

| 名前 | 説明 |
|------|------|
| 日本語 | テーブルの中の日本語のテキストです |
| emoji 👍 | ❤️ works |

| a | b | c | d | e | f | g |
|---|---|---|---|---|---|---|
| one | two | three | four | five | six | seven |

| Only a header |
|---------------|

| Math | Link | Note |
|------|------|------|
| $x^2$ | [link](https://example.com) | ref[^t] |

| Inline code with spaces |
|---|
| `cargo build --release --locked` |

[^t]: A footnote from a table.
