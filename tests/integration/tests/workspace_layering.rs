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
    // `clipped-ipc` belongs here for the reason `clipped-events` does: it is
    // shared vocabulary. It is the protocol boundary between the recorder and
    // the desktop application (docs/ipc.md), so it has to be usable from both
    // ends, and it deliberately depends on no other crate in this workspace —
    // a protocol crate that reached into the recording engine could not be
    // linked by a client.
    &[
        "clipped-windows",
        "clipped-events",
        "clipped-storage",
        "clipped-logging",
        "clipped-ipc",
        "clipped-media-validation",
        "clipped-ffmpeg-runtime",
    ],
    // Subsystems built directly on a platform or persistence layer.
    &[
        "clipped-capture",
        "clipped-audio",
        "clipped-encoder",
        "clipped-library",
        "clipped-game-detection",
        "clipped-plugins",
    ],
    // Media writing, which consumes encoded output.
    &["clipped-muxer"],
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
    &["clipped-fullscreen-dx11"],
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

#[test]
fn the_desktop_application_links_no_crate_of_this_workspace() {
    // The other direction, and the one apps/desktop/README.md claims: the window
    // "talks to the recorder process over the IPC boundary rather than linking
    // the recording crates directly". Linking one would put capture or encoding
    // inside the window's process, which is the whole thing ADR 0002 separates.
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
                .filter(|dependency| members.contains(&dependency.name))
                .map(move |dependency| format!("{crate_name} names {}", dependency.name))
                .collect::<Vec<_>>()
        })
        .collect();
    violations.sort();

    assert!(
        violations.is_empty(),
        "the desktop application must reach the recorder over IPC rather than by \
         linking it (docs/architecture.md, apps/desktop/README.md): {violations:#?}"
    );
}
