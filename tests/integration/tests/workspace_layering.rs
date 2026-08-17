//! Enforces the dependency direction documented in the repository README.
//!
//! The layering rules in AGENTS.md sections 4 and 5 are the kind of constraint
//! that erodes silently: one convenient `use` from a low-level crate into an
//! application crate is easy to review past, and hard to unpick a year later.
//! Reading the real dependency graph out of `cargo metadata` makes the rule
//! enforceable instead of aspirational.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// The layer each workspace crate belongs to, lowest first.
///
/// A crate may depend only on crates in a strictly lower layer. Keep this in
/// step with the "Dependency direction" section of the repository README; the
/// `layer_table_covers_every_workspace_member` test fails if a new crate is
/// added without being placed here.
const LAYERS: &[&[&str]] = &[
    // Platform primitives and shared vocabulary, which depend on nothing else.
    //
    // `clipped-media-validation` is here rather than with the test-only
    // packages further up for one reason: every crate that *produces* media
    // takes it as a dev-dependency to check its output (AGENTS.md section 22,
    // docs/testing.md), so it has to sit below all of them. It depends on
    // nothing in this workspace, which is what makes that placement sound.
    // Layering alone would let any crate name it under `[dependencies]`, so
    // `test_only_packages_are_never_linked_into_the_product` is what holds it
    // to a `[dev-dependencies]` entry.
    //
    // `clipped-ffmpeg-runtime` is here for a similar reason: it is named only
    // by `[build-dependencies]`, from the build scripts of the crates that link
    // FFmpeg, and it depends on nothing at all. It is not part of the product —
    // no binary links it — so it sits at the bottom where any build script can
    // reach it.
    //
    // `clipped-background` is here for the same reason `clipped-logging` is:
    // it depends on nothing else in this workspace, which is what a layer-0
    // crate is. `clipped-waveform` and the thumbnail module of
    // `clipped-library` — both layer 1 — both need the one background worker
    // it holds (issue #293), and it does not itself depend on
    // `clipped-logging`: it computes its own cache-key digest with the same
    // algorithm rather than reaching sideways for one, which would have put
    // it above layer 0 (`crates/background/src/lib.rs`, "Why this is
    // layer 0").
    //
    // `clipped-ipc` belongs here for the reason `clipped-events` does: it is
    // shared vocabulary. It is the protocol boundary between the recorder and
    // the desktop application (docs/ipc.md), so it has to be usable from both
    // ends, and it deliberately depends on no other crate in this workspace —
    // a protocol crate that reached into the recording engine could not be
    // linked by a client.
    //
    // `clipped-hotkeys` is here for the same reason and one more of its own: a
    // hotkey is a key combination and a handler somebody else supplies
    // (docs/hotkeys.md), so the dependency points *into* it — the process that
    // owns a recording session plugs a handler in, and a hotkey crate that
    // reached back into the session could be linked by neither end.
    //
    // `clipped-edit` is here for the reason `clipped-ipc` is: an edit document
    // is read by both ends of the application (docs/editing.md). The editor in
    // the desktop process shows it, the recorder process exports it, and
    // `clipped-storage` keeps it as text without understanding it — so a
    // document model that reached into the recording engine could not be linked
    // by the half of the system that only draws a timeline. It holds no
    // application logic and performs no I/O at all.
    &[
        "clipped-windows",
        "clipped-events",
        "clipped-storage",
        "clipped-logging",
        "clipped-ipc",
        "clipped-hotkeys",
        "clipped-edit",
        "clipped-media-validation",
        "clipped-ffmpeg-runtime",
        "clipped-background",
    ],
    // Subsystems built directly on a platform or persistence layer.
    //
    // `clipped-waveform` is here rather than beside `clipped-muxer` above,
    // even though both link FFmpeg, because it consumes nothing this workspace
    // produces: it reads a finished file from disk. Putting it at layer 1 is
    // what lets `clipped-library` depend on it when the timeline needs peaks
    // (issue #65) without the library having to sit above the muxer.
    &[
        "clipped-capture",
        "clipped-audio",
        "clipped-encoder",
        "clipped-library",
        "clipped-game-detection",
        "clipped-plugins",
        "clipped-waveform",
    ],
    // Consumers of encoded output. `clipped-muxer` writes it to a container.
    //
    // `clipped-league-plugin`, `clipped-cs2-plugin` and `clipped-dota2-plugin`
    // are game integrations
    // from `plugins/` (docs/plugin-api.md), and layering is not what governs
    // them: a plugin is a separate process the recorder starts rather than a
    // crate anything links, so both directions are asserted directly by
    // `PLUGINS` below — nothing may depend on one, and one may name only the
    // plugin contract and the event vocabulary. They are placed here rather
    // than up with the executables because the layer table has to cover every
    // member, and because the two rules that do govern them are stricter than
    // any layer, so the choice of layer decides nothing. Every plugin sits on
    // the same layer so that adding the next one is a line in two places
    // rather than a decision.
    &[
        "clipped-muxer",
        "clipped-league-plugin",
        "clipped-cs2-plugin",
        "clipped-dota2-plugin",
    ],
    // `clipped-replay` holds a rolling window of encoded packets in memory so
    // that a hotkey pressed after something interesting can still save it, and
    // then writes that clip out (docs/replay-buffer.md).
    //
    // It was a peer of `clipped-muxer` until issue #37 gave it the save. A clip
    // is a file, the muxer is what writes files, and the packets a buffer holds
    // are the packets a writer takes — so `save_clip` drives `MkvWriter` rather
    // than being a second Matroska implementation, or a loop every caller
    // repeats (AGENTS.md section 55). The dependency points one way: nothing in
    // `clipped-muxer` knows a replay buffer exists, which is what keeps a
    // recording and a clip written by the same code.
    //
    // `clipped-export` is beside it and not above it: the two have nothing to
    // do with each other — a replay is a window of packets held in memory, an
    // export is a document rendered from files on disk — and both are here for
    // the same reason, which is that they drive `clipped-muxer` to write a
    // file rather than containing a second Matroska implementation. An export
    // also *reads* containers, which nothing below layer 2 exposes, so it names
    // `rusty_ffmpeg` directly under the amendment issue #155 made to ADR 0004,
    // exactly as `clipped-waveform` does.
    &["clipped-replay", "clipped-export"],
    // Application logic that coordinates every subsystem above.
    &["clipped-session"],
    // Executables and test-only packages.
    &["clipped-recorder", "clipped-workspace-tests"],
    // The controlled test applications capture tests point at instead of an
    // installed game (AGENTS.md section 26, docs/testing.md). They are above
    // the executables rather than beside them for one reason: nothing in the
    // product may depend on a test application, and putting them in their own
    // layers is what makes that a rule the graph enforces rather than a
    // convention.
    &["clipped-video-pattern"],
    // `fullscreen-dx11` is `video-pattern` pointed at a whole display, and
    // shares its renderer and its pattern rather than owning a second copy, so
    // it has to sit above it.
    //
    // `process-tree-audio` is here for the same reason: its child process plays
    // a tone through `video-pattern`'s render stream, which is the workspace's
    // one copy of "open the default output endpoint and feed it" (AGENTS.md
    // section 55).
    &["clipped-fullscreen-dx11", "clipped-process-tree-audio"],
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/tests/integration.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above this package")
        .to_path_buf()
}

/// One package's dependency on another package of this workspace.
struct WorkspaceDependency {
    name: String,
    /// What `cargo metadata` calls the dependency's kind: `dev`, `build`, or
    /// nothing at all for a normal `[dependencies]` entry. The distinction is
    /// the whole of the "test-only" in "test-only package": a dev-dependency is
    /// never linked into the shipping recorder, and a normal one always is.
    kind: Option<String>,
}

impl WorkspaceDependency {
    fn is_dev(&self) -> bool {
        self.kind.as_deref() == Some("dev")
    }
}

/// Every dependency each member of the workspace at `manifest` declares,
/// whether or not the dependency is itself a member of that workspace.
///
/// `--no-deps` keeps this to the workspace's own manifests, which is what makes
/// it fast and offline; it does not filter what those manifests *name*. Keeping
/// the non-member entries is the difference between the two questions this file
/// asks. Layering is about members and drops the rest, but "does anything here
/// reach out of the workspace and into the desktop application?" can only be
/// answered by an entry that is not a member - and dropping those first is
/// exactly how that check would silently pass.
fn declared_dependencies(manifest: &Path) -> HashMap<String, Vec<WorkspaceDependency>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(manifest)
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "cargo metadata should run for {}: {error}",
                manifest.display()
            )
        });
    assert!(
        output.status.success(),
        "cargo metadata failed for {}: {}",
        manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata emits valid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata contains a packages array");

    packages
        .iter()
        .map(|package| {
            let name = package["name"]
                .as_str()
                .expect("package has a name")
                .to_owned();
            let dependencies = package["dependencies"]
                .as_array()
                .expect("package has a dependencies array")
                .iter()
                .filter_map(|dependency| {
                    Some(WorkspaceDependency {
                        name: dependency["name"].as_str()?.to_owned(),
                        kind: dependency["kind"].as_str().map(str::to_owned),
                    })
                })
                .collect();
            (name, dependencies)
        })
        .collect()
}

/// The root workspace's own packages and their intra-workspace dependencies.
fn workspace_dependencies() -> HashMap<String, Vec<WorkspaceDependency>> {
    let declared = declared_dependencies(&workspace_root().join("Cargo.toml"));
    let member_names: Vec<String> = declared.keys().cloned().collect();

    declared
        .into_iter()
        .map(|(name, dependencies)| {
            let internal = dependencies
                .into_iter()
                .filter(|dependency| member_names.contains(&dependency.name))
                .collect();
            (name, internal)
        })
        .collect()
}

fn layer_of(crate_name: &str) -> Option<usize> {
    LAYERS.iter().position(|layer| layer.contains(&crate_name))
}

#[test]
fn layer_table_covers_every_workspace_member() {
    let mut undeclared: Vec<String> = workspace_dependencies()
        .into_keys()
        .filter(|name| layer_of(name).is_none())
        .collect();
    undeclared.sort();

    assert!(
        undeclared.is_empty(),
        "these workspace crates are missing from the layer table in this test \
         and from the dependency direction documented in README.md: {undeclared:?}"
    );
}

#[test]
fn every_dependency_points_down_the_stack() {
    let mut violations = Vec::new();

    for (crate_name, dependencies) in workspace_dependencies() {
        let Some(crate_layer) = layer_of(&crate_name) else {
            // Reported by `layer_table_covers_every_workspace_member`.
            continue;
        };
        for dependency in dependencies {
            let Some(dependency_layer) = layer_of(&dependency.name) else {
                continue;
            };
            if dependency_layer >= crate_layer {
                violations.push(format!(
                    "{crate_name} (layer {crate_layer}) depends on {} \
                     (layer {dependency_layer})",
                    dependency.name
                ));
            }
        }
    }
    violations.sort();

    assert!(
        violations.is_empty(),
        "dependencies must point at a strictly lower layer: {violations:#?}"
    );
}

/// Test-only packages that must never be linked into a shipping binary, and
/// which may therefore only ever appear under `[dev-dependencies]`.
///
/// Being at a low layer is not enough on its own. `clipped-media-validation`
/// sits at layer 0 so that every crate which *produces* media can check its
/// output with it, which means the layering test above is satisfied by a normal
/// `[dependencies]` entry from anywhere in the stack — and a normal entry would
/// put a test harness, and `serde_json`, inside the recorder.
const TEST_ONLY_PACKAGES: &[&str] = &["clipped-media-validation"];

#[test]
fn test_only_packages_are_never_linked_into_the_product() {
    let mut violations = Vec::new();

    for (crate_name, dependencies) in workspace_dependencies() {
        for dependency in dependencies {
            if TEST_ONLY_PACKAGES.contains(&dependency.name.as_str()) && !dependency.is_dev() {
                violations.push(format!(
                    "{crate_name} names {} as a {} dependency",
                    dependency.name,
                    dependency.kind.as_deref().unwrap_or("normal")
                ));
            }
        }
    }
    violations.sort();

    assert!(
        violations.is_empty(),
        "these packages exist only to test other packages and must appear under \
         [dev-dependencies] so that nothing links them into a shipping binary \
         (README.md, docs/testing.md): {violations:#?}"
    );
}

/// The game integrations under `plugins/`.
///
/// A plugin is not a library and not a layer. It is an executable a user
/// installs beside a `plugin.json`, which the recorder starts as a separate
/// process and talks to over its standard input and output
/// (docs/plugin-api.md) — so the two rules that matter about it are not
/// "which layer", and the two tests below assert them instead of trusting a
/// comment. Adding a plugin to the workspace means adding it here.
const PLUGINS: &[&str] = &[
    "clipped-league-plugin",
    "clipped-cs2-plugin",
    "clipped-dota2-plugin",
];

/// The only crates of this workspace a plugin may name.
///
/// `clipped-plugins` is the contract — the wire, the manifest and the report
/// types — and `clipped-events` is the vocabulary a plugin exists to translate
/// its game into. Anything in the recording pipeline is a game integration
/// reaching into the recorder: a plugin that named `clipped-session` would be
/// running a game's protocol inside the recording engine, which is the
/// arrangement the process boundary exists to prevent (AGENTS.md sections 5
/// and 33), and one that named `clipped-capture` or `clipped-encoder` would be
/// worse.
///
/// Two more are permitted, and both are about *where things are on this
/// machine* rather than about recording. The list was written against
/// `plugins/league` ([#72](https://github.com/wildware-uk/clipped/issues/72)),
/// which needs neither — Riot answers on a fixed port and the plugin keeps no
/// state — and `plugins/dota2`
/// ([#73](https://github.com/wildware-uk/clipped/issues/73)) is the first
/// integration that does:
///
/// - `clipped-game-detection` reads Steam's own library index to find where a
///   game is installed. A Game State Integration plugin has to write a
///   configuration file inside the game's directory, so it has to find that
///   directory; the alternatives are a second Steam library parser per plugin
///   (AGENTS.md section 55) or opening a handle to the game process to ask,
///   which AGENTS.md section 34 rules out. It reports paths and knows nothing
///   about a recording.
/// - `clipped-logging` answers where Clipped's own directory is, which is where
///   a plugin keeps state that has to outlive its process — for Dota, the auth
///   token the game was configured with, because the game reads that file once
///   at start-up and would refuse a freshly generated one.
///
/// Deliberately still not "anything at a lower layer". Layering would let a
/// plugin name any of the crates below it, which is most of the workspace, and
/// the two additions above are named one at a time with a reason each rather
/// than by widening the rule into a layer.
///
/// **This list is a maintainer decision, argued on
/// [#73](https://github.com/wildware-uk/clipped/issues/73) with a
/// recommendation.** The narrower alternative is for `attach` to carry the
/// game's directory and a per-plugin state directory, which would make the
/// contract provide both facts and let this list go back to two entries
/// permanently — and would be what a sandboxed plugin
/// ([#280](https://github.com/wildware-uk/clipped/issues/280)) needs, since an
/// AppContainer is exactly what stops a plugin reading the registry to find
/// Steam. That is a change to `crates/plugins` and to every plugin, so it is an
/// issue of its own:
/// [#381](https://github.com/wildware-uk/clipped/issues/381), which is what
/// takes this list back to two entries.
const PLUGINS_MAY_NAME: &[&str] = &[
    "clipped-plugins",
    "clipped-events",
    "clipped-game-detection",
    "clipped-logging",
];

