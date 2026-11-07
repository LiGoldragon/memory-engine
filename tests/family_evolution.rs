//! Witnesses for family evolution through the typed upgrade: a table
//! descriptor names its predecessor definitions, and upgrading a store
//! whose catalog names a declared predecessor migrates the rows through
//! the record's authored `UpgradeFrom` — atomically with the catalog
//! rewrite, logged as row history, fail-closed for any undeclared
//! identity.

use std::convert::Infallible;
use std::path::PathBuf;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use sema_engine::{
    Assertion, Decision, Engine, EngineOpen, EngineRecord, Error, FamilyName, MemoryDefinition,
    MemoryUpgrades, MemoryVersion, QueryPlan, RecordKey, SchemaHash, SchemaVersion, StoragePhase,
    TableDescriptor, TableName, TableReference, UpgradeFrom, UpgradeOutcome,
    VersionedHistoryAcknowledgement, VersionedHistoryRetention, VersionedPayload,
    VersionedStoreName, VersioningPolicy,
};
use signal_sema::SemaOperation;
use tempfile::TempDir;

const THOUGHTS: TableName = TableName::new("thoughts");

/// The retired first-generation shape: no mood field.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct ThoughtV1 {
    key: String,
    body: String,
}

impl ThoughtV1 {
    fn new(key: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            body: body.into(),
        }
    }
}

impl MemoryDefinition for ThoughtV1 {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("thought-v1")
    }
}

impl EngineRecord for ThoughtV1 {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

/// The current shape: a mood field appended.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Thought {
    key: String,
    body: String,
    mood: String,
}

impl Thought {
    fn new(key: impl Into<String>, body: impl Into<String>, mood: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            body: body.into(),
            mood: mood.into(),
        }
    }
}

impl EngineRecord for Thought {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

impl MemoryDefinition for Thought {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(2)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("thought-v2")
    }
}

impl UpgradeFrom<ThoughtV1> for Thought {
    type Refusal = Infallible;

    fn upgrade_from(predecessor: ThoughtV1) -> Decision<Self, Infallible> {
        Decision::Ready(Thought {
            key: predecessor.key,
            body: predecessor.body,
            mood: String::from("carried"),
        })
    }
}

/// A conversion that would reinterpret v1 bytes as an incompatible shape.
impl UpgradeFrom<CountedThought> for Thought {
    type Refusal = Infallible;

    fn upgrade_from(counted: CountedThought) -> Decision<Self, Infallible> {
        Decision::Ready(Thought::new(counted.key, "reinterpreted", "never"))
    }
}

/// A third generation, byte-compatible with `Thought`, reached directly
/// from either earlier generation through its own declared edge.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct ThoughtV3 {
    key: String,
    body: String,
    mood: String,
}

impl MemoryDefinition for ThoughtV3 {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(3)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("thought-v3")
    }
}

impl UpgradeFrom<ThoughtV1> for ThoughtV3 {
    type Refusal = Infallible;

    fn upgrade_from(predecessor: ThoughtV1) -> Decision<Self, Infallible> {
        Decision::Ready(ThoughtV3 {
            key: predecessor.key,
            body: predecessor.body,
            mood: String::from("carried"),
        })
    }
}

impl UpgradeFrom<Thought> for ThoughtV3 {
    type Refusal = Infallible;

    fn upgrade_from(predecessor: Thought) -> Decision<Self, Infallible> {
        Decision::Ready(ThoughtV3 {
            key: predecessor.key,
            body: predecessor.body,
            mood: predecessor.mood,
        })
    }
}

/// A shape whose field layout is incompatible with `ThoughtV1` bytes,
/// for the fail-closed decode witness.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct CountedThought {
    key: String,
    count: u64,
    weight: u64,
    depth: u64,
}

