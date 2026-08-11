## What changed

<!-- What this does and why. One or two sentences. -->

Closes #

## Verification

<!--
Real evidence, per AGENTS.md section 53: what you ran and what it showed.
Not "everything works".
-->

- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace` and `cargo test --workspace` all pass
- Behaviour verified by:

## Network access

<!--
Tick exactly one. "Network communication" is defined in docs/privacy.md,
and covers loopback as well as outbound, dependencies that open sockets on
our behalf, and shelling out to something that does.
-->

- [ ] This change introduces no network communication.
- [ ] This change introduces network communication:
  - Class: loopback / outbound
  - What is sent, to where, and when:
  - Documented in `docs/privacy.md`:
  - Off by default, opted in via:

## Third-party source

<!--
Tick exactly one. This covers source committed into the tree - generated FFI
bindings, a copied header, a transcribed constant - and NOT crates Cargo
resolves, which `cargo deny` already checks. See CONTRIBUTING.md, "Dependencies
and vendored source are two different things".
-->

- [ ] This change commits no third-party source.
- [ ] This change commits third-party source:
  - Licence:
  - Upstream project, and the tag or commit it came from:
  - Notices carried in the file itself:
  - Recorded in `THIRD-PARTY-NOTICES.md`:

## Documentation

- [ ] Docs updated, or not needed because:
