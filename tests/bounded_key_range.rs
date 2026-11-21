//! Bounded key-range reads preserve the existing inclusive key-range order
//! while stopping after the caller's requested number of visible rows.

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use sema_engine::{
    Assertion, CommitRequest, Engine, EngineOpen, EngineRecord, FamilyName, KeyRange,
    KeyRangeLimit, Mutation, QueryPlan, RecordIdentifier, RecordKey, Retraction, SchemaHash,
    SchemaVersion, TableDescriptor, TableName, TableReference, VersionedStoreName,
    VersioningPolicy,
};
use std::ops::{Bound, ControlFlow};
use tempfile::TempDir;

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Receipt {
    key: String,
    body: String,
}

impl Receipt {
    fn new(key: &str, body: &str) -> Self {
        Self {
            key: key.to_owned(),
            body: body.to_owned(),
        }
    }
}

impl EngineRecord for Receipt {
    fn record_key(&self) -> RecordKey {
        RecordKey::domain(self.key.clone())
    }
}

struct Store {
    _directory: TempDir,
    engine: Engine,
    receipts: TableReference<Receipt>,
}

impl Store {
    fn new(versioned: bool) -> Self {
        let directory = tempfile::tempdir().expect("temp directory is created");
        let mut open =
            EngineOpen::new(directory.path().join("bounded.sema"), SchemaVersion::new(1));
        if versioned {
            open = open.with_versioning(VersioningPolicy::new(VersionedStoreName::new("bounded")));
        }
        let mut engine = Engine::open(open).expect("engine opens");
        let receipts = engine
            .register_table(TableDescriptor::new(
                TableName::new("receipts"),
                FamilyName::new("receipt"),
                SchemaHash::for_label("receipt-v1"),
            ))
            .expect("receipt family registers");
        for key in ["a", "b", "c", "d", "e"] {
            engine
                .assert(Assertion::new(receipts, Receipt::new(key, "base")))
                .expect("receipt commits");
        }
        Self {
            _directory: directory,
            engine,
            receipts,
        }
    }

    fn bounded_keys(&self, range: KeyRange, count: usize) -> Vec<String> {
        self.engine
            .match_first_in_key_range(QueryPlan::first_in_key_range(
                self.receipts,
                range,
                KeyRangeLimit::new(count),
            ))
            .expect("bounded range read succeeds")
            .records()
            .iter()
            .map(|receipt| receipt.key.clone())
            .collect()
    }

    fn full_range_keys(&self, range: KeyRange) -> Vec<String> {
        self.engine
            .match_records(QueryPlan::key_range(self.receipts, range))
            .expect("full range read succeeds")
            .records()
            .iter()
            .map(|receipt| receipt.key.clone())
            .collect()
    }

    fn add_many(&self, count: usize) {
        let mut request = CommitRequest::new(self.receipts);
        for index in 0..count {
            request = request.assert(Receipt::new(&format!("key-{index:06}"), "scaled fixture"));
        }
        self.engine.commit(request).expect("scaled rows commit");
    }
}

#[test]
fn committed_reads_match_inclusive_range_prefix_at_each_limit() {
    let store = Store::new(false);
    let range = KeyRange::between(RecordKey::domain("b"), RecordKey::domain("d"));
    let full = store.full_range_keys(range.clone());
    assert_eq!(full, ["b", "c", "d"]);
    for limit in [0, 1, 2, 3, 8] {
        assert_eq!(
            store.bounded_keys(range.clone(), limit),
            full.iter().take(limit).cloned().collect::<Vec<_>>(),
            "the first {limit} rows match the existing inclusive range read"
        );
    }
}

#[test]
fn empty_and_non_domain_bounds_match_record_key_ordering() {
    let store = Store::new(false);
    assert!(
        store
            .bounded_keys(
                KeyRange::between(RecordKey::domain("d"), RecordKey::domain("b")),
                2
            )
            .is_empty()
    );
    assert!(
        store
            .bounded_keys(
                KeyRange::from(RecordKey::identifier(RecordIdentifier::new(1))),
                2
            )
            .is_empty()
    );
    assert_eq!(
        store.bounded_keys(
            KeyRange::through(RecordKey::identifier(RecordIdentifier::new(1))),
            8
        ),
        ["a", "b", "c", "d", "e"]
    );
    assert!(store.bounded_keys(KeyRange::new(None, None), 0).is_empty());
}

