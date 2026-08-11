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
    // `clipped-ipc` belongs here for the same reason `clipped-events` does: it
    // is shared vocabulary. It is the protocol boundary between the recorder
    // and the desktop application (docs/ipc.md), so it must be usable from
    // both ends, and it deliberately depends on no other crate in this
    // workspace - a protocol crate that reached into the recording engine
    // could not be linked by a client.
    &[
        "clipped-windows",
        "clipped-events",
        "clipped-storage",
        "clipped-logging",
        "clipped-ipc",
        "clipped-media-validation",
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

/// Reads the workspace's own packages and their intra-workspace dependencies.
fn workspace_dependencies() -> HashMap<String, Vec<WorkspaceDependency>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata should run from the workspace root");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata emits valid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata contains a packages array");

    let member_names: Vec<String> = packages
        .iter()
        .map(|package| {
            package["name"]
                .as_str()
                .expect("package has a name")
                .to_owned()
        })
        .collect();

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
                    let name = dependency["name"].as_str()?;
                    member_names
                        .iter()
                        .any(|member| member == name)
                        .then(|| WorkspaceDependency {
                            name: name.to_owned(),
                            kind: dependency["kind"].as_str().map(str::to_owned),
                        })
                })
                .collect();
            (name, dependencies)
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

#[test]
fn no_crate_depends_on_the_desktop_application() {
    // The desktop application and the web packages are not Cargo packages at
    // all, which is what keeps them unreachable from the Rust dependency
    // graph. Assert that property directly, so that turning one of them into a
    // crate is a deliberate decision rather than an accident.
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
