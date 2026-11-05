use std::path::PathBuf;
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
