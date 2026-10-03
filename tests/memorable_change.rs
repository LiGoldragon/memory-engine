//! Witnesses for a Memorable record's typed change: a ready change commits
//! and answers a durable receipt under its stable change identity; a
//! refused change answers the record's own typed reason and leaves every
//! byte of the store unchanged; a storage failure is a third, structured
//! branch. Expected values are the literal notes the witnesses write.

mod note_family;

use note_family::{Note, NoteChange, NoteRefusal, NoteStore};
use sema_engine::{
    ChangeEffect, ChangeIdentifier, ChangeOutcome, MemoryChange, MemoryChanges, QueryPlan,
    RecordKey, StoragePhase, TableReference,
};

fn write(id: &str, title: &str, body: &str) -> NoteChange {
    NoteChange::Write {
        id: id.to_owned(),
        title: title.to_owned(),
        body: body.to_owned(),
    }
}

fn rename(title: &str) -> NoteChange {
    NoteChange::Rename {
        title: title.to_owned(),
    }
}

#[test]
fn a_ready_change_commits_and_answers_its_durable_receipt() {
    let store = NoteStore::new();
    let mut engine = store.open();
    let notes = engine
        .register_table(store.descriptor_v2())
        .expect("notes register");
    let key = RecordKey::new("alpha");

    let created = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        ChangeIdentifier::for_label("write-alpha"),
        write("alpha", "Groceries", "milk"),
    ));
    let ChangeOutcome::Changed(created) = created else {
        panic!("a ready write commits, got {created:?}");
    };
    assert_eq!(created.effect(), ChangeEffect::Created);
    assert_eq!(created.change(), ChangeIdentifier::for_label("write-alpha"));
    assert_eq!(created.key(), &key);

    let renamed = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        ChangeIdentifier::for_label("rename-alpha"),
        rename("Shopping"),
    ));
    let ChangeOutcome::Changed(renamed) = renamed else {
        panic!("a ready rename commits, got {renamed:?}");
    };
    assert_eq!(renamed.effect(), ChangeEffect::Replaced);
    assert!(renamed.commit_sequence() > created.commit_sequence());
    assert_eq!(
        engine.current_commit_sequence().expect("sequence reads"),
        renamed.commit_sequence(),
    );
    drop(engine);

    let mut reopened = store.open();
    let notes = reopened
        .register_table(store.descriptor_v2())
        .expect("notes re-register");
    assert_eq!(
        reopened
            .match_records(QueryPlan::key(notes, key))
            .expect("note reads")
            .records(),
        &[Note::new("alpha", "Shopping", "milk")],
    );
    assert_eq!(
        reopened
            .change_receipt(ChangeIdentifier::for_label("rename-alpha"))
            .expect("receipt reads"),
        Some(renamed),
        "the receipt is durable under its change identity",
    );
}

#[test]
fn a_repeated_change_identity_answers_its_recorded_receipt_without_replay() {
    let store = NoteStore::new();
    let mut engine = store.open();
    let notes = engine
        .register_table(store.descriptor_v2())
        .expect("notes register");
    let key = RecordKey::new("alpha");
    let identity = ChangeIdentifier::for_label("write-alpha");

    let ChangeOutcome::Changed(first) = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        identity,
        write("alpha", "Groceries", "milk"),
    )) else {
        panic!("the first write commits");
    };
    // The same identity resent — as after a lost reply — with a different
    // payload: the recorded receipt answers and nothing is replayed.
    let again = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        identity,
        write("alpha", "Other", "never"),
    ));
    let ChangeOutcome::Changed(again) = again else {
        panic!("a committed identity answers its receipt, got {again:?}");
    };
    assert_eq!(again, first);
    assert_eq!(
        engine.current_commit_sequence().expect("sequence reads"),
        first.commit_sequence(),
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::key(notes, key))
            .expect("note reads")
            .records(),
        &[Note::new("alpha", "Groceries", "milk")],
    );
}

#[test]
fn a_refused_change_answers_its_typed_reason_and_leaves_every_byte_unchanged() {
    let store = NoteStore::new();
    let mut engine = store.open();
    let notes = engine
        .register_table(store.descriptor_v2())
        .expect("notes register");
    let key = RecordKey::new("alpha");
    let ChangeOutcome::Changed(_) = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        ChangeIdentifier::for_label("write-alpha"),
        write("alpha", "Groceries", "milk"),
    )) else {
        panic!("the seed write commits");
    };
    let before = store.file_bytes();
    let sequence = engine.current_commit_sequence().expect("sequence reads");

    let refused = engine.change(MemoryChange::new(
        notes,
        key.clone(),
        ChangeIdentifier::for_label("blank-rename"),
        rename("   "),
    ));
    assert!(
        matches!(refused, ChangeOutcome::Refused(NoteRefusal::EmptyTitle)),
        "a blank title refuses with the record's reason, got {refused:?}",
    );
    let missing = engine.change(MemoryChange::new(
        notes,
        RecordKey::new("absent"),
        ChangeIdentifier::for_label("rename-absent"),
        rename("Anything"),
    ));
    assert!(matches!(
        missing,
        ChangeOutcome::Refused(NoteRefusal::Missing)
    ));

    assert_eq!(store.file_bytes(), before, "a refusal writes no byte");
    assert_eq!(
        engine.current_commit_sequence().expect("sequence reads"),
        sequence
    );
    assert_eq!(
        engine
            .change_receipt(ChangeIdentifier::for_label("blank-rename"))
            .expect("receipt reads"),
        None,
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::key(notes, key))
            .expect("note reads")
            .records(),
        &[Note::new("alpha", "Groceries", "milk")],
    );
}

#[test]
fn a_storage_failure_is_its_own_structured_branch() {
    let store = NoteStore::new();
    let engine = store.open();
    // Never registered: the engine cannot admit the change.
    let unregistered = TableReference::<Note>::new(note_family::NOTES);
    let failed = engine.change(MemoryChange::new(
        unregistered,
        RecordKey::new("alpha"),
        ChangeIdentifier::for_label("write-alpha"),
        write("alpha", "Groceries", "milk"),
    ));
    let ChangeOutcome::Failed(failure) = failed else {
        panic!("an unregistered table fails, got {failed:?}");
    };
    assert_eq!(failure.phase(), StoragePhase::Admission);
    assert!(failure.is_unchanged());
}
