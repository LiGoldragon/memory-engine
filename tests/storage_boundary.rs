//! Architectural truth witnesses for the storage boundary: the public
//! engine API exposes no durable-write path that bypasses the commit
//! log. The narrowed kernel handoff type, [`memory_engine::StorageReader`],
//! simply has no write affordance — the type system is the witness;
//! these tests pin the source surface so the affordance cannot
//! silently return.

use memory_engine::{
    Assertion, Engine, EngineOpen, EngineRecord, FamilyName, RecordKey, SchemaHash, SchemaVersion,
    StorageKernelTable, TableDescriptor, TableName,
};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use tempfile::TempDir;

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Thought {
    key: String,
    body: String,
}

impl EngineRecord for Thought {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

struct EngineFixture {
    directory: TempDir,
}

impl EngineFixture {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temp dir is created"),
        }
    }

    fn open_engine(&self) -> Engine {
        Engine::open(EngineOpen::new(
            self.directory.path().join("engine.memory"),
            SchemaVersion::new(1),
        ))
        .expect("engine opens")
    }
}

#[test]
fn storage_reader_observes_state_written_through_logged_choke_points() {
    const COUNTERS: StorageKernelTable<&'static str, u64> =
        StorageKernelTable::new("__memory_engine_counters");

    let fixture = EngineFixture::new();
    let mut engine = fixture.open_engine();
    let thoughts = engine
        .register_table(TableDescriptor::<Thought>::new(
            TableName::new("thoughts"),
            FamilyName::new("thought"),
            SchemaHash::for_label("thought-v1"),
        ))
        .expect("table registers");
    engine
        .assert(Assertion::new(
            thoughts,
            Thought {
                key: "alpha".into(),
                body: "first".into(),
            },
        ))
        .expect("assert succeeds");

    let observed = engine
        .storage_reader()
        .read(|transaction| COUNTERS.get(transaction, "latest_commit_sequence"))
        .expect("reader reads");

    assert_eq!(observed, Some(1));
}
