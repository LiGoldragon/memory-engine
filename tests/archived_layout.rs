//! Stored-format witness for the archived commit log entries.
//!
//! `CommitLogEntry` and `VersionedCommitLogEntry` are `rkyv::Archive`
//! types written to durable storage, and both carry their operations
//! in a `NonEmpty`. Golden byte strings pin the Memory archived
//! representation, including its namespace-bound schema and entry digests.

use memory_engine::{
    Catalog, CommitLogEntry, CommitLogOperation, CommitSequence, EntryDigest, FamilyIdentity,
    FamilyName, NonEmpty, RecordKey, SchemaHash, SnapshotIdentifier, StoreSchemaHash, TableName,
    VersionedCommitLogEntry, VersionedLogOperation, VersionedPayload, VersionedStoreName,
};
use signal_memory::MemoryOperation;

const COMMIT_LOG_ENTRY_ARCHIVE: &str = "0174686f7567687473010062657461ffffffff0374686f75676874730000000000000000000007000000000000000b000000000000000074686f75676874730100616c706861ffffffb7ffffff02000000";
const VERSIONED_COMMIT_LOG_ENTRY_ARCHIVE: &str = "7769746e6573732d73746f7265010203040254686f75676874ff030303030303030303030303030303030303030303030303030303030303030374686f7567687473010062657461ffffffff0100000000000000008d000000abffffff2f7a9db3184997e5cfa357a1d76adc12bcb24b199e55ad3b2c70cf9a1fac4dcd07000000000000000b000000000000000105050505050505050505050505050505050505050505050505050505050505051d3ae3109e73933fbd3baa124fe169877f5b0aafd87f8472c95ee8fbecd49fab0054686f75676874ff030303030303030303030303030303030303030303030303030303030303030374686f75676874730100616c706861ffffff0003ffffff04000000fffeffff01000000";
const VERSIONED_ENTRY_DIGEST: &str =
    "1d3ae3109e73933fbd3baa124fe169877f5b0aafd87f8472c95ee8fbecd49fab";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn commit_log_entry() -> CommitLogEntry {
    CommitLogEntry::new(
        CommitSequence::new(7),
        SnapshotIdentifier::new(11),
        NonEmpty::from_head_and_tail(
            CommitLogOperation::new(
                MemoryOperation::Assert,
                TableName::new("thoughts"),
                Some(RecordKey::new("alpha")),
            ),
            vec![
                CommitLogOperation::new(
                    MemoryOperation::Mutate,
                    TableName::new("thoughts"),
                    Some(RecordKey::new("beta")),
                ),
                CommitLogOperation::new(MemoryOperation::Match, TableName::new("thoughts"), None),
            ],
        ),
    )
}

fn versioned_commit_log_entry() -> VersionedCommitLogEntry {
    let family = FamilyIdentity::new(
        FamilyName::new("Thought"),
        SchemaHash::new([3u8; 32]),
        TableName::new("thoughts"),
    );
    VersionedCommitLogEntry::new(
        VersionedStoreName::new("witness-store"),
        StoreSchemaHash::from(&Catalog::new(Vec::new())),
        CommitSequence::new(7),
        SnapshotIdentifier::new(11),
        Some(EntryDigest::new([5u8; 32])),
        NonEmpty::from_head_and_tail(
            VersionedLogOperation::new(
                MemoryOperation::Assert,
                family.clone(),
                Some(RecordKey::new("alpha")),
                VersionedPayload::record(vec![1, 2, 3, 4]),
            ),
            vec![VersionedLogOperation::new(
                MemoryOperation::Retract,
                family,
                Some(RecordKey::new("beta")),
                VersionedPayload::tombstone(),
            )],
        ),
    )
}

#[test]
fn archived_commit_log_entry_bytes_are_unchanged() {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&commit_log_entry())
        .expect("commit log entry serializes");
    assert_eq!(hex(&bytes), COMMIT_LOG_ENTRY_ARCHIVE);
}

#[test]
fn archived_versioned_commit_log_entry_bytes_are_unchanged() {
    let entry = versioned_commit_log_entry();
    assert_eq!(hex(entry.entry_digest().bytes()), VERSIONED_ENTRY_DIGEST);
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&entry)
        .expect("versioned commit log entry serializes");
    assert_eq!(hex(&bytes), VERSIONED_COMMIT_LOG_ENTRY_ARCHIVE);
}

#[test]
fn empty_catalog_schema_hash_uses_memory_namespace() {
    let hash = StoreSchemaHash::from(&Catalog::new(Vec::new()));
    assert_eq!(
        hex(hash.bytes()),
        "2f7a9db3184997e5cfa357a1d76adc12bcb24b199e55ad3b2c70cf9a1fac4dcd"
    );
}
