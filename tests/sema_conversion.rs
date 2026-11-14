//! A store sema-engine wrote converts into the memory format in one call,
//! and the converted store opens, folds, verifies, compacts again and
//! reads back the records it held.
//!
//! The fixtures under `fixtures/sema/` were written by sema-engine 27e814a
//! (0.15.1, storage layout 7, sema kernel 51d7927), built and run through
//! Nix by `fixtures/sema/write_legacy_fixtures.rs`; see that file for each
//! store's history. `lojix-8.1.0.sema` is the store Lojix 8.1.0 wrote, byte
//! for byte the fixture of lojix 09f5b50.

use std::path::PathBuf;

use memory_engine::{
    Assertion, CommitSequence, Durability, Engine, EngineOpen, EngineRecord, Error,
    FamilyDirectory, FamilyName, IdentifiedQueryPlan, IdentifiedTableDescriptor,
    IdentifiedTableReference, LegacySemaStore, MirrorAcknowledgement, MirrorHead, QueryPlan,
    RecordKey, RowMaterializer, SchemaHash, SchemaVersion, StorageKernelError, TableDescriptor,
    TableName, TableReference, VersionedCommitLogEntry, VersionedHistoryAcknowledgement,
    VersionedHistoryRetention, VersionedRecoveryTopology, VersionedStoreName, VersioningPolicy,
};
use rkyv::rancor;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use tempfile::TempDir;

const ENTRIES: &[u8] = include_bytes!("fixtures/sema/entries.sema");
const COMPACTED: &[u8] = include_bytes!("fixtures/sema/compacted.sema");
const CHAINED: &[u8] = include_bytes!("fixtures/sema/chained.sema");
const MIRRORED: &[u8] = include_bytes!("fixtures/sema/mirrored.sema");
const GROWN: &[u8] = include_bytes!("fixtures/sema/grown.sema");
const STAGED: &[u8] = include_bytes!("fixtures/sema/staged.sema");
const LOJIX: &[u8] = include_bytes!("fixtures/sema/lojix-8.1.0.sema");

const VERSIONED_COMMIT_LOG: ::memory::Table<u64, VersionedCommitLogEntry> =
    ::memory::Table::new("__memory_engine_versioned_commit_log");
const CHECKPOINTS: ::memory::Table<u64, memory_engine::CheckpointMetadata> =
    ::memory::Table::new("__memory_engine_checkpoints");

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Thought {
    key: String,
    body: String,
}

impl Thought {
    fn new(key: &str, body: &str) -> Self {
        Self {
            key: key.to_owned(),
            body: body.to_owned(),
        }
    }
}

impl EngineRecord for Thought {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
struct Tally {
    note: String,
}

/// One legacy store copied out of its fixture: the copy is converted, and
/// moved to the memory name only when the conversion succeeds.
struct LegacyCopy {
    directory: TempDir,
}

impl LegacyCopy {
    fn of(fixture: &[u8]) -> Self {
        let copy = Self {
            directory: tempfile::tempdir().expect("temp directory"),
        };
        std::fs::write(copy.legacy_path(), fixture).expect("fixture copies");
        copy
    }

    fn legacy_path(&self) -> PathBuf {
        self.directory.path().join("store.sema")
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("store.memory")
    }

    /// The fixture placed at the memory name itself, as a consumer that
    /// renamed its store file finds it: the engine open upgrades it there.
    fn in_place(fixture: &[u8]) -> Self {
        let copy = Self {
            directory: tempfile::tempdir().expect("temp directory"),
        };
        std::fs::write(copy.path(), fixture).expect("fixture copies");
        copy
    }

    fn convert(&self) -> memory_engine::Result<memory_engine::SemaConversion> {
        let conversion = LegacySemaStore::new(self.legacy_path()).convert()?;
        std::fs::rename(self.legacy_path(), self.path()).expect("converted copy moves");
        Ok(conversion)
    }

    fn policy(&self, topology: VersionedRecoveryTopology) -> EngineOpen {
        EngineOpen::new(self.path(), SchemaVersion::new(1)).with_versioning(
            VersioningPolicy::new(VersionedStoreName::new("legacy"))
                .with_recovery_topology(topology),
        )
    }