#[test]
fn staged_first_rows_merge_latest_replacements_insertions_and_tombstones_before_decode() {
    let store = Store::new(true);
    store.engine.begin_staged_group().expect("staging engages");
    store
        .engine
        .mutate(Mutation::new(
            store.receipts,
            Receipt::new("b", "temporary"),
        ))
        .expect("first replacement stages");
    store
        .engine
        .mutate(Mutation::new(store.receipts, Receipt::new("b", "latest")))
        .expect("latest replacement stages");
    store
        .engine
        .retract(Retraction::new(store.receipts, RecordKey::domain("c")))
        .expect("tombstone stages");
    store
        .engine
        .assert(Assertion::new(
            store.receipts,
            Receipt::new("bb", "inserted"),
        ))
        .expect("insertion stages");
    store
        .engine
        .mutate(Mutation::new(store.receipts, Receipt::new("d", "changed")))
        .expect("second replacement stages");

    let range = KeyRange::between(RecordKey::domain("b"), RecordKey::domain("d"));
    let expected = [
        Receipt::new("b", "latest"),
        Receipt::new("bb", "inserted"),
        Receipt::new("d", "changed"),
    ];
    for limit in [0, 1, 2, 3, 5] {
        let records = store
            .engine
            .match_first_in_key_range(QueryPlan::first_in_key_range(
                store.receipts,
                range.clone(),
                KeyRangeLimit::new(limit),
            ))
            .expect("staged bounded read succeeds");
        assert_eq!(records.records(), &expected[..limit.min(expected.len())]);
    }
}

#[test]
fn decoded_and_visited_prefix_counts_do_not_grow_with_the_table_tail() {
    let store = Store::new(false);
    let table = store.receipts;
    let observed = |engine: &Engine, table: TableReference<Receipt>| {
        let mut visited = 0;
        let mut decoded = 0;
        engine
            .storage_reader()
            .read(|transaction| {
                table.sema_table().visit_range::<String>(
                    transaction,
                    (Bound::<String>::Unbounded, Bound::<String>::Unbounded),
                    |entry| {
                        visited += 1;
                        let (_key, _record) = entry.decode()?;
                        decoded += 1;
                        Ok(if decoded == 4 {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        })
                    },
                )?;
                Ok(())
            })
            .expect("range observer read succeeds");
        (visited, decoded)
    };

    assert_eq!(observed(&store.engine, table), (4, 4));
    store.add_many(2_048);
    assert_eq!(observed(&store.engine, table), (4, 4));
    assert_eq!(
        store.bounded_keys(KeyRange::new(None, None), 4),
        ["a", "b", "c", "d"]
    );
}

#[test]
fn sparse_staging_history_preserves_the_full_range_prefix() {
    let store = Store::new(true);
    store.add_many(512);
    store.engine.begin_staged_group().expect("staging engages");
    for index in [50, 125, 400] {
        store
            .engine
            .mutate(Mutation::new(
                store.receipts,
                Receipt::new(&format!("key-{index:06}"), "updated"),
            ))
            .expect("sparse replacement stages");
    }
    store
        .engine
        .retract(Retraction::new(
            store.receipts,
            RecordKey::domain("key-000020"),
        ))
        .expect("sparse tombstone stages");
    store
        .engine
        .assert(Assertion::new(
            store.receipts,
            Receipt::new("key-000010x", "inserted"),
        ))
        .expect("sparse insertion stages");

    let range = KeyRange::between(
        RecordKey::domain("key-000000"),
        RecordKey::domain("key-999999"),
    );
    let complete = store.full_range_keys(range.clone());
    assert_eq!(complete.len(), 512);
    assert_eq!(
        store.bounded_keys(range, 12),
        complete.into_iter().take(12).collect::<Vec<_>>(),
        "the staged bounded reader matches the independent full-read prefix"
    );
}
