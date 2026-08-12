# Sample Game State Integration payloads

These are the payloads the parsing and diffing tests run against. **Read the
provenance before trusting one of them as evidence.**

## Provenance

**These are constructed, not captured.** They were written by hand against
Valve's documented Game State Integration shape, to exercise the states this
plugin acts on and the ones it deliberately does not. Nobody on the machine
this plugin was written on has Dota 2 installed, so no payload here came out of
a running client.

What that means for what they prove, stated plainly (AGENTS.md sections 27 and
54):

- They **do** prove that this plugin's parsing is lenient in the ways it claims
  to be, that its diffing produces the event stream `src/dota/mod.rs` documents,
  and that a payload it does not understand produces no events rather than
  wrong ones. Those are properties of this code, and a fixture is enough to
  hold them.
- They **do not** prove that Dota 2 posts payloads of this shape, that the
  field names are spelled this way in the build a user is running, or that a
  kill increments `player.kills` when a courier snipe does not. Only a real
  match can show that, and
  [issue #73](https://github.com/wildware-uk/clipped/issues/73) is where that
  evidence is recorded.

When somebody with Dota 2 installed runs the verification in #73, the payloads
they capture should **replace** these files rather than joining them, and this
section should say so. A captured payload and a constructed one that disagree
is a bug in the constructed one.

## What each file is

| File | What it is |
| --- | --- |
| `01-menu.json` | Dota is running and nothing else is true: no match, no hero |
| `02-hero-selection.json` | A match exists and is being drafted |
| `03-strategy-time.json` | Still before the horn |
| `04-match-in-progress.json` | The horn: the state this plugin reads as a match starting |
| `05-first-kill.json` | One kill |
| `06-death.json` | A death, and the killing spree counter reset with it |
| `07-double-kill.json` | Two kills in one posting interval, taking the streak to two |
| `08-assist-and-killing-spree.json` | A kill and an assist, and the streak reaching the three Dota calls a killing spree |
| `09-radiant-wins.json` | `map.win_team` names a team while the match is still in progress |
| `10-post-game.json` | The scoreboard |
| `11-next-match.json` | A different `matchid`, with counters that start again |
| `spectating.json` | A game being watched rather than played: `player` keyed by team and slot |
| `unrecognisable.json` | A payload with nothing this plugin can read in it |

`04` to `10` are one match, in order, and
`tests/payload_sequence.rs` replays them as one.
