//! Stored-format witness for the archived commit log entries.
//!
//! `CommitLogEntry` and `VersionedCommitLogEntry` are `rkyv::Archive`
//! types written to durable storage, and both carry their operations
//! in a `NonEmpty`. The golden byte strings below were captured from a
//! build that still took `NonEmpty` from `signal-frame`; they pin the
//! archived layout across the move of that type into this crate, so a
//! store written by an earlier build stays readable.

use memory_engine::{
    Catalog, CommitLogEntry, CommitLogOperation, CommitSequence, EntryDigest, FamilyIdentity,
    FamilyName, NonEmpty, RecordKey, SchemaHash, SnapshotIdentifier, StoreSchemaHash, TableName,
    VersionedCommitLogEntry, VersionedLogOperation, VersionedPayload, VersionedStoreName,
};
use signal_sema::SemaOperation;

const COMMIT_LOG_ENTRY_ARCHIVE: &str = "0174686f7567687473010062657461ffffffff0374686f75676874730000000000000000000007000000000000000b000000000000000074686f75676874730100616c706861ffffffb7ffffff02000000";
const VERSIONED_COMMIT_LOG_ENTRY_ARCHIVE: &str = "7769746e6573732d73746f7265010203040254686f75676874ff030303030303030303030303030303030303030303030303030303030303030374686f7567687473010062657461ffffffff0100000000000000008d000000abffffff1e15b6a433f4dcd3d00c9a9ed9edca2d413067d9386d5a2f325d574c46b627ce07000000000000000b0000000000000001050505050505050505050505050505050505050505050505050505050505050514cc522845067a875544c311181e7fe7990abcc41d4f161af252093005c299dc0054686f75676874ff030303030303030303030303030303030303030303030303030303030303030374686f75676874730100616c706861ffffff0003ffffff04000000fffeffff01000000";
const VERSIONED_ENTRY_DIGEST: &str =
    "14cc522845067a875544c311181e7fe7990abcc41d4f161af252093005c299dc";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn commit_log_entry() -> CommitLogEntry {
    CommitLogEntry::new(
        CommitSequence::new(7),
        SnapshotIdentifier::new(11),
        NonEmpty::from_head_and_tail(
            CommitLogOperation::new(
                SemaOperation::Assert,
                TableName::new("thoughts"),
                Some(RecordKey::new("alpha")),
            ),
            vec![
                CommitLogOperation::new(
                    SemaOperation::Mutate,
                    TableName::new("thoughts"),
                    Some(RecordKey::new("beta")),
                ),
                CommitLogOperation::new(SemaOperation::Match, TableName::new("thoughts"), None),
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
                SemaOperation::Assert,
                family.clone(),
                Some(RecordKey::new("alpha")),
                VersionedPayload::record(vec![1, 2, 3, 4]),
            ),
            vec![VersionedLogOperation::new(
                SemaOperation::Retract,
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
