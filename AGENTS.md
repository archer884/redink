# AGENTS.md

Guidance for AI agents. Two roles: **using redink** to spellcheck prose (the
common case), and **developing redink** itself. Read the section relevant to
your task.

## Using redink on a manuscript

Redink is the spellchecker for this prose. It is deliberately conservative and
respects the author's voice — your job is to fix genuine misspellings and
register coinages, **not** to "improve" the writing.

### Find errors

```sh
redink check --json path/to/file.md     # one JSON object per occurrence
redink check --json                     # all .md/.txt in cwd
```

Exit code: `0` clean, `1` misspellings present, `2` hard error. Each object:

```json
{
  "file": "ch01.md", "line": 42, "column": 13, "byte_offset": 1234,
  "word": "teh", "suggestions": ["the", "tech"], "compound": null
}
```

`byte_offset` is absolute in the file; `word` is the exact bytes there.
`compound` holds the whole hyphenated token when the flagged word is one part
of a compound (`"teh-bar"`), else `null`.

### Apply a fix

Prefer the offset-keyed command (it validates the word is still there before
replacing, so it cannot clobber the wrong text):

```sh
redink fix ch01.md --at 1234 --word teh --to the
```

Editing the file directly is also fine **if** you replace exactly the bytes at
`byte_offset..byte_offset + len(word)`. Never reorder or rewrap surrounding
text. Only change the single misspelled token.

### Register coinages, names, and phrases (do this instead of "fixing" them)

Speculative fiction is full of proper nouns and invented terms. **Do not
"correct" a coinage to a real word.** Add it to the working dictionary
(`.redink.dic`, found by searching upward from the cwd). Three equivalent ways:

```sh
redink dict add hobbit              # case-insensitive: any casing accepted
redink dict add --sensitive Gondor  # exact case only ("Gondor", not "gondor")
redink dict add "per se"            # a multi-word argument becomes a phrase
```

…or edit `.redink.dic` directly (it is plain text, sorted, git-friendly):

```text
hobbit        # case-insensitive
=Gondor       # exact case
per se        # phrase (matched against neighbouring words)
```

You only ever edit the **working** dictionary. The system dictionary (vendored
SCOWL) is read-only — never modify `assets/dict/`.

### Things redink already handles — don't redo them

- **Possessives are automatic.** Add the *base* form only: `Bill` covers
  `Bill's`. Adding via the possessive (`redink dict add "Atrax's"`) registers
  the stem (`Atrax`). Don't add both `Bill` and `Bill's`.
- **Hyphenated compounds.** A compound is checked whole, then per part. To
  accept a compound coinage as a unit, add the *whole* thing:
  `redink dict add Tzeya-Gan` (clears both `Tzeya` and `Gan`).
- **Phrases.** `per se`, `de facto`, etc. are recognized in context — fragment
  words like `se` are accepted only inside the phrase. Add recurring foreign
  phrases as multi-word lines.
- **All-caps alphanumeric tokens are skipped.** Acronyms, model numbers, and
  Roman numerals (`NASA`, `M16`, `XVII`, including possessives like `NASA's`)
  are never flagged — don't add them to the working dict.
- **Markdown is parsed.** Code blocks, inline code, URLs, YAML frontmatter,
  and HTML comments are skipped automatically — do not "fix" anything inside
  them.

### Hard rules

- Fix only real misspellings. Leave dialect, archaisms, and deliberate word
  choice alone. When in doubt, flag it for the author rather than guess.
- Never alter proper nouns; add them to the working dict.
- Never reflow, reformat, or rewrap prose. A fix touches exactly one token.
- Don't introduce changes inside code, frontmatter, URLs, or comments.
- Suggestions under 3 characters are already filtered out as noise.

## Developing redink

Rust 2024 edition. Modules: `main`/`cli` (dispatch), `engine` (dictionary
layers + checking/suggesting), `sysdict` (locate/embed system dict), `dict`
(working dictionary + possessive canonicalization + phrase bigrams), `token`
(word tokenizer), `format` (Markdown skip ranges), `check` (drive files →
`Misspelling`), `report` (text/json/words output), `tui` (ratatui app).

Before finishing any change, run:

```sh
cargo test
cargo clippy -- -D warnings
```

Both must pass clean. The vendored dictionary is SCOWL `en_US` 2020.12.07 with
one local patch (`else` → `else/M`, so `else's` is recognized); a regression
test (`vendored_dict_accepts_else_possessive`) guards it — re-apply the patch
if you ever re-vendor. See `assets/dict/en_US-COPYRIGHT`.
