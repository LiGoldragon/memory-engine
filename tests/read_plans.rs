//! The `Filter` read plan: a typed record predicate narrowing a source
//! plan's rows. Consumers (flow, message) read a whole table or a key range
//! and then keep the rows whose fields name what they look for; this is
//! that question asked of the engine. Expected rows are spelled out by
//! hand from the fixture, never derived through the tested path.

use std::sync::Arc;

use memory_engine::{
    Assertion, Engine, EngineOpen, EngineRecord, Error, FamilyName, KeyRange, QueryPlan,
    ReadOperator, RecordKey, RecordPredicate, SchemaHash, SchemaVersion, SinkError,
    SubscriptionEvent, SubscriptionSink, TableDescriptor, TableName, TableReference,
    VersionedStoreName, VersioningPolicy,
};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use tempfile::TempDir;

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Receipt {
    key: String,
    message: String,
}

impl Receipt {
    fn new(key: &str, message: &str) -> Self {
        Self {
            key: key.to_owned(),
            message: message.to_owned(),
        }
    }
}

impl EngineRecord for Receipt {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

/// The data-bearing selection a consumer would name: receipts of one message.
struct OfMessage {
    message: String,
}

impl OfMessage {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_owned(),
        }
    }
}

impl RecordPredicate<Receipt> for OfMessage {
    fn admits(&self, record: &Receipt) -> bool {
        record.message == self.message
    }
}

struct Store {
    _directory: TempDir,
    engine: Engine,
    receipts: TableReference<Receipt>,
}

impl Store {
    fn open(versioned: bool) -> Self {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut open =
            EngineOpen::new(directory.path().join("reads.memory"), SchemaVersion::new(1));
        if versioned {
            open = open.with_versioning(VersioningPolicy::new(VersionedStoreName::new("reads")));
        }
        let mut engine = Engine::open(open).expect("engine opens");
        let receipts = engine
            .register_table(TableDescriptor::new(
                TableName::new("receipts"),
                FamilyName::new("receipt"),
                SchemaHash::for_label("receipt-v1"),
            ))
            .expect("family registers");
        for (key, message) in [
            ("a1", "alpha"),
            ("a2", "beta"),
            ("a3", "alpha"),
            ("b1", "alpha"),
            ("b2", "beta"),
        ] {
            engine
                .assert(Assertion::new(receipts, Receipt::new(key, message)))
                .expect("receipt lands");
        }
        Self {
            _directory: directory,
            engine,
            receipts,
        }
    }

    fn keys(&self, plan: QueryPlan<Receipt>) -> Vec<String> {
        let mut keys: Vec<String> = self
            .engine
            .match_records(plan)
            .expect("filtered plan executes")
            .records()
            .iter()
            .map(|receipt| receipt.key.clone())
            .collect();
        keys.sort();
        keys
    }
}

#[test]
fn filter_keeps_only_admitted_rows_of_the_whole_table() {
    let store = Store::open(false);
    let plan = QueryPlan::filtered(store.receipts, OfMessage::new("alpha"));
    assert_eq!(plan.read_plan().operator(), ReadOperator::Filter);
    assert_eq!(store.keys(plan), ["a1", "a3", "b1"]);
}

#[test]
fn filter_narrows_a_key_range_source() {
    let store = Store::open(false);
    let plan = QueryPlan::key_range(
        store.receipts,
        KeyRange::between(RecordKey::new("a1"), RecordKey::new("a9")),
    )
    .filtered_by(OfMessage::new("alpha"));
    assert_eq!(store.keys(plan), ["a1", "a3"]);
}

#[test]
fn filter_over_a_single_key_admits_or_refuses_that_row() {
    let store = Store::open(false);
    let kept =
        QueryPlan::key(store.receipts, RecordKey::new("a2")).filtered_by(OfMessage::new("beta"));
    let refused =
        QueryPlan::key(store.receipts, RecordKey::new("a2")).filtered_by(OfMessage::new("alpha"));
    assert_eq!(store.keys(kept), ["a2"]);
    assert!(store.keys(refused).is_empty());
}

#[test]
fn validate_counts_the_filtered_rows() {
    let store = Store::open(false);
    let receipt = store
        .engine
        .validate(QueryPlan::filtered(store.receipts, OfMessage::new("beta")))
        .expect("filtered validate executes");
    assert_eq!(receipt.record_count(), 2);
}

#[test]
fn filter_inside_a_staging_session_sees_the_staged_overlay() {
    let store = Store::open(true);
    store.engine.begin_staged_group().expect("session engages");
    store
        .engine
        .assert(Assertion::new(store.receipts, Receipt::new("c1", "alpha")))
        .expect("staged assert buffers");
    let plan = QueryPlan::filtered(store.receipts, OfMessage::new("alpha"));
    assert_eq!(store.keys(plan), ["a1", "a3", "b1", "c1"]);
}

struct DiscardingSink;

impl SubscriptionSink<Receipt> for DiscardingSink {
    fn deliver(&self, _event: SubscriptionEvent<Receipt>) -> Result<(), SinkError> {
        Ok(())
    }
}

#[test]
fn subscribing_to_a_filtered_plan_is_refused_with_a_typed_error() {
    // Deltas are matched by key; a retraction carries no record for the
    // predicate to judge, so a filtered subscription would deliver rows
    // outside its own view. The engine refuses it instead.
    let store = Store::open(false);
    let error = store
        .engine
        .subscribe(
            QueryPlan::filtered(store.receipts, OfMessage::new("alpha")),
            Arc::new(DiscardingSink),
        )
        .expect_err("filtered subscription is refused");
    assert!(matches!(
        error,
        Error::UnsupportedReadPlan {
            operator: ReadOperator::Filter
        }
    ));
}