    fn open(&self) -> Store {
        Store::open(self.policy(VersionedRecoveryTopology::LocalCheckpoint))
    }

    fn open_mirrored(&self) -> Store {
        Store::open(self.policy(VersionedRecoveryTopology::Mirror))
    }

    fn raw_kernel(&self) -> ::memory::Memory {
        ::memory::Memory::open_with_schema(
            &self.path(),
            &::memory::Schema {
                version: SchemaVersion::new(1),
            },
        )
        .expect("raw kernel opens")
    }
}

/// A converted store opened with its two families registered.
struct Store {
    engine: Engine,
    thoughts: TableReference<Thought>,
    tallies: IdentifiedTableReference<Tally>,
}

impl Store {
    fn open(request: EngineOpen) -> Self {
        let mut engine = Engine::open(request).expect("converted store opens");
        let thoughts = engine
            .register_table(TableDescriptor::new(
                TableName::new("thoughts"),
                FamilyName::new("thought"),
                SchemaHash::for_label("thought-v1"),
            ))
            .expect("thoughts register");
        let tallies = engine
            .register_identified_table(IdentifiedTableDescriptor::new(
                TableName::new("tallies"),
                FamilyName::new("tally"),
                SchemaHash::for_label("tally-v1"),
            ))
            .expect("tallies register");
        Self {
            engine,
            thoughts,
            tallies,
        }
    }

    /// The records the ten-entry history leaves.
    fn assert_records(&self) {
        let thoughts = self
            .engine
            .match_records(QueryPlan::all(self.thoughts))
            .expect("thoughts read");
        assert_eq!(
            thoughts.records(),
            &[
                Thought::new("alpha", "second"),
                Thought::new("delta", "first"),
                Thought::new("gamma", "second"),
            ]
        );
        let tallies = self
            .engine
            .match_identified(IdentifiedQueryPlan::all(self.tallies))
            .expect("tallies read");
        let notes: Vec<&str> = tallies
            .records()
            .iter()
            .map(|record| record.value().note.as_str())
            .collect();
        assert_eq!(notes, ["one again", "two"]);
    }

    fn log(&self) -> Vec<VersionedCommitLogEntry> {
        self.engine.versioned_commit_log().expect("log reads")
    }

    /// Every retained entry carries the store's own schema hash and names
    /// its predecessor's digest.
    fn assert_chain(&self) {
        let log = self.log();
        for pair in log.windows(2) {
            assert_eq!(
                pair[1].previous_entry_digest(),
                Some(pair[0].entry_digest())
            );
        }
        for entry in &log {
            assert_eq!(entry.schema_hash(), self.engine.store_schema_hash());
        }
        assert_eq!(
            self.engine.versioned_chain_head().expect("head reads"),
            log.last().map(VersionedCommitLogEntry::entry_digest)
        );
    }

    fn rebuild(&self) -> usize {
        self.engine
            .rebuild_from_log(&Directory {
                thoughts: self.thoughts,
                tallies: self.tallies,
            })
            .expect("store rebuilds from its log")
            .folded_entry_count()
    }

