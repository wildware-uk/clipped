# Game State Integration payloads

Every file here is one payload as Counter-Strike 2 POSTs it. The directories —
`competitive_match/` and `match_end/` — are sequences of them, read in file-name
order, which is the order they would arrive in.

**These are constructed, not captured.** Counter-Strike 2 is not installed on
the machine this plugin was written on, so they were written by hand against
Valve's documented Game State Integration payload — the blocks a
`gamestate_integration_*.cfg` subscribes to, and the field names inside them.
Saying so plainly matters: a test is only worth what its fixtures are, and a
constructed payload proves the derivation is right about the shape it was told
about, not that the shape is right. Replacing these with a capture from a real
match is the outstanding half of issue #70's first acceptance criterion, and
`plugins/cs2/README.md` says how to take one.

## Adding a captured sequence

`tests/derivation.rs` reads a directory of payloads and plays it through the
tracker, so a capture goes in a **new directory** beside these — the reader
takes the directory's name, and every `.json` in it, in file-name order.

What it does not do is check itself. Each test in that file asserts the exact
list of events a named sequence produces, file by file, so a capture needs a
test of its own stating what happened in the match it came from. That is the
point rather than an inconvenience: a fixture whose expected output is whatever
the code currently prints proves nothing. Dropping real payloads on top of these
without rewriting the assertions would leave the tests red, which is the honest
outcome — the events in a real match are not the events in these.

The Steam identifiers are `76561198000000001` (the local player) and
`76561198000000002` (a teammate), which are well-formed and belong to nobody.
The token is `fixture-token-not-a-secret`, which is what it looks like. Nothing
here is a credential.
