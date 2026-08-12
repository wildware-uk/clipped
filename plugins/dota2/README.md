# Dota 2 highlight plugin

Reports kills, deaths, assists and match results from Dota 2, so that Clipped
can mark them on a recording's timeline and cut clips around them.

It is built on **Game State Integration** — Valve's own documented mechanism, in
which a configuration file in the game's directory names a local address and the
game posts a JSON description of what it can see to it. Nothing here reads the
game's memory, injects anything into it, or opens a handle to the game process
(AGENTS.md section 34). A user's Dota account is worth more than a highlight.

- The contract this is written against: [docs/plugin-api.md](../../docs/plugin-api.md)
- What it does with the network: [docs/privacy.md](../../docs/privacy.md)
- What maps to an event and what deliberately does not: `src/dota/mod.rs`

## What it reports

| Event | When |
| --- | --- |
| `match_started` | The horn — `map.game_state` reaching `DOTA_GAMERULES_STATE_GAME_IN_PROGRESS` |
| `match_ended` | `DOTA_GAMERULES_STATE_POST_GAME` |
| `kill`, `death`, `assist` | The player's own counters increasing, one event per step |
| `win`, `loss` | `map.win_team` naming a team, read against the player's own |
| `dota-2.kill_streak` | The killing spree Dota itself announces, at three |

**Roshan, towers, wards and first blood are not reported.** They are not in the
components this plugin subscribes to, and inferring them from what is would be a
guess presented as a fact. `src/dota/mod.rs` has the whole argument, including
what would change it.

Events are reported for **the player at this computer only**. A spectated game
says so once and reports nothing, because somebody else's kills are not the
player's.

## What it does to this machine

Two things, both declared in `plugin.json` and neither of them optional for an
integration that works:

1. **Listens on `127.0.0.1:3213`** for the game's payloads. Loopback, never a
   wildcard bind, and every payload has to carry a token this plugin generated —
   a socket on `127.0.0.1` is reachable by every process on the machine,
   including a web page.
2. **Writes one file**, `gamestate_integration_clipped.cfg`, into Dota's own
   `game\dota\cfg\gamestate_integration` directory. It is the only file this
   plugin ever writes there, it is written only when its contents would change,
   and nothing else in that directory is read, listed or touched. Delete it to
   stop the game reporting to Clipped.

Dota's installation directory is found through **Steam's own files on this
disk** — the library index and application manifest 570 — using
`clipped-game-detection`. The game process is never opened or inspected.

## The restart

Valve's client reads that configuration directory **when it starts**. So the
first time Clipped records a Dota session after this plugin is enabled, the file
is written and the plugin says so:

> Clipped has set Dota 2 up to report its events. Restart Dota 2 for it to take
> effect — this recording will not have any.

Every launch after that works without a word. This is a property of the
mechanism rather than of this plugin: `plugins/cs2` has it too, and answers it
differently — it installs the file from a command the user runs rather than on
attach ([docs/plugin-api.md](../../docs/plugin-api.md), "The Dota 2 plugin, and
what it shares with Counter-Strike 2").

## Building and installing it

```powershell
cargo build --release -p clipped-dota2-plugin
```

An installed plugin is a directory holding `plugin.json` and the executable it
names (`docs/plugin-api.md`, "What a plugin is"):

```text
plugins/dota-2/
    plugin.json
    clipped-dota2-plugin.exe
```

Producing that directory is a packaging step, and Clipped has no packaging yet
(M14). Until it does, copying the two files together by hand is what
`clipped_plugins::discover` needs to find.

## What is verified, and what is not

The tests in this crate run against **constructed** sample payloads, not
captured ones: nobody who has worked on this plugin has Dota 2 installed. See
[`fixtures/README.md`](fixtures/README.md) for exactly what that does and does
not prove, and
[issue #73](https://github.com/wildware-uk/clipped/issues/73) for the
verification against a real match, which has not been done.
