# Edge cases

Control characters are shown, never executed: &#27;[31m red? &#x9b;2J

A very long URL: https://example.com/a/very/long/path/that/keeps/going/and/going?with=query&and=more#fragment

Soft&shy;hyphen&shy;ated words and no&nbsp;break&nbsp;spaces.

Unclosed <b>bold that ends with the paragraph

Next paragraph is not bold. Unknown <span class="x">tags</span> keep text.

<div align="center">
Unclosed div at the end of an HTML block
</div>

- [ ] task
  ```
  code in a task
  ```

  More paragraph text.

>>>>> five levels of quote

* * *

| a |
|---|
| no closing pipe
| [^missing] |

[link with **bold** and `code`](https://example.com/x_(y))

Escapes: \*not emphasis\*, \# not a heading, \[not a link\](x).

Emoji: 🎉 ❤️ 👍🏽 🇺🇸 and combining: e&#769; a&#770;.

한국어 문장은
띄어쓰기를 합니다.