impl EngineRecord for CountedThought {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

impl MemoryDefinition for CountedThought {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("thought-v1")
    }
}

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temp dir is created"),
        }
    }

    fn database_path(&self, name: &str) -> PathBuf {
        self.directory.path().join(format!("{name}.sema"))
    }

    fn open_plain(&self, name: &str) -> Engine {
        Engine::open(EngineOpen::new(
            self.database_path(name),
            SchemaVersion::new(1),
        ))
        .expect("engine opens")
    }

    fn open_versioned(&self, name: &str) -> Engine {
        Engine::open(
            EngineOpen::new(self.database_path(name), SchemaVersion::new(1))
                .with_versioning(VersioningPolicy::new(VersionedStoreName::new(name))),
        )
        .expect("versioned engine opens")
    }

    fn family(&self) -> FamilyName {
        FamilyName::new("thought")
    }

    fn hash_v1(&self) -> SchemaHash {
        SchemaHash::for_label("thought-v1")
    }

    fn hash_v2(&self) -> SchemaHash {
        SchemaHash::for_label("thought-v2")
    }

    fn descriptor_v1(&self) -> TableDescriptor<ThoughtV1> {
        TableDescriptor::new(THOUGHTS, self.family(), self.hash_v1())
    }

    fn descriptor_v2_with_prior(&self) -> TableDescriptor<Thought> {
        TableDescriptor::new(THOUGHTS, self.family(), self.hash_v2())
            .with_predecessor::<ThoughtV1>()
    }

    /// Upgrade the v1 family to v2 through its declared edge.
    fn upgrade(&self, engine: &mut Engine) -> TableReference<Thought> {
        match engine.upgrade_table::<ThoughtV1, Thought>(self.descriptor_v2_with_prior()) {
            UpgradeOutcome::Upgraded(receipt) => receipt.table(),
            UpgradeOutcome::AlreadyCurrent(table) => table,
            other => panic!("the v1 family upgrades, got {other:?}"),
        }
    }

    fn seed_v1(&self, engine: &mut Engine, rows: &[ThoughtV1]) {
        let table = engine
            .register_table(self.descriptor_v1())
            .expect("v1 family registers");
        for row in rows {
            engine
                .assert(Assertion::new(table, row.clone()))
                .expect("v1 row asserts");
        }
    }
}

#[test]
fn evolution_carries_rows_across_a_family_identity_bump() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("carried");
    fixture.seed_v1(
        &mut engine,
        &[
            ThoughtV1::new("alpha", "first"),
            ThoughtV1::new("beta", "second"),
        ],
    );
    drop(engine);

    let mut evolved = fixture.open_plain("carried");
    let table = fixture.upgrade(&mut evolved);
    let mut rows = evolved
        .match_records(QueryPlan::all(table))
        .expect("current rows read")
        .records()
        .to_vec();
    rows.sort_by(|left, right| left.key.cmp(&right.key));
    assert_eq!(
        rows,
        vec![
            Thought::new("alpha", "first", "carried"),
            Thought::new("beta", "second", "carried"),
        ],
    );
}

#[test]
fn evolved_store_reopens_as_existing_registration() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("reopens");
    fixture.seed_v1(&mut engine, &[ThoughtV1::new("alpha", "first")]);
    drop(engine);

    let mut evolved = fixture.open_plain("reopens");
    fixture.upgrade(&mut evolved);
    drop(evolved);

    // The second open under the evolved identity is an ordinary
    // Existing registration — no steps consulted, rows intact.
    let mut reopened = fixture.open_plain("reopens");
    let table = reopened
        .register_table(TableDescriptor::<Thought>::new(
            THOUGHTS,
            fixture.family(),
            fixture.hash_v2(),
        ))
        .expect("evolved identity re-registers as existing");
    let rows = reopened
        .match_records(QueryPlan::all(table))
        .expect("rows read");
    assert_eq!(rows.records().len(), 1);
}

