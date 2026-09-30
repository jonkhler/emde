# Regressions

## glamour 314: list continuation indent

- Lorem ipsum dolor sit amet, consectetur adipiscing elit. Nulla mattis dignissim leo et tempus. Cras sit amet nisi id leo eleifend iaculis nec in lectus.
    - Cras quis ornare mi, in condimentum tortor. Vivamus id convallis ligula. Morbi ac commodo lacus, in blandit augue.

## glamour 172: quote prefix after wrapping

> The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog.

> The quick brown fox jumps over the lazy dog. The quick brown fox jumps over
> the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox
> jumps over the lazy dog.

## glamour 149: wrapped hyperlinks

See https://github.com/ValveSoftware/steam-for-linux/issues/1234567890 for the details.

## glamour 84: two trailing spaces

First line with two trailing spaces  
second line after a hard break.

## glamour 454: backgrounds fill the line

```
short
```

## glamour 315: inline code in tables

| Command | Meaning |
|---------|---------|
| `git commit --amend` | rewrite the last commit |

## mdcat 43: footnote in a table, and OSC 8 ids

| Berry      | Number of Rs |
|------------|--------------|
| Strawberry | 2[^1]        |
| Raspberry  | 3            |

## mdcat 301: `<br>` in a table

| Port PoE | Connection | Power |
| --- | --- | --- |
| 8+ | PoE Splitter<br>G5 Left Rear<br>G5 Front Lawn | 3W PoE<br>4W PoE |

## mdcat 302: task lists with paragraphs

- [ ] foo
  - [ ] bar
  - [ ] baz

- [ ] test
  - [ ] test1
  - [ ] test2

[^1]: [According to many LLMs](https://techcrunch.com/2024/08/27/why-ai-cant-spell-strawberry/)
