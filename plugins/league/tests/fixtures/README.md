# Live Client Data API samples

Payloads for the tests in `tests/live_api_payloads.rs`, one file per shape the
plugin has to cope with.

## Where they came from, exactly

**They were constructed by hand from the published shape of the Live Client Data
API, and they were not captured from a match.** League of Legends is not
installed on the machine this plugin was written on, and inventing a capture
would be inventing evidence (AGENTS.md section 27). The field *names* here are
the API's; every name, champion, timestamp and score in them is made up, and
none of it is anybody's data.

What that buys, and what it does not:

- It proves the parser reads the documented shape, ignores what it does not
  know, and refuses what it cannot interpret. That is a real property, and
  `tests/live_api_payloads.rs` breaks if it stops holding.
- It does **not** prove the shape matches the client on any particular patch.
  Only a real match can say that, and
  [issue #72](https://github.com/wildware-uk/clipped/issues/72) records that as
  outstanding rather than done.

When somebody does run this against a real client, the way to close that gap is
to save the body of one `GET /liveclientdata/allgamedata` beside these files —
with the names replaced, because a real payload carries the Riot IDs of nine
other people (docs/privacy.md) — and point a test at it.

## The files

| File | What it is for |
| --- | --- |
| `match_started.json` | The first seconds of a match: `GameStart` and nothing else. |
| `kills_deaths_assists.json` | The same match later, with the events the player is in and several they are not. |
| `ended_in_a_win.json` | The same match again, finished. A superset of the two above, so the three read in order are three polls of one match. |
| `ended_in_a_loss.json` | A different match with no kills in it at all, lost. |
| `no_active_player.json` | Spectating: there is no active player to attribute a kill to. |
| `summoner_names_only.json` | A client that reports the player as a summoner name, with an event list that has started carrying Riot IDs — the mixed shape a patch produces, and the one case where a name is matched without its tag. |
| `later_patch.json` | Fields and event names this build has never heard of. |
| `not_in_a_game.json` | What the endpoint answers with when there is no match. |