#[test]
fn undeclared_stored_identity_keeps_failing_closed() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("closed");
    fixture.seed_v1(&mut engine, &[ThoughtV1::new("alpha", "first")]);
    drop(engine);

    // v2 descriptor without any declared prior: the original typed
    // mismatch surfaces, and the store is untouched.
    let mut plain = fixture.open_plain("closed");
    let outcome = plain.register_table(TableDescriptor::<Thought>::new(
        THOUGHTS,
        fixture.family(),
        fixture.hash_v2(),
    ));
    assert!(matches!(outcome, Err(Error::FamilyIdentityMismatch { .. })));
    drop(plain);

    let mut untouched = fixture.open_plain("closed");
    let table = untouched
        .register_table(fixture.descriptor_v1())
        .expect("prior identity still registers after the refused open");
    let rows = untouched
        .match_records(QueryPlan::all(table))
        .expect("prior rows read");
    assert_eq!(rows.records().len(), 1);
}

#[test]
fn another_family_at_the_same_table_is_not_evolvable() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("foreign");
    fixture.seed_v1(&mut engine, &[ThoughtV1::new("alpha", "first")]);
    drop(engine);

    // Same table coordinate, different family name, matching prior
    // hash: a reused coordinate is a genuine incompatibility.
    let mut foreign = fixture.open_plain("foreign");
    let outcome = foreign.upgrade_table::<ThoughtV1, Thought>(
        TableDescriptor::<Thought>::new(THOUGHTS, FamilyName::new("memo"), fixture.hash_v2())
            .with_predecessor::<ThoughtV1>(),
    );
    let UpgradeOutcome::Failed(failure) = outcome else {
        panic!("a foreign family does not upgrade, got {outcome:?}");
    };
    assert_eq!(failure.phase(), StoragePhase::Admission);
    assert!(matches!(
        failure.cause(),
        Error::FamilyIdentityMismatch { .. }
    ));
}

#[test]
fn empty_family_evolves_as_catalog_only_rewrite() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("empty");
    engine
        .register_table(fixture.descriptor_v1())
        .expect("v1 family registers");
    drop(engine);

    let mut evolved = fixture.open_plain("empty");
    let before = evolved.commit_log().expect("commit log reads").len();
    let outcome = evolved.upgrade_table::<ThoughtV1, Thought>(fixture.descriptor_v2_with_prior());
    let UpgradeOutcome::Upgraded(receipt) = outcome else {
        panic!("the empty family upgrades, got {outcome:?}");
    };
    assert_eq!(receipt.rows(), 0);
    assert_eq!(receipt.commit_sequence(), None);
    let table = receipt.table();
    let after = evolved.commit_log().expect("commit log reads").len();
    assert_eq!(before, after, "an empty evolution logs no commit entry");
    let rows = evolved.match_records(QueryPlan::all(table)).expect("reads");
    assert!(rows.records().is_empty());
}

#[test]
fn a_prior_that_does_not_decode_fails_and_leaves_the_store_intact() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_plain("intact");
    fixture.seed_v1(&mut engine, &[ThoughtV1::new("alpha", "first")]);
    drop(engine);

    // The declared prior shape does not match the stored bytes: the
    // validated decode refuses, and nothing changes.
    let mut wrong = fixture.open_plain("intact");
    let outcome = wrong.upgrade_table::<CountedThought, Thought>(
        TableDescriptor::<Thought>::new(THOUGHTS, fixture.family(), fixture.hash_v2())
            .with_predecessor::<CountedThought>(),
    );
    let UpgradeOutcome::Failed(failure) = outcome else {
        panic!("mismatched predecessor bytes must fail, got {outcome:?}");
    };
    assert_eq!(failure.phase(), StoragePhase::Read);
    drop(wrong);

    let mut untouched = fixture.open_plain("intact");
    let table = untouched
        .register_table(fixture.descriptor_v1())
        .expect("store still carries the prior identity");
    let rows = untouched
        .match_records(QueryPlan::all(table))
        .expect("prior rows read");
    assert_eq!(
        rows.records().to_vec(),
        vec![ThoughtV1::new("alpha", "first")],
    );
}

