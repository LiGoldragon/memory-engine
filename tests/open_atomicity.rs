//! An open's durable effect is one write transaction: the layout stamp,
//! the versioning-policy row and the declared families' catalog rows land
//! together or not at all. The witnesses interrupt the open between the
//! stamp and the rest, as a crash there would, and read the raw store
//! through the kernel to see what survived. Expected slot values come
//! from ARCHITECTURE.md (storage layout 7), not from the engine.

use std::path::PathBuf;

use memory_engine::{
    Decision, Engine, EngineOpen, EngineRecord, Error, FamilyName, MemoryDefinition,
    MemoryUpgrades, MemoryVersion, OpenFault, RecordKey, SchemaHash, SchemaVersion,
    TableDescriptor, TableName, TableRegistration, UpgradeFrom, UpgradeOutcome, VersionedStoreName,
    VersioningPolicy,
};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use tempfile::TempDir;

const COUNTERS: ::memory::Table<&'static str, u64> =
    ::memory::Table::new("__memory_engine_counters");
const LAYOUT_KEY: &str = "engine_storage_layout";
const POLICY: ::memory::Table<&'static str, VersioningPolicy> =
    ::memory::Table::new("__memory_engine_versioning_policy");
const POLICY_KEY: &str = "configured";
const CATALOG: ::memory::Table<&'static str, TableRegistration> =
    ::memory::Table::new("__memory_engine_catalog");
const DOCUMENTED_LAYOUT: u64 = 7;

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Note {
    key: String,
}

impl EngineRecord for Note {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

impl MemoryDefinition for Note {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("note-v1")
    }
}

/// The byte-compatible second definition of `Note`.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct NoteV2 {
    key: String,
}

impl MemoryDefinition for NoteV2 {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(2)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("note-v2")
    }
}

impl UpgradeFrom<Note> for NoteV2 {
    type Refusal = std::convert::Infallible;

    fn upgrade_from(note: Note) -> Decision<Self, Self::Refusal> {
        Decision::Ready(NoteV2 { key: note.key })
    }
}

/// What the raw store holds of an open's durable effect.
#[derive(Debug, PartialEq, Eq)]
struct Stamped {
    layout: Option<u64>,
    policy: Option<VersioningPolicy>,
    families: Vec<String>,
}

struct Store {
    directory: TempDir,
}

impl Store {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temp dir"),
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("open.memory")
    }

    fn policy(&self) -> VersioningPolicy {
        VersioningPolicy::new(VersionedStoreName::new("open"))
    }

    fn request(&self) -> EngineOpen {
        EngineOpen::new(self.path(), SchemaVersion::new(1)).with_versioning(self.policy())
    }

    fn notes(&self, version: &str) -> TableDescriptor<Note> {
        TableDescriptor::new(
            TableName::new("notes"),
            FamilyName::new("note"),
            SchemaHash::for_label(version),
        )
    }

    fn notes_elsewhere(&self) -> TableDescriptor<Note> {
        TableDescriptor::new(
            TableName::new("notes_again"),
            FamilyName::new("note"),
            SchemaHash::for_label("note-v1"),
        )
    }

    fn stamped(&self) -> Stamped {
        let kernel = ::memory::Memory::open_with_schema(
            &self.path(),
            &::memory::Schema {
                version: SchemaVersion::new(1),
            },
        )
        .expect("kernel opens");
        kernel
            .read(|transaction| {
                Ok(Stamped {
                    layout: COUNTERS.get(transaction, LAYOUT_KEY)?,
                    policy: POLICY.get(transaction, POLICY_KEY)?,
                    families: CATALOG
                        .iter(transaction)?
                        .into_iter()
                        .map(|(table, _registration)| table.to_owned())
                        .collect(),
                })
            })
            .expect("raw store reads")
    }

    fn unstamped() -> Stamped {
        Stamped {
            layout: None,
            policy: None,
            families: Vec::new(),
        }
    }
}

#[test]
fn an_open_interrupted_after_the_layout_stamp_leaves_the_store_unstamped() {
    let store = Store::new();
    let error = Engine::open(
        store
            .request()
            .with_family(&store.notes("note-v1"))
            .interrupted_at(OpenFault::AfterLayoutStamped),
    )
    .err()
    .expect("the interrupted open fails");
    assert!(matches!(
        error,
        Error::OpenInterrupted {
            fault: OpenFault::AfterLayoutStamped
        }
    ));
    assert_eq!(store.stamped(), Store::unstamped());
}

#[test]
fn a_reopen_after_the_interruption_stamps_everything_at_once() {
    let store = Store::new();
    let _ = Engine::open(
        store
            .request()
            .with_family(&store.notes("note-v1"))
            .interrupted_at(OpenFault::AfterLayoutStamped),
    );
    let engine = Engine::open(store.request().with_family(&store.notes("note-v1")))
        .expect("the clean reopen succeeds");
    // The declared family is registered by the open itself, before any
    // register_table call.
    assert_eq!(engine.list_tables().len(), 1);
    drop(engine);
    assert_eq!(
        store.stamped(),
        Stamped {
            layout: Some(DOCUMENTED_LAYOUT),
            policy: Some(store.policy()),
            families: vec!["notes".to_owned()],
        }
    );
}

#[test]
fn a_family_failure_at_open_leaves_a_fresh_store_unstamped() {
    let store = Store::new();
    let error = Engine::open(
        store
            .request()
            .with_family(&store.notes("note-v1"))
            .with_family(&store.notes_elsewhere()),
    )
    .err()
    .expect("one family bound to two tables refuses the open");
    assert!(matches!(error, Error::FamilyAlreadyBound { .. }));
    assert_eq!(store.stamped(), Store::unstamped());
}

#[test]
fn a_declared_family_conflicting_with_the_catalog_refuses_the_open() {
    let store = Store::new();
    let mut engine = Engine::open(store.request()).expect("first open");
    engine
        .register_table(store.notes("note-v1"))
        .expect("v1 registers");
    drop(engine);

    let error = Engine::open(store.request().with_family(&store.notes("note-v2")))
        .err()
        .expect("an undeclared generation change refuses the open");
    assert!(matches!(error, Error::FamilyIdentityMismatch { .. }));
}

#[test]
fn a_declared_family_evolving_from_the_stored_generation_opens() {
    let store = Store::new();
    let mut engine = Engine::open(store.request()).expect("first open");
    engine
        .register_table(store.notes("note-v1"))
        .expect("v1 registers");
    drop(engine);

    let evolving = TableDescriptor::<NoteV2>::new(
        TableName::new("notes"),
        FamilyName::new("note"),
        NoteV2::schema_hash(),
    )
    .with_predecessor::<Note>();
    let mut engine =
        Engine::open(store.request().with_family(&evolving)).expect("a declared prior opens");
    let outcome = engine.upgrade_table::<Note, NoteV2>(evolving);
    assert!(
        matches!(outcome, UpgradeOutcome::Upgraded(_)),
        "v2 upgrades from v1, got {outcome:?}"
    );
}
