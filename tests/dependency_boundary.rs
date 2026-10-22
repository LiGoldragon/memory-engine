use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use signal_sema::SemaOperation;

struct RepositoryFixture {
    root: PathBuf,
}

impl RepositoryFixture {
    fn current() -> Self {
        Self {
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    fn cargo_toml(&self) -> String {
        fs::read_to_string(self.root.join("Cargo.toml")).expect("Cargo.toml is readable")
    }

    fn has_file(&self, path: impl AsRef<Path>) -> bool {
        self.root.join(path).exists()
    }

    fn cargo_tree(&self, arguments: &[&str]) -> String {
        let output = Command::new(env!("CARGO"))
            .arg("tree")
            .args(arguments)
            .current_dir(&self.root)
            .output()
            .expect("cargo tree runs");
        assert!(
            output.status.success(),
            "cargo tree failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("cargo tree output is UTF-8")
    }
}

#[test]
fn memory_engine_ships_no_daemon_binary() {
    let fixture = RepositoryFixture::current();
    let cargo = fixture.cargo_toml();

    assert!(!cargo.contains("[[bin]]"));
    assert!(!fixture.has_file("src/main.rs"));
}

#[test]
fn memory_engine_carries_repo_local_direction_context() {
    let fixture = RepositoryFixture::current();

    // Durable direction lives in ARCHITECTURE.md; there is no per-repo
    // INTENT.md.
    assert!(!fixture.has_file("INTENT.md"));
    assert!(fixture.has_file("ARCHITECTURE.md"));
    assert!(fixture.has_file("AGENTS.md"));
    assert!(fixture.has_file("skills.md"));
}

#[test]
fn memory_engine_has_no_runtime_or_text_dependencies() {
    let fixture = RepositoryFixture::current();
    let cargo = fixture.cargo_toml();

    for forbidden in [
        "kameo",
        "tokio",
        "nota-codec",
        "signal-persona",
        "persona-router",
        "persona-mind",
    ] {
        assert!(
            !cargo.contains(forbidden),
            "Cargo.toml must not contain {forbidden}"
        );
    }
}

#[test]
fn signal_frame_is_absent_from_the_graph_a_consumer_inherits() {
    let fixture = RepositoryFixture::current();
    let tree = fixture.cargo_tree(&["--edges", "normal", "--all-features"]);

    assert!(
        !tree.contains("signal-frame"),
        "the retired envelope layer must not reach a memory-engine consumer:\n{tree}"
    );
}

#[test]
fn memory_engine_normal_dependency_tree_has_no_nota_next() {
    let fixture = RepositoryFixture::current();
    let tree = fixture.cargo_tree(&["--edges", "normal", "--no-default-features"]);

    assert!(
        !tree.contains("nota-next") && !tree.contains("nota_next"),
        "memory-engine normal dependency tree must not contain nota-next:\n{tree}"
    );
}

#[test]
fn memory_engine_normal_dependency_tree_is_one_exact_runtime_family() {
    let fixture = RepositoryFixture::current();
    let duplicates =
        fixture.cargo_tree(&["--edges", "normal", "--no-default-features", "--duplicates"]);
    assert!(
        duplicates.trim().is_empty(),
        "memory-engine normal graph contains duplicate package families:\n{duplicates}"
    );

    let tree = fixture.cargo_tree(&["--edges", "normal", "--no-default-features"]);
    for build_time_only in ["schema-rust", "sema-translator", "core-ethos", "core-nomos"] {
        assert!(
            !tree.contains(build_time_only),
            "{build_time_only} must remain source-generation machinery, not runtime substrate:\n{tree}"
        );
    }
}

#[test]
fn table_specification_fixture_is_owned_by_the_engine() {
    let fixture = RepositoryFixture::current();
    assert!(fixture.has_file("tests/fixtures/domain_snapshot_table.rs"));
    let cargo = fixture.cargo_toml();
    for generator in [
        "schema-rust",
        "core-ethos",
        "core-nomos",
        "rust-logos",
        "sema-translator",
        "structural-codec",
    ] {
        assert!(
            !cargo.contains(&format!("{generator} =")),
            "engine contract fixtures must not require {generator}"
        );
    }
}

#[test]
fn memory_engine_does_not_depend_on_raw_redb_directly() {
    let fixture = RepositoryFixture::current();
    let cargo = fixture.cargo_toml();

    assert!(!cargo.contains("redb ="));
}

#[test]
fn memory_engine_depends_on_storage_kernel_and_signal_sema_by_git() {
    let fixture = RepositoryFixture::current();
    let cargo = fixture.cargo_toml();

    assert!(cargo.contains("https://github.com/LiGoldragon/memory.git"));
    assert!(cargo.contains("https://github.com/LiGoldragon/signal-sema.git"));
    assert!(!cargo.contains("path = \"../memory\""));
    assert!(!cargo.contains("path = \"../signal-sema\""));
    assert!(
        !cargo.contains("signal-core"),
        "memory-engine must not depend on the retired signal-core crate"
    );
}

#[test]
fn memory_engine_owns_read_plan_vocabulary() {
    let fixture = RepositoryFixture::current();
    let source =
        fs::read_to_string(fixture.root.join("src/query.rs")).expect("query source is readable");
    let cargo = fixture.cargo_toml();

    for operator in ["Constrain", "Project", "Aggregate", "Infer", "Recurse"] {
        assert!(
            source.contains(operator),
            "{operator} must live in memory-engine read-plan vocabulary"
        );
    }
    assert!(cargo.contains("signal-sema"));
}

#[test]
fn signal_sema_operation_set_is_closed_at_six_operations_without_atomic() {
    let operations = [
        SemaOperation::Assert,
        SemaOperation::Mutate,
        SemaOperation::Retract,
        SemaOperation::Match,
        SemaOperation::Subscribe,
        SemaOperation::Validate,
    ];

    assert_eq!(operations.len(), 6);
    for operation in operations {
        assert!(SemaOperationWitness::accepts(operation));
    }
}

struct SemaOperationWitness;

impl SemaOperationWitness {
    fn accepts(operation: SemaOperation) -> bool {
        match operation {
            SemaOperation::Assert
            | SemaOperation::Mutate
            | SemaOperation::Retract
            | SemaOperation::Match
            | SemaOperation::Subscribe
            | SemaOperation::Validate => true,
        }
    }
}