#[test]
fn two_declared_generations_each_evolve_directly() {
    let fixture = Fixture::new();

    // A store parked at v1 and a store parked at v2 both land on v3
    // through their own declared step.
    let descriptor_v3 = || {
        TableDescriptor::<ThoughtV3>::new(
            THOUGHTS,
            FamilyName::new("thought"),
            ThoughtV3::schema_hash(),
        )
        .with_predecessor::<ThoughtV1>()
        .with_predecessor::<Thought>()
    };
    let landed = |outcome: UpgradeOutcome<ThoughtV3, Infallible>| match outcome {
        UpgradeOutcome::Upgraded(receipt) => receipt.table(),
        other => panic!("the store upgrades to v3, got {other:?}"),
    };
    let v3 = |key: &str, body: &str, mood: &str| ThoughtV3 {
        key: key.to_owned(),
        body: body.to_owned(),
        mood: mood.to_owned(),
    };

    let mut at_v1 = fixture.open_plain("ladder-v1");
    fixture.seed_v1(&mut at_v1, &[ThoughtV1::new("alpha", "first")]);
    drop(at_v1);
    let mut evolved_v1 = fixture.open_plain("ladder-v1");
    let table = landed(evolved_v1.upgrade_table::<ThoughtV1, ThoughtV3>(descriptor_v3()));
    assert_eq!(
        evolved_v1
            .match_records(QueryPlan::all(table))
            .expect("reads")
            .records(),
        &[v3("alpha", "first", "carried")],
    );

    let mut at_v2 = fixture.open_plain("ladder-v2");
    let v2_table = at_v2
        .register_table(TableDescriptor::<Thought>::new(
            THOUGHTS,
            fixture.family(),
            fixture.hash_v2(),
        ))
        .expect("v2 family registers");
    at_v2
        .assert(Assertion::new(
            v2_table,
            Thought::new("beta", "second", "byte-compatible"),
        ))
        .expect("v2 row asserts");
    drop(at_v2);
    let mut evolved_v2 = fixture.open_plain("ladder-v2");
    let table = landed(evolved_v2.upgrade_table::<Thought, ThoughtV3>(descriptor_v3()));
    assert_eq!(
        evolved_v2
            .match_records(QueryPlan::all(table))
            .expect("reads")
            .records(),
        &[v3("beta", "second", "byte-compatible")],
    );
}

#[test]
fn versioned_evolution_logs_retraction_and_assertion_as_row_history() {
    let fixture = Fixture::new();
    let mut engine = fixture.open_versioned("logged");
    fixture.seed_v1(&mut engine, &[ThoughtV1::new("alpha", "first")]);
    drop(engine);

    let mut evolved = fixture.open_versioned("logged");
    fixture.upgrade(&mut evolved);
    let log = evolved.versioned_commit_log().expect("versioned log reads");
    let evolution_entry = log.last().expect("evolution entry exists");

    let operations: Vec<_> = evolution_entry.operations().iter().collect();
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0].operation(), SemaOperation::Retract);
    assert_eq!(operations[0].family().schema_hash(), fixture.hash_v1());
    assert_eq!(*operations[0].payload(), VersionedPayload::tombstone());
    assert_eq!(operations[1].operation(), SemaOperation::Assert);
    assert_eq!(operations[1].family().schema_hash(), fixture.hash_v2());
    assert_ne!(*operations[1].payload(), VersionedPayload::tombstone());

    // The entry's derived store hash names the evolved inventory.
    assert_eq!(evolution_entry.schema_hash(), evolved.store_schema_hash());
}

