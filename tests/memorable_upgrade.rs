//! Witnesses for a Memorable record's upgrade from its pinned predecessor
//! across one manifest edge (note v1 `text` to v2 `title` plus `body`,
//! split rule in `note_family`): a ready upgrade lands every row and the
//! format marker together; a refused conversion leaves the predecessor
//! bytes and marker readable; an interrupted commit reopens with one
//! format only. Expected rows are written out literally from the rule.

mod note_family;

use memory_engine::{
    Engine, Error, MemoryDefinition, MemoryUpgrades, MemoryVersion, QueryPlan, RecordKey,
    StoragePhase, UpgradeFault, UpgradeOutcome,
};
use note_family::{Note, NoteStore, NoteUpgradeRefusal, NoteV1};

fn seed(store: &NoteStore, rows: &[NoteV1]) {
    let mut engine = store.open();
    let notes = engine
        .register_table(store.descriptor_v1())
        .expect("v1 notes register");
    for row in rows {
        engine
            .assert_keyed(memory_engine::KeyedAssertion::new(
                notes,
                RecordKey::new(row.id.clone()),
                row.clone(),
            ))
            .expect("v1 note asserts");
    }
}

fn sorted(engine: &Engine, notes: memory_engine::TableReference<Note>) -> Vec<Note> {
    let mut rows = engine
        .match_records(QueryPlan::all(notes))
        .expect("v2 notes read")
        .records()
        .to_vec();
    rows.sort_by(|left, right| left.id.cmp(&right.id));
    rows
}

#[test]
fn a_ready_upgrade_splits_text_and_lands_rows_with_the_format_marker() {
    let store = NoteStore::new();
    seed(
        &store,
        &[
            NoteV1::new("alpha", "Groceries\nmilk\neggs"),
            NoteV1::new("beta", "  Plans  "),
        ],
    );

    let mut engine = store.open();
    let outcome = engine.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    let UpgradeOutcome::Upgraded(receipt) = outcome else {
        panic!("every v1 note converts, got {outcome:?}");
    };
    assert_eq!(receipt.rows(), 2);
    assert_eq!(
        receipt.edge().predecessor().version(),
        MemoryVersion::new(1)
    );
    assert_eq!(receipt.edge().current().version(), MemoryVersion::new(2));
    assert_eq!(
        receipt.edge().predecessor().schema_hash(),
        NoteV1::schema_hash()
    );
    assert_eq!(
        sorted(&engine, receipt.table()),
        vec![
            Note::new("alpha", "Groceries", "milk\neggs"),
            Note::new("beta", "Plans", ""),
        ],
    );
    drop(engine);

    let (marker, rows) = store.raw_v2();
    assert_eq!(marker, Note::schema_hash());
    assert_eq!(rows.len(), 2);
    // Reopened, the upgraded family registers as an ordinary existing one.
    let mut reopened = store.open();
    let again = reopened.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    assert!(matches!(again, UpgradeOutcome::AlreadyCurrent(_)));
    reopened
        .register_table(store.descriptor_v2())
        .expect("v2 registers as existing");
}

#[test]
fn a_refused_conversion_leaves_the_predecessor_bytes_and_format_readable() {
    let store = NoteStore::new();
    let seeded = [
        NoteV1::new("alpha", "Groceries\nmilk"),
        NoteV1::new("empty", ""),
    ];
    seed(&store, &seeded);

    let mut engine = store.open();
    let before = store.file_bytes();
    let outcome = engine.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    let UpgradeOutcome::Refused(refusal) = outcome else {
        panic!("an empty v1 text refuses, got {outcome:?}");
    };
    assert_eq!(refusal.key(), &RecordKey::new("empty"));
    assert_eq!(
        refusal.reason(),
        &NoteUpgradeRefusal::InvalidLegacyTitle {
            note: String::from("empty")
        },
    );
    // A predecessor still stored keeps plain registration of the new
    // definition closed until the upgrade lands.
    let pending = engine.register_table(store.descriptor_v2());
    assert!(matches!(pending, Err(Error::UpgradePending { .. })));
    assert_eq!(
        store.file_bytes(),
        before,
        "a refused upgrade writes no byte"
    );
    drop(engine);

    let (marker, rows) = store.raw_v1();
    assert_eq!(marker, NoteV1::schema_hash());
    assert_eq!(
        rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
        seeded.to_vec(),
    );
    let mut readable = store.open();
    let notes = readable
        .register_table(store.descriptor_v1())
        .expect("the v1 format still registers");
    assert_eq!(
        readable
            .match_records(QueryPlan::all(notes))
            .expect("v1 rows read")
            .records()
            .len(),
        2,
    );
}

#[test]
fn an_interrupted_upgrade_reopens_with_no_mixed_format() {
    let store = NoteStore::new();
    let seeded = [
        NoteV1::new("alpha", "Groceries\nmilk"),
        NoteV1::new("beta", "Plans"),
    ];
    seed(&store, &seeded);

    let mut engine = store.open();
    let sequence = engine.current_commit_sequence().expect("sequence reads");
    engine.interrupt_upgrade_at(UpgradeFault::AfterRowsWritten);
    let outcome = engine.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    let UpgradeOutcome::Failed(failure) = outcome else {
        panic!("the interrupted upgrade fails, got {outcome:?}");
    };
    assert_eq!(failure.phase(), StoragePhase::Commit);
    assert!(failure.is_unchanged());
    drop(engine);

    // Reopened: the marker names v1 and every row decodes as v1 — a v2
    // row would fail the validated v1 decode.
    let (marker, rows) = store.raw_v1();
    assert_eq!(marker, NoteV1::schema_hash());
    assert_eq!(
        rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
        seeded.to_vec(),
    );
    let mut reopened = store.open();
    assert_eq!(
        reopened.current_commit_sequence().expect("sequence reads"),
        sequence,
        "no log entry survived the interruption",
    );

    // The uninterrupted retry lands the whole upgrade.
    let retried = reopened.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    let UpgradeOutcome::Upgraded(receipt) = retried else {
        panic!("the retry converts, got {retried:?}");
    };
    assert_eq!(
        sorted(&reopened, receipt.table()),
        vec![
            Note::new("alpha", "Groceries", "milk"),
            Note::new("beta", "Plans", ""),
        ],
    );
    drop(reopened);
    let (marker, rows) = store.raw_v2();
    assert_eq!(marker, Note::schema_hash());
    assert_eq!(rows.len(), 2);
}

#[test]
fn a_store_holding_no_predecessor_needs_no_upgrade() {
    let store = NoteStore::new();
    let mut engine = store.open();
    let outcome = engine.upgrade_table::<NoteV1, Note>(store.descriptor_v2());
    assert!(
        matches!(outcome, UpgradeOutcome::AlreadyCurrent(_)),
        "a fresh store registers v2 directly, got {outcome:?}",
    );
    drop(engine);
    let (marker, rows) = store.raw_v2();
    assert_eq!(marker, Note::schema_hash());
    assert!(rows.is_empty());
}
