use signal_sema::SemaOperation;

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