/// 2026-07-18 incident, second defect: after a family evolution the versioned
/// log names the retired prior identity (the retract half of the carried
/// history). The checkpoint fold must resolve those keys from the identities
/// the log itself declares — a compaction immediately after an evolution
/// previously failed `FamilyUnknown`, wedging every store-open path whose
/// write maintenance triggers it.
#[test]
fn evolved_versioned_store_checkpoints_and_compacts_over_retired_identities() {
    let fixture = Fixture::new();
    {
        let mut engine = fixture.open_versioned("evolved-compaction");
        fixture.seed_v1(
            &mut engine,
            &[
                ThoughtV1::new("alpha", "first"),
                ThoughtV1::new("beta", "second"),
            ],
        );
    }
    let mut engine = fixture.open_versioned("evolved-compaction");
    let table = fixture.upgrade(&mut engine);
    let compacted = engine
        .compact_versioned_history(
            VersionedHistoryRetention::new(0),
            VersionedHistoryAcknowledgement::LocalCheckpoint,
        )
        .expect("checkpoint fold resolves retired identities from the log itself");
    assert!(compacted.compacted_entries() > 0);
    engine
        .assert(Assertion::new(
            table,
            Thought::new("gamma", "third", "calm"),
        ))
        .expect("post-compaction write maintains cleanly");
}

const COUNTS: TableName = TableName::new("counts");
const INDEXES: TableName = TableName::new("indexes");
const MARKERS: TableName = TableName::new("markers");

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct CountV1 {
    key: String,
    count: u64,
}
impl EngineRecord for CountV1 {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}
impl MemoryDefinition for CountV1 {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }
    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("count-v1")
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Count {
    key: String,
    count: u64,
    unit: String,
}
impl EngineRecord for Count {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}
impl MemoryDefinition for Count {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(2)
    }
    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("count-v2")
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Index {
    key: String,
}
impl EngineRecord for Index {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}
impl MemoryDefinition for Index {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }
    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("index-v1")
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Marker {
    key: String,
    value: String,
}
impl EngineRecord for Marker {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}
impl MemoryDefinition for Marker {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }
    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("marker-v1")
    }
}

fn count_v1() -> TableDescriptor<CountV1> {
    TableDescriptor::new(
        COUNTS,
        FamilyName::new("count"),
        SchemaHash::for_label("count-v1"),
    )
}
fn count_v2() -> TableDescriptor<Count> {
    TableDescriptor::new(
        COUNTS,
        FamilyName::new("count"),
        SchemaHash::for_label("count-v2"),
    )
    .with_predecessor::<CountV1>()
}
fn indexes() -> TableDescriptor<Index> {
    TableDescriptor::new(
        INDEXES,
        FamilyName::new("index"),
        SchemaHash::for_label("index-v1"),
    )
}
fn markers() -> TableDescriptor<Marker> {
    TableDescriptor::new(
        MARKERS,
        FamilyName::new("marker"),
        SchemaHash::for_label("marker-v1"),
    )
}