    fn compact(&self) {
        self.engine
            .compact_versioned_history(
                VersionedHistoryRetention::new(0),
                VersionedHistoryAcknowledgement::LocalCheckpoint,
            )
            .expect("converted history compacts");
    }
}

struct Directory {
    thoughts: TableReference<Thought>,
    tallies: IdentifiedTableReference<Tally>,
}

impl FamilyDirectory for Directory {
    fn materialize(&self, row: RowMaterializer<'_>) -> memory_engine::Result<()> {
        match row.family().family().as_str() {
            "thought" => row.apply(self.thoughts),
            "tally" => row.apply_identified(self.tallies),
            other => panic!("unexpected family {other}"),
        }
    }
}

#[test]
fn a_store_with_entries_converts_and_serves_its_records() {
    let copy = LegacyCopy::of(ENTRIES);
    let conversion = copy.convert().expect("entries store converts");
    assert_eq!(conversion.entries(), 10);
    assert_eq!(conversion.checkpoints(), 0);
    assert!(conversion.kept_digests().is_empty());

    let store = copy.open();
    store.assert_records();
    store.assert_chain();
    assert_eq!(store.log()[0].previous_entry_digest(), None);
    assert_eq!(store.rebuild(), 10);
    store
        .engine
        .checkpoint()
        .expect("converted log checkpoints");
    store
        .engine
        .latest_checkpoint()
        .expect("checkpoint verifies")
        .expect("checkpoint exists");
    store.compact();
    assert!(store.log().is_empty());

    let head = store.engine.versioned_chain_head().expect("head reads");
    store
        .engine
        .assert(Assertion::new(
            store.thoughts,
            Thought::new("epsilon", "after"),
        ))
        .expect("converted store takes a write");
    assert_eq!(store.log()[0].previous_entry_digest(), head);
    drop(store);

    let reopened = copy.open();
    assert_eq!(reopened.rebuild(), 1);
    let thoughts = reopened
        .engine
        .match_records(QueryPlan::all(reopened.thoughts))
        .expect("thoughts read");
    assert_eq!(thoughts.records().len(), 4);
}

#[test]
fn a_compacted_store_converts_and_keeps_its_cut_off_origin() {
    let copy = LegacyCopy::of(COMPACTED);
    let conversion = copy.convert().expect("compacted store converts");
    assert_eq!(conversion.entries(), 4);
    assert_eq!(conversion.checkpoints(), 1);
    assert_eq!(conversion.segments(), 1);
    let [origin] = conversion.kept_digests() else {
        panic!("one cut-off digest is kept");
    };

    let store = copy.open();
    store.assert_records();
    store.assert_chain();
    let checkpoint = store
        .engine
        .latest_checkpoint()
        .expect("converted checkpoint verifies")
        .expect("checkpoint exists");
    assert_eq!(
        checkpoint.metadata().covered().last(),
        CommitSequence::new(6)
    );
    assert_eq!(checkpoint.metadata().covered_entry_digest(), *origin);
    assert_eq!(
        checkpoint.metadata().store_schema_hash(),
        store.engine.store_schema_hash()
    );
    let log = store.log();
    assert_eq!(log[0].commit_sequence(), CommitSequence::new(7));
    assert_eq!(log[0].previous_entry_digest(), Some(*origin));
    assert!(log.iter().all(|entry| entry.entry_digest() != *origin));
    assert_eq!(store.rebuild(), 4, "the fold starts at the kept origin");

    let last = log.last().expect("log holds entries").entry_digest();
    store.compact();
    let compacted = store
        .engine
        .latest_checkpoint()
        .expect("recompacted checkpoint verifies")
        .expect("checkpoint exists");
    assert_eq!(
        compacted.metadata().covered().last(),
        CommitSequence::new(10)
    );
    assert_eq!(compacted.metadata().covered_entry_digest(), last);
    drop(store);

    let reopened = copy.open();
    reopened.assert_records();
    assert_eq!(reopened.rebuild(), 0);
}

#[test]
fn a_chained_store_relinks_its_checkpoints() {
    let copy = LegacyCopy::of(CHAINED);
    let conversion = copy.convert().expect("chained store converts");
    assert_eq!(conversion.entries(), 4);
    assert_eq!(conversion.checkpoints(), 2);
    assert_eq!(conversion.kept_digests().len(), 1);

    let store = copy.open();
    store.assert_records();
    store.assert_chain();
    let latest = store
        .engine
        .latest_checkpoint()
        .expect("chained checkpoint verifies")
        .expect("checkpoint exists");
    assert_eq!(latest.metadata().covered().last(), CommitSequence::new(9));
    let ninth = store
        .log()
        .into_iter()
        .find(|entry| entry.commit_sequence() == CommitSequence::new(9))
        .expect("entry 9 is retained");
    assert_eq!(
        latest.metadata().covered_entry_digest(),
        ninth.entry_digest()
    );
    let root = store
        .engine
        .storage_reader()
        .read(|transaction| CHECKPOINTS.get(transaction, 1))
        .expect("root reads")
        .expect("root checkpoint is stored");
    assert_eq!(
        latest.metadata().previous_checkpoint_digest(),
        Some(root.checkpoint_digest())
    );
    assert_eq!(root.covered_entry_digest(), conversion.kept_digests()[0]);
    assert_eq!(store.rebuild(), 1);
    store.compact();
    drop(store);

    let reopened = copy.open();
    reopened.assert_records();
}

#[test]
fn a_mirrored_store_converts_its_outbox_and_acknowledged_head() {
    let copy = LegacyCopy::of(MIRRORED);
    copy.convert().expect("mirrored store converts");

    let store = copy.open_mirrored();
    store.assert_records();
    store.assert_chain();
    let log = store.log();
    let head = store
        .engine
        .mirror_head()
        .expect("mirror head reads")
        .expect("mirror head is recorded");
    assert_eq!(
        head,
        MirrorHead::new(CommitSequence::new(4), log[3].entry_digest())
    );
    let unshipped: Vec<_> = store
        .engine
        .unshipped_outbox()
        .expect("outbox reads")
        .iter()
        .map(|row| (row.commit_sequence(), row.entry_digest()))
        .collect();
    let expected: Vec<_> = log[4..]
        .iter()
        .map(|entry| (entry.commit_sequence(), entry.entry_digest()))
        .collect();
    assert_eq!(unshipped, expected);
    let last = log.last().expect("log holds entries");
    let acknowledgement = store
        .engine
        .acknowledge_mirror(MirrorHead::new(last.commit_sequence(), last.entry_digest()))
        .expect("the converted head acknowledges");
    assert!(matches!(
        acknowledgement,
        MirrorAcknowledgement::Advanced(_)
    ));
    assert_eq!(
        store
            .engine
            .durability_of(last.commit_sequence())
            .expect("durability reads"),
        Durability::ServerCommitted
    );
}

#[test]
fn the_lojix_8_1_0_store_converts_folds_and_compacts() {
    let copy = LegacyCopy::of(LOJIX);
    let conversion = copy.convert().expect("the lojix store converts");
    assert_eq!(conversion.entries(), 7);
    assert_eq!(conversion.checkpoints(), 0);
    let open = || {
        Engine::open(
            EngineOpen::new(copy.path(), SchemaVersion::new(5)).with_versioning(
                VersioningPolicy::new(VersionedStoreName::new("lojix"))
                    .with_retention(VersionedHistoryRetention::new(4_096)),
            ),
        )
        .expect("the converted lojix store opens")
    };
    let engine = open();
    let log = engine.versioned_commit_log().expect("log reads");
    assert_eq!(log.len(), conversion.entries());
    for entry in &log {
        assert_eq!(entry.schema_hash(), engine.store_schema_hash());
    }
    engine
        .checkpoint()
        .expect("the converted log folds into a checkpoint");
    engine
        .latest_checkpoint()
        .expect("checkpoint verifies")
        .expect("checkpoint exists");
    engine
        .compact_versioned_history(
            VersionedHistoryRetention::new(0),
            VersionedHistoryAcknowledgement::LocalCheckpoint,
        )
        .expect("the converted lojix history compacts");
    drop(engine);
    let reopened = open();
    assert!(
        reopened
            .versioned_commit_log()
            .expect("log reads")
            .is_empty()
    );
    reopened
        .latest_checkpoint()
        .expect("checkpoint verifies after reopen")
        .expect("checkpoint exists");
}

#[test]
fn a_store_whose_entries_predate_its_catalog_is_refused_untouched() {
    let copy = LegacyCopy::of(GROWN);
    let refusal = LegacySemaStore::new(copy.legacy_path())
        .convert()
        .expect_err("no family list the store holds derives entry 1's schema hash");
    assert!(matches!(
        refusal,
        Error::SemaSchemaHashUnmatched { sequence: 1, .. }
    ));
    assert_eq!(
        std::fs::read(copy.legacy_path()).expect("file reads"),
        GROWN
    );
}

#[test]
fn a_store_with_a_parked_staged_group_is_refused_untouched() {
    let copy = LegacyCopy::of(STAGED);
    let refusal = LegacySemaStore::new(copy.legacy_path())
        .convert()
        .expect_err("a parked staged group is not converted");
    assert!(matches!(refusal, Error::SemaStagingPending));
    assert_eq!(
        std::fs::read(copy.legacy_path()).expect("file reads"),
        STAGED
    );
}

#[test]
fn a_converted_store_is_not_converted_again() {
    let copy = LegacyCopy::of(ENTRIES);
    copy.convert().expect("entries store converts");
    let before = std::fs::read(copy.path()).expect("file reads");
    let refusal = LegacySemaStore::new(copy.path())
        .convert()
        .expect_err("a memory store is not a sema store");
    assert!(matches!(
        refusal,
        Error::Memory(StorageKernelError::AlreadyMemoryFile { .. })
    ));
    assert_eq!(std::fs::read(copy.path()).expect("file reads"), before);
}

/// The kept origin is where the fold starts, not a link it skips: an
/// entry after it that names anything else breaks the chain there.
#[test]
fn the_fold_holds_the_first_entry_to_the_kept_origin() {
    let copy = LegacyCopy::of(COMPACTED);
    copy.convert().expect("compacted store converts");
    let first = copy.open().log()[0].clone();
    let relinked = VersionedCommitLogEntry::new(
        first.store_name().clone(),
        first.schema_hash(),
        first.commit_sequence(),
        first.snapshot(),
        Some(first.entry_digest()),
        first.operations().clone(),
    );
    copy.raw_kernel()
        .write(|transaction| {
            VERSIONED_COMMIT_LOG.insert(transaction, first.commit_sequence().value(), &relinked)
        })
        .expect("forged entry lands");

    let store = copy.open();
    let refusal = store
        .engine
        .rebuild_from_log(&Directory {
            thoughts: store.thoughts,
            tallies: store.tallies,
        })
        .expect_err("entry 7 no longer names the kept origin");
    assert!(matches!(
        refusal,
        Error::VersionedChainBroken { sequence: 7 }
    ));
}

/// The kept origin is bound by the checkpoint digest: changing it in the
/// stored checkpoint fails the checkpoint's verification.
#[test]
fn the_checkpoint_digest_binds_the_kept_origin() {
    let copy = LegacyCopy::of(COMPACTED);
    let conversion = copy.convert().expect("compacted store converts");
    let origin = conversion.kept_digests()[0];
    let kernel = copy.raw_kernel();
    let metadata = kernel
        .read(|transaction| CHECKPOINTS.get(transaction, 1))
        .expect("checkpoint reads")
        .expect("checkpoint is stored");
    let bytes = rkyv::to_bytes::<rancor::Error>(&metadata).expect("metadata serializes");
    let position = bytes
        .windows(32)
        .position(|window| window == origin.bytes())
        .expect("the origin is in the checkpoint");
    let mut doctored = bytes.to_vec();
    doctored[position] ^= 0x01;
    let doctored = rkyv::from_bytes::<memory_engine::CheckpointMetadata, rancor::Error>(&doctored)
        .expect("doctored metadata decodes");
    kernel
        .write(|transaction| CHECKPOINTS.insert(transaction, 1, &doctored))
        .expect("doctored checkpoint lands");
    drop(kernel);

    let refusal = Engine::open(copy.policy(VersionedRecoveryTopology::LocalCheckpoint))
        .expect("store opens")
        .latest_checkpoint()
        .expect_err("the doctored origin fails the checkpoint digest");
    assert!(matches!(refusal, Error::CheckpointDigestMismatch { .. }));
}

#[test]
fn an_engine_open_upgrades_a_sema_store_in_place_once() {
    let copy = LegacyCopy::in_place(ENTRIES);
    let store = copy.open();
    let conversion = store
        .engine
        .sema_conversion()
        .expect("the open converted the sema store");
    assert_eq!(conversion.entries(), 10);
    assert_eq!(conversion.checkpoints(), 0);
    store.assert_records();
    store.assert_chain();
    assert_eq!(store.rebuild(), 10);
    drop(store);

    let reopened = copy.open();
    assert_eq!(reopened.engine.sema_conversion(), None);
    reopened.assert_records();
    reopened.assert_chain();
    assert_eq!(reopened.rebuild(), 10);
}

#[test]
fn an_engine_open_upgrades_a_compacted_sema_store_with_its_kept_origin() {
    let copy = LegacyCopy::in_place(COMPACTED);
    let store = copy.open();
    let conversion = store
        .engine
        .sema_conversion()
        .expect("the open converted the sema store");
    assert_eq!(conversion.checkpoints(), 1);
    assert_eq!(conversion.kept_digests().len(), 1);
    store
        .engine
        .latest_checkpoint()
        .expect("converted checkpoint verifies")
        .expect("checkpoint exists");
}

#[test]
fn an_unversioned_file_that_is_no_sema_store_keeps_the_kernel_refusal() {
    let copy = LegacyCopy::in_place(&[]);
    let refusal = match Engine::open(EngineOpen::new(copy.path(), SchemaVersion::new(1))) {
        Ok(_) => panic!("an unversioned file that is no sema store is refused"),
        Err(refusal) => refusal,
    };
    assert!(matches!(
        refusal,
        Error::Memory(StorageKernelError::LegacyFileLacksSchema { .. })
    ));
}

#[test]
fn a_sema_store_at_another_layout_is_refused_and_stays_a_sema_store() {
    const SEMA_META: ::memory::Table<&'static str, u64> = ::memory::Table::new("__sema_meta");
    const SEMA_HEADERS: ::memory::Table<&'static str, u64> = ::memory::Table::new("__sema_headers");
    const SEMA_COUNTERS: ::memory::Table<&'static str, u64> =
        ::memory::Table::new("__sema_engine_counters");
    let copy = LegacyCopy::in_place(ENTRIES);
    // A sema store whose layout slot names 6: the kernel tables are renamed
    // back after the write, so the file stays a sema file.
    ::memory::SemaFile::open(&copy.path())
        .expect("fixture is a sema file")
        .convert(|transaction| {
            SEMA_COUNTERS.insert(transaction, "engine_storage_layout", &6)?;
            SEMA_META.rename_from(transaction, "__memory_meta")?;
            SEMA_HEADERS.rename_from(transaction, "__memory_headers")?;
            Ok(())
        })
        .expect("layout rewrites");

    let refusal = match Engine::open(copy.policy(VersionedRecoveryTopology::LocalCheckpoint)) {
        Ok(_) => panic!("a sema store at layout 6 is not converted"),
        Err(refusal) => refusal,
    };
    assert!(matches!(
        refusal,
        Error::StorageLayoutMismatch {
            stored: 6,
            expected: 7
        }
    ));
    ::memory::SemaFile::open(&copy.path()).expect("the refused file is still a sema file");
}

#[test]
fn an_open_that_refuses_a_store_it_upgraded_still_names_the_upgrade() {
    let copy = LegacyCopy::in_place(ENTRIES);
    // The fixture's writer stamped schema version 1; an opener at version 2
    // is refused by the kernel's version guard, after the conversion.
    let refusal = match Engine::open(EngineOpen::new(copy.path(), SchemaVersion::new(2))) {
        Ok(_) => panic!("a store written at schema version 1 is refused at version 2"),
        Err(refusal) => refusal,
    };
    let conversion = refusal
        .sema_conversion()
        .expect("the refusal names the conversion the open made first");
    assert_eq!(conversion.entries(), 10);
    assert!(matches!(
        refusal,
        Error::RefusedAfterSemaConversion { ref refusal, .. }
            if matches!(**refusal, Error::Memory(_))
    ));
    assert!(refusal.to_string().starts_with(
        "the store was upgraded from the sema format to the memory format, then refused: "
    ));

    // The file is a memory store now: the next open converts nothing.
    let reopened = copy.open();
    assert_eq!(reopened.engine.sema_conversion(), None);
    reopened.assert_records();
}

#[test]
fn a_refusal_with_no_upgrade_names_none() {
    let copy = LegacyCopy::in_place(&[]);
    let refusal = match Engine::open(EngineOpen::new(copy.path(), SchemaVersion::new(1))) {
        Ok(_) => panic!("an unversioned file that is no sema store is refused"),
        Err(refusal) => refusal,
    };
    assert_eq!(refusal.sema_conversion(), None);
}
