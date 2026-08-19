# redink

A spellchecker for prose — a terminal UI for humans and a JSON CLI for agents.

Redink is built for long-form writing (novels, manuscripts). It pairs a vetted
system dictionary with a per-project working dictionary that lives in your
repository, so coinages, names, and foreign phrases travel with the text. It's
smart about the things prose actually trips on: possessives of coinages,
hyphenated compounds, short Latin phrases, and Markdown.

## Dictionary model

- **System dictionary:** the bundled [SCOWL](https://wordlist.aspell.net)
  `en_US` (size 60, version 2020.12.07), embedded in the binary so redink works
  with nothing installed. If a system `en_US.{aff,dic}` is present it is
  preferred (so `--lang en_GB` etc. work); see `--sysdict-dir`.
- **Working dictionary:** `.redink.dic` in your repo (found by searching from
  the current directory upward). All additions go here and **only** here — the
  system dictionary is never modified. Plain-text, one entry per line,
  git-friendly.

The working dictionary format:

```text
# bare word      -> case-insensitive (accepted in any case)
# =Word          -> case-sensitive   (accepted only in that exact casing)
# multi-word line -> phrase          (matched as bigrams against neighbours)
hobbit
=Gondor
per se
```

## Build

```sh
cargo build --release
# binary: target/release/redink
```

## Usage

```sh
redink src/chapter.*.md      # TUI over those files (interactive terminal)
redink                        # TUI over all .md/.txt in the cwd
redink check                  # non-interactive check (text output)
redink check --json ch01.md   # machine-readable, for scripts/agents
redink check --words          # just the unique misspelled words
redink dict list              # show the working dictionary
redink dict add hobbit Gondor # add case-insensitive words
redink dict add --sensitive Gondor   # add exact-case entry
redink dict add "per se"      # add a phrase (multi-word argument)
redink dict remove hobbit
redink fix ch01.md --at 1234 --word brwon --to brown   # replace at a byte offset
```

Global options: `--dict <PATH>`, `--lang <LANG>` (default `en_US`),
`--sysdict-dir <DIR>`, `--format <auto|md|text>` (default `auto`).

A bare `redink <files>` launches the TUI when stdout is a terminal, otherwise
runs a non-interactive check.

**Exit codes:** `0` clean · `1` misspellings found · `2` error.

### JSON schema (`check --json`)

An array of objects, one per occurrence:

```json
{
  "file": "ch01.md",
  "line": 42,
  "column": 13,
  "byte_offset": 1234,
  "word": "teh",
  "suggestions": ["the", "tech"],
  "compound": null
}
```

`byte_offset` is absolute and `word` is the exact bytes at that offset, so a
precise, unambiguous replacement is `redink fix <file> --at <byte_offset>
--word <word> --to <replacement>`. `compound` carries the whole hyphenated
token when the misspelling is one part of a compound, otherwise `null`.

## TUI keys

| key | action |
|---|---|
| `j` `k` `n` `N` | move between misspellings |
| `1`–`9` | replace with the Nth suggestion |
| `r` | type a replacement (`Enter`/`Esc`) |
| `i` | ignore this word for the session (case-insensitive, not persisted) |
| `a` | add the word, case-insensitive (any case) |
| `A` | add the word, exact case |
| `h` | add the whole hyphenated compound, case-insensitive |
| `H` | add the whole hyphenated compound, exact case |
| `s` | save edited files · `q` save + quit · `Q` discard + quit · `?` help |

The misspelling is shown in context (a character window centered on the word),
highlighted, with numbered suggestions.

## How it handles prose

- **Possessives of coinages.** Add the base form and the possessive comes free:
  adding `Bill` accepts `Bill's` too. Adding *via* the possessive
  (`Atrax's`) registers the stem (`Atrax`). Works for ASCII and Unicode (`'`)
  apostrophes.
- **Hyphenated compounds.** `Tzeya-Gan` is checked whole first; if nothing
  recognizes it, each part is checked, and only the bad part is flagged (so
  `recieve-bar` flags `recieve`, leaves `bar`). Add a compound with `h`/`H` or
  `dict add Tzeya-Gan` to accept it as a unit. `--` em-dashes split cleanly.
- **Phrases.** `per se`, `de facto`, `a priori`, `in vitro`, `ad hoc`, … are
  recognized as units (43 common Latin phrases bundled; add your own as
  multi-word lines). The fragment words are accepted *only* in the phrase, so a
  standalone `se` is still flagged.
- **Markdown.** Fenced/inline code, bare URLs, YAML frontmatter, and HTML
  comments/tags are skipped. Override per-file with `--format`.
- **Suggestions** shorter than 3 characters are dropped as noise.

## Credit & license

The bundled English dictionary is derived from [SCOWL](https://wordlist.sourceforge.net/)
© 2000–2026 Kevin Atkinson and others (permissive; see
`assets/dict/en_US-COPYRIGHT`), with the affix file from Geoff Kuenning's
ispell (BSD). Local fixes are kept as a build-time patch manifest
(`assets/dict/en_US.patches`); see "Local modifications" in that file.