#[test]
fn heterogeneous_evolution_lands_catalog_rows_and_log_in_one_commit() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous");
    let mut predecessor =
        Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).expect("predecessor opens");
    let thoughts = predecessor
        .register_table(fixture.descriptor_v1())
        .expect("thoughts register");
    let counts = predecessor
        .register_table(count_v1())
        .expect("counts register");
    let marker = predecessor
        .register_table(markers())
        .expect("marker registers");
    predecessor
        .assert(Assertion::new(thoughts, ThoughtV1::new("t", "kept")))
        .expect("thought stored");
    predecessor
        .assert(Assertion::new(
            counts,
            CountV1 {
                key: "c".into(),
                count: 7,
            },
        ))
        .expect("count stored");
    predecessor
        .assert(Assertion::new(
            marker,
            Marker {
                key: "m".into(),
                value: "old".into(),
            },
        ))
        .expect("marker stored");
    drop(predecessor);

    let thought_current = fixture.descriptor_v2_with_prior();
    let count_current = count_v2();
    let mut engine = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_versioning(VersioningPolicy::new(VersionedStoreName::new(
                "heterogeneous",
            )))
            .with_family(&thought_current)
            .with_family(&count_current)
            .with_family(&markers()),
    )
    .expect("predecessor catalog is admitted");
    let before = engine.commit_log().expect("log reads").len();
    let versioned_before = engine
        .versioned_commit_log()
        .expect("versioned log reads")
        .len();
    let thought_inventory = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&thought_current)
        .expect("thoughts inspect");
    let count_inventory = engine
        .inspect_predecessor::<CountV1, Count>(&count_current)
        .expect("counts inspect");
    let marker = engine.register_table(markers()).expect("marker opens");
    let evolution = engine
        .begin_atomic_evolution()
        .evolve(
            thought_inventory,
            thought_current,
            vec![Thought::new("t", "kept", "carried")],
        )
        .evolve(
            count_inventory,
            count_current,
            vec![Count {
                key: "c".into(),
                count: 7,
                unit: "items".into(),
            }],
        )
        .register(indexes(), vec![Index { key: "i".into() }])
        .mutate(
            marker,
            Marker {
                key: "m".into(),
                value: "new".into(),
            },
        );
    let receipt = engine
        .commit_atomic_evolution(evolution)
        .expect("one evolution commits");
    assert_eq!(receipt.operation_count(), 6);
    assert_eq!(engine.commit_log().expect("log rereads").len(), before + 1);
    assert_eq!(
        engine
            .versioned_commit_log()
            .expect("versioned log rereads")
            .len(),
        versioned_before + 1
    );

    let thoughts = engine
        .register_table(fixture.descriptor_v2_with_prior())
        .expect("thoughts current");
    let counts = engine.register_table(count_v2()).expect("counts current");
    let indexes = engine.register_table(indexes()).expect("indexes current");
    assert_eq!(
        engine
            .match_records(QueryPlan::all(thoughts))
            .unwrap()
            .records()[0]
            .mood,
        "carried"
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::all(counts))
            .unwrap()
            .records()[0]
            .unit,
        "items"
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::all(indexes))
            .unwrap()
            .records()[0]
            .key,
        "i"
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::all(marker))
            .unwrap()
            .records()[0]
            .value,
        "new"
    );
    let committed_log = engine.commit_log().unwrap().len();
    drop(engine);
    let mut reopened = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_versioning(VersioningPolicy::new(VersionedStoreName::new(
                "heterogeneous",
            )))
            .with_family(&fixture.descriptor_v2_with_prior())
            .with_family(&count_v2())
            .with_family(&indexes())
            .with_family(&markers()),
    )
    .expect("wholly-current store reopens");
    assert_eq!(reopened.commit_log().unwrap().len(), committed_log);
    assert!(
        reopened
            .inspect_predecessor::<ThoughtV1, Thought>(&fixture.descriptor_v2_with_prior())
            .is_err(),
        "a current family is not evolved a second time"
    );
    assert_eq!(reopened.commit_log().unwrap().len(), committed_log);
    assert!(reopened.register_table(count_v2()).is_ok());
}