#[test]
fn nothing_in_the_workspace_depends_on_a_plugin() {
    // Layering cannot say this: a plugin has to sit at *some* layer, and every
    // crate above whichever one it is would be free to name it. The rule is not
    // about direction at all — it is that a plugin is reached by starting a
    // process, never by linking a crate, so that a game integration cannot end
    // up inside the recorder even by accident.
    let mut violations: Vec<String> = workspace_dependencies()
        .into_iter()
        .flat_map(|(crate_name, dependencies)| {
            dependencies
                .into_iter()
                .filter(|dependency| PLUGINS.contains(&dependency.name.as_str()))
                .map(move |dependency| {
                    format!(
                        "{crate_name} names {} as a {} dependency",
                        dependency.name,
                        dependency.kind.as_deref().unwrap_or("normal")
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    violations.sort();

    assert!(
        violations.is_empty(),
        "a plugin is a separate process the recorder starts, not a crate anything links \
         (docs/plugin-api.md): {violations:#?}"
    );
}

#[test]
fn a_plugin_names_only_the_plugin_contract_and_the_event_vocabulary() {
    let dependencies = workspace_dependencies();
    let mut violations = Vec::new();

    for plugin in PLUGINS {
        let named = dependencies
            .get(*plugin)
            .unwrap_or_else(|| panic!("{plugin} is a member of this workspace"));
        for dependency in named {
            if !PLUGINS_MAY_NAME.contains(&dependency.name.as_str()) {
                violations.push(format!(
                    "{plugin} names {} as a {} dependency",
                    dependency.name,
                    dependency.kind.as_deref().unwrap_or("normal")
                ));
            }
        }
    }
    violations.sort();

    assert!(
        violations.is_empty(),
        "a plugin translates one game into the shared vocabulary and may name nothing else \
         this workspace builds; only {PLUGINS_MAY_NAME:?} are permitted \
         (AGENTS.md section 33, docs/plugin-api.md): {violations:#?}"
    );
}

/// The desktop application's crate, which lives in its own Cargo workspace.
const DESKTOP_CRATE: &str = "clipped-desktop";

/// Where that workspace's single manifest is, relative to the repository root.
const DESKTOP_MANIFEST: &str = "apps/desktop/src-tauri/Cargo.toml";

#[test]
fn the_javascript_side_never_becomes_a_cargo_package() {
    // The interface and the packages it is drawn from are not Cargo packages at
    // all, which is what keeps them unreachable from the Rust dependency graph.
    // Assert that property directly, so that turning one of them into a crate is
    // a deliberate decision rather than an accident.
    let root = workspace_root();

    for directory in ["apps/desktop", "packages/ui", "packages/shared"] {
        let cargo_toml = root.join(directory).join("Cargo.toml");
        assert!(
            !cargo_toml.exists(),
            "{directory} must not become a Cargo package: the desktop application \
             talks to the recorder over IPC, not by linking to it"
        );
    }
}

#[test]
fn no_crate_depends_on_the_desktop_application() {
    // `apps/desktop/src-tauri` *is* a Cargo package - it is the Tauri binary -
    // and the only thing keeping it out of the layering test above is that it
    // belongs to a different workspace, which `layer_of` therefore knows
    // nothing about. That is not a guarantee: a path dependency on it from
    // anywhere under crates/ would resolve perfectly well and be dropped before
    // any layering assertion saw it. This is the assertion that catches it, and
    // it reads every dependency each member declares rather than only the ones
    // that are members themselves.
    let mut violations: Vec<String> = declared_dependencies(&workspace_root().join("Cargo.toml"))
        .into_iter()
        .flat_map(|(crate_name, dependencies)| {
            dependencies
                .into_iter()
                .filter(|dependency| dependency.name == DESKTOP_CRATE)
                .map(move |dependency| {
                    format!(
                        "{crate_name} names {DESKTOP_CRATE} as a {} dependency",
                        dependency.kind.as_deref().unwrap_or("normal")
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    violations.sort();

    assert!(
        violations.is_empty(),
        "nothing in this workspace may depend on the desktop application: it is a \
         client of the recorder over IPC, and closing or crashing a window must not \
         be able to interrupt a recording (docs/architecture.md, ADR 0002): {violations:#?}"
    );
}

/// The only crate of this workspace the desktop application may link.
///
/// `clipped-ipc` is the protocol boundary, and the layer table above already
/// says it "has to be usable from both ends". Something has to open the named
/// pipe, and a webview cannot: the Tauri host is the client, so it either uses
/// this crate or contains a second implementation of the handshake, the framing
/// and the compatibility policy — which is the duplication AGENTS.md section 55
/// exists to prevent, of the one surface where the two halves disagreeing is a
/// user-visible bug.
///
/// The rule the test still enforces is the one ADR 0002 actually cares about:
/// **no capture, no encoding, no muxing and no session state inside the
/// window's process.** `clipped-ipc` depends on no other crate in this
/// workspace, which is what makes an exception for it an exception for the
/// messages alone; the moment it grew a dependency on `clipped-session`, this
/// entry would be letting the recording engine in through the back door, and
/// `every_dependency_points_down_the_stack` is what would report that.
const DESKTOP_MAY_LINK: &[&str] = &["clipped-ipc"];

#[test]
fn the_desktop_application_links_nothing_of_this_workspace_but_the_protocol() {
    // The other direction, and the one apps/desktop/README.md claims: the window
    // "talks to the recorder process over the IPC boundary rather than linking
    // the recording crates directly". Linking a recording crate would put
    // capture or encoding inside the window's process, which is the whole thing
    // ADR 0002 separates.
    //
    // Its manifest is read through `cargo metadata` like any other, rather than
    // by searching the file for a string, so a dependency renamed with `package
    // = "..."` is still reported under the name it actually resolves to.
    let root = workspace_root();
    let members: Vec<String> = workspace_dependencies().into_keys().collect();

    let desktop = declared_dependencies(&root.join(DESKTOP_MANIFEST));
    let mut violations: Vec<String> = desktop
        .into_iter()
        .flat_map(|(crate_name, dependencies)| {
            dependencies
                .into_iter()
                .filter(|dependency| {
                    members.contains(&dependency.name)
                        && !DESKTOP_MAY_LINK.contains(&dependency.name.as_str())
                })
                .map(move |dependency| format!("{crate_name} names {}", dependency.name))
                .collect::<Vec<_>>()
        })
        .collect();
    violations.sort();

    assert!(
        violations.is_empty(),
        "the desktop application must reach the recorder over IPC rather than by \
         linking it; only {DESKTOP_MAY_LINK:?} may be named (docs/architecture.md, \
         apps/desktop/README.md, ADR 0006): {violations:#?}"
    );
}

#[test]
fn the_crate_the_desktop_application_may_link_drags_nothing_else_in() {
    // The exception above is only sound while `clipped-ipc` names no other crate
    // of this workspace. If it ever did, the allowance would be linking that
    // crate into the window as well, transitively and invisibly — so the
    // property is asserted here rather than left to the comment beside the
    // constant.
    let dependencies = workspace_dependencies();

    for allowed in DESKTOP_MAY_LINK {
        let named: Vec<String> = dependencies
            .get(*allowed)
            .unwrap_or_else(|| panic!("{allowed} is a member of this workspace"))
            .iter()
            .filter(|dependency| !dependency.is_dev())
            .map(|dependency| dependency.name.clone())
            .collect();

        assert!(
            named.is_empty(),
            "{allowed} may be linked by the desktop application, so it must depend on no \
             other crate of this workspace: {named:?}"
        );
    }
}

#[test]
fn every_plugin_in_the_workspace_is_named_by_the_constant_that_governs_plugins() {
    // `PLUGINS` is what the two rules above are applied to: nothing may depend
    // on a plugin, and a plugin may name only `PLUGINS_MAY_NAME`. A plugin
    // missing from it is not covered by either, and **nothing noticed** —
    // removing an entry while leaving the crate in the layer table left all
    // nine tests in this file green.
    //
    // That is not hypothetical: a rebase auto-merge did exactly that to #342
    // and it was caught by hand rather than by a test
    // ([issue #70](https://github.com/wildware-uk/clipped/issues/70)). The next
    // plugin arrives by the same route.
    //
    // The directory is the source of truth rather than the constant, because
    // the failure mode is a plugin that exists and is not listed.
    let plugins = workspace_root().join("plugins");
    let entries = std::fs::read_dir(&plugins)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", plugins.display()));

    let mut missing = Vec::new();
    for entry in entries.flatten() {
        let manifest = entry.path().join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("{} should be readable: {error}", manifest.display()));
        // The package name, from the `[package]` table's own `name` — not the
        // directory, which need not match it.
        let Some(name) = text
            .lines()
            .find_map(|line| line.trim().strip_prefix("name = "))
            .map(|value| value.trim().trim_matches('"').to_owned())
        else {
            panic!("{} declares no package name", manifest.display());
        };

        if !PLUGINS.contains(&name.as_str()) {
            missing.push(name);
        }
    }

    assert!(
        missing.is_empty(),
        "every member of plugins/ has to be in PLUGINS or the rules that govern plugins do \
         not reach it: {missing:?} are not listed"
    );
}
