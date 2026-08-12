# Counter-Strike 2 highlight plugin

Reports kills, deaths, assists, rounds and match results from Counter-Strike 2,
so that Clipped can mark them on a recording's timeline.

It uses **Game State Integration**, which is Valve's own documented mechanism:
you put a small configuration file in the game's `cfg` folder, and the game
posts a snapshot of its state to a port on your own computer while you play.
Nothing is injected into the game, nothing reads the game's memory, and nothing
touches anti-cheat. That is not a happy accident — AGENTS.md section 34 rules
out every technique that could look like a cheat, and Counter-Strike 2 is the
reference integration precisely because it does not need one.

- [What it writes, and where](#what-it-writes-and-where)
- [Setting it up](#setting-it-up)
- [Removing it](#removing-it)
- [The port, and what it accepts](#the-port-and-what-it-accepts)
- [What it reports](#what-it-reports)
- [What it deliberately does not report](#what-it-deliberately-does-not-report)
- [If it is not working](#if-it-is-not-working)
- [For contributors](#for-contributors)

## What it writes, and where

**One file**, and only when you ask for it:

```text
<Counter-Strike 2>\game\csgo\cfg\gamestate_integration_clipped.cfg
```

That is the whole of it. Installing the plugin does not write it, launching the
game does not write it, and Clipped never writes anywhere else in your game
directory. If the plugin is attached to a session and the file is not there, it
says so and stops — it does not quietly install one.

The file looks like this, and this is exactly what it contains:

```text
"Clipped Game State Integration v1"
{
    "uri"       "http://127.0.0.1:3212/"
    "timeout"   "5.0"
    "buffer"    "0.1"
    "throttle"  "0.1"
    "heartbeat" "10.0"
    "auth"
    {
        "token" "<32 random hexadecimal characters>"
    }
    "data"
    {
        "provider"           "1"
        "map"                "1"
        "round"              "1"
        "player_id"          "1"
        "player_state"       "1"
        "player_match_stats" "1"
    }
}
```

Six `data` lines and no more. Counter-Strike will send whatever is subscribed
to, and a subscription this plugin does not need is data it does not need to be
handling. There is no subscription to your weapons, your position, your bomb
timer, or anybody else's grenades.

The `token` is generated once, on this machine, when you install. It is not a
Steam credential and grants nothing; it exists so that the plugin can tell a
payload from your game apart from anything else on the machine that can reach a
loopback port. See [below](#the-port-and-what-it-accepts).

One more file is written, beside the plugin's own executable rather than in your
game:

```text
<plugin folder>\installed-at.json
```

It holds the path of the configuration file above and nothing else — the port
and the token are read back out of the configuration itself, so there is only
ever one copy of each. It exists because Clipped tells a plugin the game's
executable *name*, not where the game is installed, so a running plugin has no
other way to find the file it wrote.

## Setting it up

```text
clipped-cs2-plugin install "C:\Program Files (x86)\Steam\steamapps\common\Counter-Strike Global Offensive"
```

Steam shows you that folder under **Counter-Strike 2 → Manage → Browse local
files**. You can also pass the `cfg` folder itself.

It prints the full path of what it wrote. **Restart Counter-Strike 2**
afterwards: the game reads these files at start-up.

| | |
| --- | --- |
| `--port <n>` | Use a different loopback port. The default is 3212, which is the port `plugin.json` beside this executable declares. **Nothing checks that the two agree**: if you change the port here, edit the `endpoint` in that file to match, or the plugin will be listening on a port its own declaration does not name. |
| `--replace` | Rewrite a configuration Clipped installed earlier, with a fresh token. It will never replace a file another tool wrote. |

`clipped-cs2-plugin status` says whether it is installed and what it listens on.

**It will not stand on another tool's toes.** Counter-Strike loads *every*
`gamestate_integration_*.cfg` in that folder, which is how several tools coexist.
So a file of Clipped's name that Clipped did not write is left exactly as it is
and reported, and a neighbouring file that already posts to the port you asked
for is a refusal naming that file, not an overwrite — two integrations on one
port means one of them silently gets nothing.

## Removing it

```text
clipped-cs2-plugin uninstall
```

It removes the one file it wrote and the record beside the executable, and
nothing else. Deleting `gamestate_integration_clipped.cfg` by hand does the same
job: the game stops posting, and nothing in Counter-Strike is left worse for it.

Verifying the game files through Steam also removes it, which is worth knowing
because it looks like the plugin breaking. `status` will tell you.

## The port, and what it accepts

The plugin listens on `127.0.0.1:3212` — loopback, never `0.0.0.0`, so nothing
outside this computer can reach it. That is declared in `plugin.json` and
recorded in [docs/privacy.md](../../docs/privacy.md)'s register.

The declaration is meant to be a sentence you read before you enable the plugin.
The sentence exists and is tested — "Listens on 127.0.0.1:3212 (this machine
only) — receives Counter-Strike 2 game state" — but **there is no screen in
Clipped that shows it to you yet**
([issue #281](https://github.com/wildware-uk/clipped/issues/281)), and nothing
records which plugins you have enabled
([issue #282](https://github.com/wildware-uk/clipped/issues/282)). Today the
whole of the opt-in is that you run `install` yourself, having read this page.
Nor is the declaration enforced: a plugin is a separate process and opens its
own socket ([issue #280](https://github.com/wildware-uk/clipped/issues/280)).

Loopback is not the same as safe, and privacy.md is explicit about it: a port
bound to loopback is reachable by *every other process on this machine*,
including a page open in a browser. So every payload has to carry the token from
the configuration file, and one that does not is answered `403` and dropped
before anything about it is believed. A request that is not a `POST` with a
`Content-Length` this endpoint will read gets `400`, and one that starts and
never finishes is dropped when its ten seconds are up — payloads are read one
connection at a time, so a connection that lasts is the game not being heard.
Nothing is sent anywhere: the socket only ever receives.

## What it reports

Game State Integration sends **state**, not events. There is no "kill" message —
there is a match statistics block whose `kills` was 8 a moment ago and is 9 now.
Everything below is a *difference* between two payloads.

| Clipped event | Derived from |
| --- | --- |
| `match_started` | a map appearing, the map changing, or the same map leaving `gameover` — two matches in a row on one map are two matches |
| `match_ended`, `win`, `loss` | `map.phase` becoming `gameover`; the result from the two scores and which side you are on |
| `round_started` | `round.phase` becoming `live` |
| `round_ended` | `round.phase` becoming `over`, carrying the winning side |
| `kill`, `death`, `assist` | `player.match_stats` counting up |

A kill also carries `"headshot": true` or `false` **when that can be said** — one
kill in the step, and the round's headshot counter moving by exactly one
alongside it. Two kills between two payloads with one headshot between them says
nothing about which one it was, so neither event claims it.

Every event is placed in the **middle of the window between the two payloads it
was derived from**, and carries half that window as its precision. With the
throttle above, that is around fifty milliseconds during a live round. Clipped
uses precision to decide how much to pad a clip, so an honest number matters
more than a flattering one.

## What it deliberately does not report

- **The first payload of a session.** It is a baseline. Attaching to a game
  already three rounds into a match tells the plugin the score and nothing about
  how it got there, and a `match_started` at the moment it happened to look
  would be a mark where nothing happened.
- **A weapon.** The payload carries the weapon you are *holding when it
  arrives*, which after a kill is very often the next one you switched to.
  A plausible-looking guess is worse than an absent field.
- **A spectated teammate's kills.** After you die the camera follows somebody
  else and the payload follows the camera. Their kills are not yours, and this
  plugin checks the Steam identifier on every payload before it believes one.
- **A `match_ended` for a match it never saw end.** Leaving one match for
  another reports the new match starting and nothing for the old one.
- **A `win` or `loss` per round.** Those two are how the *match* went; a round
  carries its winning side in its payload instead.
- **Anything at all from a payload that arrived out of order.** Each post is its
  own connection, so they can overtake each other. One stamped earlier than the
  last one accepted is dropped whole rather than measured against.

## If it is not working

| What you see | What it usually is |
| --- | --- |
| "Counter-Strike 2 has no Game State Integration file for Clipped" | It was never installed, or Steam's file verification removed it. Run `install` again. |
| "Clipped cannot listen on 127.0.0.1:3212" | Something else has the port. Run `install --replace --port <other>` and restart the game. |
| Installed, and no events | The game was not restarted after `install`. Counter-Strike reads these files at start-up. |
| "…was replaced by something other than Clipped" | Another tool took the file name. Clipped has left it alone; move it aside if you want Clipped to use that name. |

The plugin writes its diagnostics to standard error, which Clipped captures.

## For contributors

```text
src/integration.rs   the .cfg: what is written, and what is never touched
src/listener.rs      the loopback socket and the token check
src/payload.rs       what Counter-Strike posts, read leniently
src/derive.rs        state snapshots into events — the substance
src/keyvalues.rs     enough of Valve's config format to read one back
src/location.rs      where install left the file
src/main.rs          the subcommands, and the plugin protocol loop
```

`tests/payloads/` holds the Game State Integration payloads the tests run
against, and its README is honest about where they came from: they are
**constructed against Valve's documented payload shape, not captured from a
running game**, because Counter-Strike 2 is not installed on the machine this
was written on. They prove the derivation matches the shape it was told about.
They cannot prove the shape is right.

**Capturing real ones** is the outstanding work, and it needs a machine with the
game on it. Install as above, run any program that echoes what it is POSTed on
that port — or the plugin itself, with its standard error captured — play a
competitive match, and keep one payload per interesting transition. What is
worth having is a warm-up, a round starting, a single kill, a multi-kill, a
headshot, a death, a round ending, a spectated teammate, and a game over.

Put them in a **new directory** under `tests/payloads/`, numbered in the order
they arrived: `tests/derivation.rs` plays any such directory through the tracker
without changes. What it will not do is tell you what should come out — every
test there asserts the exact events a named sequence produces, so a capture
needs a test saying what happened in the match it came from. `tests/payloads/`
has its own README with the detail.
