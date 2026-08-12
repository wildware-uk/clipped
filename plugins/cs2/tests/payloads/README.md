# Game State Integration payloads

Every file here is one payload as Counter-Strike 2 POSTs it, and
`competitive_half/` is a sequence of them in the order they would arrive.

**These are constructed, not captured.** Counter-Strike 2 is not installed on
the machine this plugin was written on, so they were written by hand against
Valve's documented Game State Integration payload — the blocks a
`gamestate_integration_*.cfg` subscribes to, and the field names inside them.
Saying so plainly matters: a test is only worth what its fixtures are, and a
constructed payload proves the derivation is right about the shape it was told
about, not that the shape is right. Replacing these with a capture from a real
match is the outstanding half of issue #70's first acceptance criterion, and
`plugins/cs2/README.md` says how to take one.

The Steam identifiers are `76561198000000001` (the local player) and
`76561198000000002` (a teammate), which are well-formed and belong to nobody.
The token is `fixture-token-not-a-secret`, which is what it looks like. Nothing
here is a credential.
