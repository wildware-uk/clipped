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
    &["clipped-windows", "clipped-events", "clipped-storage"],
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
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/tests/integration.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above this package")
        .to_path_buf()
}

/// Reads the workspace's own packages and their intra-workspace dependencies.
fn workspace_dependencies() -> HashMap<String, Vec<String>> {
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
                .filter_map(|dependency| dependency["name"].as_str())
                .filter(|dependency| member_names.iter().any(|member| member == dependency))
                .map(str::to_owned)
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
            let Some(dependency_layer) = layer_of(&dependency) else {
                continue;
            };
            if dependency_layer >= crate_layer {
                violations.push(format!(
                    "{crate_name} (layer {crate_layer}) depends on {dependency} \
                     (layer {dependency_layer})"
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