#[test]
fn interrupted_heterogeneous_evolution_leaves_every_family_predecessor() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous-interrupted");
    let mut predecessor = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).unwrap();
    let thoughts = predecessor.register_table(fixture.descriptor_v1()).unwrap();
    let counts = predecessor.register_table(count_v1()).unwrap();
    predecessor
        .assert(Assertion::new(thoughts, ThoughtV1::new("t", "kept")))
        .unwrap();
    predecessor
        .assert(Assertion::new(
            counts,
            CountV1 {
                key: "c".into(),
                count: 7,
            },
        ))
        .unwrap();
    drop(predecessor);

    let thought_current = fixture.descriptor_v2_with_prior();
    let count_current = count_v2();
    let mut engine = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_family(&thought_current)
            .with_family(&count_current),
    )
    .unwrap();
    let thought_inventory = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&thought_current)
        .unwrap();
    let count_inventory = engine
        .inspect_predecessor::<CountV1, Count>(&count_current)
        .unwrap();
    let before = engine.commit_log().unwrap().len();
    let evolution = engine
        .begin_atomic_evolution()
        .evolve(
            thought_inventory,
            thought_current,
            vec![Thought::new("t", "kept", "carried")],
        )
        .evolve(
            count_inventory,
            count_current,
            vec![Count {
                key: "c".into(),
                count: 7,
                unit: "items".into(),
            }],
        );
    engine.interrupt_upgrade_at(sema_engine::UpgradeFault::AfterRowsWritten);
    assert!(engine.commit_atomic_evolution(evolution).is_err());
    assert_eq!(engine.commit_log().unwrap().len(), before);
    let thoughts = engine
        .register_table(fixture.descriptor_v1())
        .expect("thought predecessor remains");
    let counts = engine
        .register_table(count_v1())
        .expect("count predecessor remains");
    assert_eq!(
        engine
            .match_records(QueryPlan::all(thoughts))
            .unwrap()
            .records()[0]
            .body,
        "kept"
    );
    assert_eq!(
        engine
            .match_records(QueryPlan::all(counts))
            .unwrap()
            .records()[0]
            .count,
        7
    );
    drop(engine);
    let mut reopened = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_family(&fixture.descriptor_v2_with_prior())
            .with_family(&count_v2()),
    )
    .expect("interrupted predecessor store reopens");
    assert!(reopened.register_table(fixture.descriptor_v1()).is_ok());
    assert!(reopened.register_table(count_v1()).is_ok());
}

#[test]
fn heterogeneous_evolution_may_drop_move_and_empty_predecessor_rows() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous-cardinality");
    let mut predecessor = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).unwrap();
    predecessor.register_table(fixture.descriptor_v1()).unwrap();
    let counts = predecessor.register_table(count_v1()).unwrap();
    predecessor
        .assert(Assertion::new(
            counts,
            CountV1 {
                key: "a".into(),
                count: 1,
            },
        ))
        .unwrap();
    predecessor
        .assert(Assertion::new(
            counts,
            CountV1 {
                key: "b".into(),
                count: 2,
            },
        ))
        .unwrap();
    drop(predecessor);

    let thoughts = fixture.descriptor_v2_with_prior();
    let counts = count_v2();
    let mut engine = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_family(&thoughts)
            .with_family(&counts),
    )
    .unwrap();
    let empty = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&thoughts)
        .unwrap();
    assert!(empty.rows().is_empty());
    let old_counts = engine
        .inspect_predecessor::<CountV1, Count>(&counts)
        .unwrap();
    assert_eq!(old_counts.rows().len(), 2);
    let evolution = engine
        .begin_atomic_evolution()
        .evolve(empty, thoughts, Vec::new())
        .evolve(
            old_counts,
            counts,
            vec![Count {
                key: "moved".into(),
                count: 3,
                unit: "total".into(),
            }],
        )
        .register(indexes(), vec![Index { key: "seed".into() }]);
    engine.commit_atomic_evolution(evolution).unwrap();
    let counts = engine.register_table(count_v2()).unwrap();
    let rows = engine.match_records(QueryPlan::all(counts)).unwrap();
    assert_eq!(
        rows.records(),
        &[Count {
            key: "moved".into(),
            count: 3,
            unit: "total".into()
        }]
    );
}

