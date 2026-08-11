# Local search

**Status: the language, its parser and its matcher exist in
[`crates/library/src/search`](../crates/library/src/search); nothing runs them
over a real library yet.** Building the rows they match against is
[issue #56](https://github.com/wildware-uk/clipped/issues/56), the database
under that is [issue #55](https://github.com/wildware-uk/clipped/issues/55), and
the screen with the search box in it is
[issue #60](https://github.com/wildware-uk/clipped/issues/60). What exists today
is the language itself and its meaning, which is deliberate: the syntax is the
part a user learns and the part that cannot be changed afterwards without
breaking what they have learned (AGENTS.md section 43), so it is designed,
written down and tested before anything is built on top of it.

SPEC.md section 30 is the brief: search locally by game, session, date, clip
title, event type, tags, favourite and duration, with `game:cs2 kill favourite`
as the worked example.

## The language

### Words and phrases

A bare word matches anything with that text in it — the title, the game, the
session, a tag or an event type:

```text
mirage
```

Two words mean both, in any order and in any of those places:

```text
mirage clutch
```

Quotes make one term out of several words, and match the phrase including its
spaces:

```text
"grand final"
```

A phrase is a substring, not a set of words: `"ace mirage"` does not match a
clip called *Ace on Mirage*, and `ace mirage` does.

### Fields

Put a field in front to look in one place instead of everywhere:

| Written | Matches |
| --- | --- |
| `game:cs2` | the game's name |
| `session:friday` | the session's name |
| `title:ace` | the title of the recording, clip or screenshot |
| `tag:clutch` | one of its tags (`tags:` too) |
| `event:kill` | one of its event types (`events:` too) |
| `favourite` | favourited things (`favorite` too) |
| `date:2026-08-11` | the day it belongs to |
| `duration:>90s` | how long it lasts |

Field names are matched without regard to case, so `Game:` and `GAME:` work.

A bare `favourite` is the filter, not a word to look for, because SPEC.md
section 30 writes the example that way. To search for the *word*, quote it:
`"favourite"`.

### Dates

Written year first, always: `date:2026-08-11`. One format only, because
`03-04-2026` is March in one country and April in another and a search box has
no way to ask which was meant.

| Written | Matches |
| --- | --- |
| `date:2026-08-11` | that day |
| `date:>2026-08-01` | after that day |
| `date:>=2026-08-01` | that day or after |
| `date:<2026-09-01` | before that day |
| `date:<=2026-08-31` | that day or before |

A range is two terms: `date:>=2026-08-01 date:<=2026-08-31`.

The day is the day on the user's own calendar. Converting a stored instant into
one is the indexer's job, not the query's, so this crate has no time zone in it
and no clock to depend on in a test.

### Durations

A number and a unit — `s`, `m` or `h` — and as many of those as you like:

```text
duration:>30s
duration:<5m
duration:>=1h30m
```

A bare number is refused rather than assumed to be seconds. Half a library's
lengths are naturally read as minutes and half as seconds, and guessing wrong
means quietly selecting the wrong recordings.

### Not, and, or

| Written | Means |
| --- | --- |
| `-tag:spoiler` | not that. `NOT tag:spoiler` says the same |
| `a b` | both. `a AND b` says the same |
| `a OR b` | either |
| `(a OR b) c` | brackets, when the precedence is not what you want |

**`NOT` binds tighter than `AND`, and `AND` binds tighter than `OR`.** So:

```text
inferno mirage OR clutch
```

means *(inferno and mirage) or clutch*, and

```text
inferno (mirage OR clutch)
```

is how to say the other thing.

`OR`, `AND` and `NOT` are recognised in capitals only. Somebody searching for a
clip called *not my finest hour* is typing words, not operators, and the
capitals are what tells the two apart.

### Quoting escapes all of it

Anything inside quotes is text and nothing else:

| Written | Searches for |
| --- | --- |
| `"favourite"` | the word, not the flag |
| `"game:cs2"` | the literal text `game:cs2` |
| `"OR"` | the word `OR` |
| `"-a"` | text starting with a hyphen |
| `title:">"` | a title containing `>` |

`\"` is a quote inside a phrase and `\\` is a backslash; a backslash before
anything else is just a backslash, because a user pasting a Windows path means
it literally.

Text that only looks like a field is left alone too: `12:30` and `21:04` are
searched for as written, because what precedes the colon has to be letters
before it is treated as a field name.

## Case and other alphabets

Every text comparison ignores case, in every alphabet: `ЗАМЕС` finds `замес`,
`GROẞE` finds `große`, and `Pokémon` finds `POKÉMON`.

Folding is case and nothing else. `pokemon` does **not** find `Pokémon` —
stripping diacritics is a bigger decision than it looks (it changes what the
Turkish dotless `ı` and the Scandinavian `å` mean to their own speakers) and it
is not being made by accident here.

Case folding changes the length of text, in bytes and in characters, which is
why nothing in the implementation compares lengths as a shortcut. That check is
exactly what broke non-ASCII matching in `clipped-game-detection`, and
`a_match_survives_folding_changing_the_length_of_the_text` is the test that
stops it coming back.

## When a query does not parse

A search that cannot be understood says so, at the character it went wrong,
with what was expected there. It never quietly returns nothing: a user cannot
tell an empty library from a mistyped field name, and guessing on their behalf
is how a search box loses their trust (AGENTS.md section 45).

| Typed | Said |
| --- | --- |
| `game:cs2 colour:red` | ``colour`` at position 10 is not something Clipped can search by. Use game:, session:, title:, tag:, event:, date:, duration: or favourite, or put the text in quotes to search for it as words |
| `game:` | ``game:`` at position 1 says what to search but not what to look for. Write the value after the colon, such as game:cs2 |
| `ace "clutch` | the quote opened at position 5 is never closed. Add a closing quote, or remove the opening one |
| `date:2026-13-01` | ``2026-13-01`` at position 6 is not a date on the calendar. Write a date as year-month-day, such as date:2026-08-11, date:>2026-08-01 or date:<=2026-08-31 |
| `duration:5x` | ``5x`` at position 10 is not a length of time. Write a number and a unit, such as duration:>30s, duration:<5m or duration:>=1h30m |
| `game:>cs2` | ``game:`` cannot be compared with ``>`` at position 6. Only date: and duration: are compared with < and >; for ``game:`` write the value on its own, or quote it to search for ``>`` as text |
| `favourite:maybe` | ``maybe`` at position 11 is neither yes nor no. Write favourite on its own for favourites, -favourite for everything else, or favourite:true and favourite:false to be explicit |
| `ace OR` | ``OR`` at position 5 needs something to search for on each side of it. Add the missing side, or remove the ``OR`` |
| `ace -` | ``-`` at position 5 has nothing after it to leave out. Write what to exclude, such as -favourite, or remove the ``-`` |
| `(ace OR clutch` | the bracket opened at position 1 is never closed. Add a closing bracket, or remove the opening one |
| `ace) clutch` | the closing bracket at position 4 has no opening bracket before it. Remove it, or add the bracket it should close |
| `ace ()` | the brackets at position 5 have nothing between them. Put part of the search inside them, or remove them |
| `ace ""` | the quotes at position 5 have nothing between them. Put the words to search for inside them, or remove them |

The positions in those messages count from one, the way a person counts
characters. `QueryError::position` gives the same place as a 0-based offset **in
characters, not bytes**, which is what a caller underlines the mistake with —
and what keeps the underline in the right place when the query has a Cyrillic
game name in it.

An empty query is not a mistake. It selects the whole library, so the library
screen needs no separate idea of "not searching".

## What it deliberately does not do

- **No wildcards.** Every text term is already a substring match, which is what
  `*ace*` would mean anyway.
- **No ranges in one term.** `date:2026-08-01..2026-08-31` is two comparisons
  written as one, and two comparisons already work.
- **No relevance ranking.** A row matches or it does not; the order results
  appear in is the library screen's decision.
- **No index.** This is the meaning of a query, not a strategy for running one
  over a million rows — see below.

Each of those is a thing that could be added later without changing what an
existing query means, which is why they are absent rather than half-built.

## How it is put together

```text
crates/library/src/search/
    mod.rs        the module documentation, the public surface, FromStr
    lexer.rs      text to tokens, each remembering where it came from
    parser.rs     tokens to a Query, and every message a bad query produces
    query.rs      the query model: Query, Expr, Term, TextField, Comparison
    matcher.rs    running a Query against a Row
    row.rs        Row: what a query is matched against
    date.rs       the calendar date a date: term compares against
    text.rs       case folding, and the one text comparison
```

The grammar the parser implements:

```text
query        := alternatives?
alternatives := conjunction ( "OR" conjunction )*
conjunction  := unary ( "AND"? unary )*
unary        := ( "-" | "NOT" ) unary | "(" alternatives ")" | term
term         := field ":" comparison? value | value
```

A `Row` is **not** a database row. It is the projection whatever holds the
library produces: the text a search can look in, the flag it can filter by, and
the two values it can compare. Nothing here knows what the schema will be, which
is what lets the language be finished and tested before the schema exists.

### Running a query against a database

A SQLite-backed executor consumes the same `Query` this module produces, and
walks the tree instead of calling `Query::matches`:

- `Expr::All`, `Expr::Any` and `Expr::Not` become `AND`, `OR` and `NOT` around
  bracketed fragments. Precedence is already resolved into the tree, so nothing
  downstream re-decides how `a b OR c` groups.
- `Term::Text` becomes `LIKE '%' || ? || '%'` against a **folded** column, with
  `FoldedText::folded` as the bound parameter. It must not use SQLite's
  `COLLATE NOCASE`, which folds ASCII only and would answer differently from
  this module for every non-ASCII game, tag and title. `search::fold` is public
  for exactly this: the indexer writes the folded column with the same function,
  so the two agree by construction rather than by review.
- `Term::Favourite` is a boolean column, and `Term::Date` and `Term::Duration`
  are comparisons against columns worth indexing.
- `TextField::Anywhere` is the one that shapes the schema: either an `OR` across
  every text column, or one denormalised folded column holding all of them.
  Either satisfies the definition here.

`Query::matches` stays the reference answer. Where a database executor disagrees
with it, the executor is wrong — the tests in `matcher.rs` are what "matching"
means.

## Measured cost

Matching is the part this crate is responsible for, so it is the part measured.
A fixture library of 100,000 rows — each with a game, a session, a title, a
date, a duration, and tags and events on some of them — is built in memory and
every row is matched against each query in turn. Every row is folded once when
it is built, so matching allocates nothing.

Measured on this project's development machine: Windows 11 Pro build 26200, AMD
Ryzen 9 9950X3D, single-threaded, with the rest of the machine busy building.
`crates/library/src/search/mod.rs`,
`a_large_library_is_searched_in_a_measured_time`.

| Query | Selected | Release build | Test (debug) build |
| --- | ---: | ---: | ---: |
| *(empty)* | 100,000 | 0 ms | 0.8 ms |
| `mirage` | 100,000 | 7.1 ms | 23.3 ms |
| `game:counter kill favourite` | 607 | 14.3 ms | 31.7 ms |
| `тан` | 20,000 | 16.5 ms | 39.9 ms |
| `game:"Elden Ring" duration:>5m -favourite` | 9,984 | 6.8 ms | 18.8 ms |
| `date:>=2026-08-20 (tag:clutch OR event:kill) -game:minecraft` | 10,474 | 5.2 ms | 9.4 ms |

One run of each build, on a machine with eight other agents' builds running:
consecutive runs of the release build varied by about 40% on the slower queries,
so read these as an order of magnitude rather than as a benchmark. The empty
query measures as zero in the release build because it is a query with nothing
to test, and the optimiser removes the loop.

So a full linear scan of a library far larger than a real one costs on the order
of ten milliseconds. That is worth knowing for two reasons: a library screen can
filter what it already holds in memory without waiting, and a database-backed
executor that turns out slower than this over 100,000 rows is slower than not
having an index at all.

The test asserts a five-second ceiling rather than these numbers. It is there to
catch a change that makes matching accidentally quadratic, not to measure a
machine that may be running eight other builds; the numbers above are what the
measurement is for, and the test prints them on every run.

What is **not** measured, because it does not exist yet: search over a library
on disk. That is issues #55 and #56, and it will need its own numbers.
