# Steam fixtures

These four files were written by Steam, not by us. They were copied off a
machine with two libraries — the default one under `C:\Program Files
(x86)\Steam` and a second at `B:\SteamLibrary` — on 12 August 2026, from a
client with three applications in the default library and eighty-five in the
other.

**That is the point of them.** A KeyValues fixture written by hand agrees with
the parser that reads it by construction: it has the tabs the parser expects,
the escaping the parser expects, and none of the keys nobody thought about.
These have Valve's tab layout, Valve's `\\`-escaped Windows paths, Valve's
mixture of `appid` and `LastUpdated` and `StateFlags` capitalisation, and the
nested `InstalledDepots`, `SharedDepots`, `UserConfig` and `MountedConfig`
tables that no fixture anybody wrote for a test would have bothered with.

| File | Came from | Why this one |
| --- | --- | --- |
| `libraryfolders.vdf` | `C:\Program Files (x86)\Steam\steamapps\` | Two libraries, the default one listed inside it as entry `0`, and an `apps` table under each |
| `appmanifest_730.acf` | `B:\SteamLibrary\steamapps\` | Counter-Strike 2 — in the **non-default** library, and its `installdir` is `Counter-Strike Global Offensive`, so name and directory differ |
| `appmanifest_620.acf` | `B:\SteamLibrary\steamapps\` | Portal 2 — a second application in the same library, with a different `StateFlags` |
| `appmanifest_228980.acf` | `C:\Program Files (x86)\Steam\steamapps\` | Steamworks Common Redistributables — in the **default** library, and not a game, which is a thing Steam manifests are |

## What was changed

Two values, in the `.acf` files only:

- `LastOwner`, which is the account's 64-bit Steam identifier, is now `0`.
- `LastPlayed`, which says when somebody played, is now `0`.

Both are values Steam itself writes as `0` — `appmanifest_228980.acf` had
`"LastPlayed" "0"` already — so the files are still exactly the shape Steam
writes. Nothing else was touched: not the tabs, not the ordering, not the
escaping, not the keys this crate ignores.

## How the tests use them

`tests/steam.rs` copies these into a temporary directory to build a Steam
installation with two libraries in it, substituting the two absolute library
paths inside `libraryfolders.vdf` for the temporary directories — that
substitution is the only edit any test makes, and it is the only one it can
make without the fixture naming drives that do not exist on a build agent.