#[test]
fn heterogeneous_evolution_rejects_duplicate_tables_and_successor_keys_before_writing() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous-duplicates");
    let mut predecessor = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).unwrap();
    let thoughts = predecessor.register_table(fixture.descriptor_v1()).unwrap();
    predecessor
        .assert(Assertion::new(thoughts, ThoughtV1::new("t", "old")))
        .unwrap();
    drop(predecessor);

    let current = fixture.descriptor_v2_with_prior();
    let mut engine =
        Engine::open(EngineOpen::new(&path, SchemaVersion::new(1)).with_family(&current)).unwrap();
    let first = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&current)
        .unwrap();
    let second = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&current)
        .unwrap();
    let before = engine.commit_log().unwrap().len();
    let duplicate_table = engine
        .begin_atomic_evolution()
        .evolve(
            first,
            current.clone(),
            vec![Thought::new("t", "old", "one")],
        )
        .evolve(
            second,
            current.clone(),
            vec![Thought::new("t", "old", "two")],
        );
    assert!(engine.commit_atomic_evolution(duplicate_table).is_err());
    assert_eq!(engine.commit_log().unwrap().len(), before);

    let inventory = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&current)
        .unwrap();
    let duplicate_key = engine.begin_atomic_evolution().evolve(
        inventory,
        current,
        vec![
            Thought::new("same", "a", "x"),
            Thought::new("same", "b", "y"),
        ],
    );
    assert!(engine.commit_atomic_evolution(duplicate_key).is_err());
    assert_eq!(engine.commit_log().unwrap().len(), before);
    assert!(engine.register_table(fixture.descriptor_v1()).is_ok());
}

#[test]
fn stale_mixed_witness_refuses_before_another_predecessor_changes() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous-stale");
    let mut predecessor = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).unwrap();
    let thoughts = predecessor.register_table(fixture.descriptor_v1()).unwrap();
    let counts = predecessor.register_table(count_v1()).unwrap();
    predecessor
        .assert(Assertion::new(thoughts, ThoughtV1::new("t", "old")))
        .unwrap();
    predecessor
        .assert(Assertion::new(
            counts,
            CountV1 {
                key: "c".into(),
                count: 1,
            },
        ))
        .unwrap();
    drop(predecessor);

    let thought_current = fixture.descriptor_v2_with_prior();
    let count_current = count_v2();
    let mut engine = Engine::open(
        EngineOpen::new(&path, SchemaVersion::new(1))
            .with_family(&thought_current)
            .with_family(&count_current),
    )
    .unwrap();
    let stale = engine
        .inspect_predecessor::<ThoughtV1, Thought>(&thought_current)
        .unwrap();
    let count_inventory = engine
        .inspect_predecessor::<CountV1, Count>(&count_current)
        .unwrap();
    assert!(matches!(
        engine.upgrade_table::<ThoughtV1, Thought>(thought_current.clone()),
        UpgradeOutcome::Upgraded(_)
    ));
    let before = engine.commit_log().unwrap().len();
    let evolution = engine
        .begin_atomic_evolution()
        .evolve(
            stale,
            thought_current,
            vec![Thought::new("t", "old", "stale")],
        )
        .evolve(
            count_inventory,
            count_current,
            vec![Count {
                key: "c".into(),
                count: 1,
                unit: "item".into(),
            }],
        );
    assert!(engine.commit_atomic_evolution(evolution).is_err());
    assert_eq!(engine.commit_log().unwrap().len(), before);
    assert!(
        engine.register_table(count_v1()).is_ok(),
        "other predecessor remains"
    );
}

#[test]
fn undeclared_predecessor_inspection_is_read_only() {
    let fixture = Fixture::new();
    let path = fixture.database_path("heterogeneous-undeclared");
    let mut engine = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1))).unwrap();
    let thoughts = engine.register_table(fixture.descriptor_v1()).unwrap();
    engine
        .assert(Assertion::new(thoughts, ThoughtV1::new("t", "old")))
        .unwrap();
    let before = engine.commit_log().unwrap().len();
    let undeclared = TableDescriptor::<Thought>::new(THOUGHTS, fixture.family(), fixture.hash_v2());
    assert!(
        engine
            .inspect_predecessor::<ThoughtV1, Thought>(&undeclared)
            .is_err()
    );
    assert_eq!(engine.commit_log().unwrap().len(), before);
    assert!(engine.register_table(fixture.descriptor_v1()).is_ok());
}
