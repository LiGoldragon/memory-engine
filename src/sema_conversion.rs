//! Converting a store sema-engine wrote into the memory format.
//!
//! sema-engine 0.15 (storage layout 7, over the `sema` kernel) stored the
//! same rows this engine stores. Two things differ: every engine table is
//! named `__sema_engine_*` where this engine names it `__memory_engine_*`
//! (and the kernel's two tables `__sema_*` where the kernel now names them
//! `__memory_*`), and five content hashes — the store schema hash, the
//! versioned entry digest, the checkpoint segment digest, the checkpoint
//! digest and the view digest — mix in `sema-engine-…` labels where this
//! engine mixes in `memory-engine-…`. The conversion is the edit those differences
//! need, done in one write transaction:
//!
//! - every table takes its memory name;
//! - every versioned entry gets the memory form of its store schema hash
//!   and digest, and names its predecessor's new digest;
//! - every checkpoint gets new segment digests, view digest, schema hash,
//!   covered entry digest and checkpoint digest, and names its
//!   predecessor checkpoint's new digest.
//!
//! A digest compaction already cut off — a checkpoint's covered entry
//! digest naming a deleted entry, and the back-link of the first entry
//! after it — names nothing the conversion can recompute. It is kept in
//! its sema form as the chain's origin, bound by the converted checkpoint
//! digest; the fold takes a checkpoint's covered digest as given and
//! never recomputes it.
//!
//! Before writing, the store is verified under the sema labels, so a
//! damaged store is refused rather than re-signed. Every refusal happens
//! before the file is opened for writing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ::memory::{ReadTransaction, SemaFile, Table, WriteTransaction};

use crate::checkpoint::{CheckpointMetadata, CheckpointSegment, SegmentReference};
use crate::compaction::CompactionIntent;
use crate::fold::{CanonicalView, ChainOrigin};
use crate::outbox::{MirrorHead, OutboxEntry};
use crate::staging::StagedOperationGroup;
use crate::versioning::DigestNamespace;
use crate::{
    Checkpoint, CheckpointDigest, EntryDigest, Error, FamilyIdentity, Result, SegmentDigest,
    StoreSchemaHash, TableRegistration, VersionedCommitLogEntry,
};

const SEMA_CATALOG: Table<&'static str, TableRegistration> = Table::new("__sema_engine_catalog");
const SEMA_COUNTERS: Table<&'static str, u64> = Table::new("__sema_engine_counters");
const SEMA_VERSIONED_COMMIT_LOG: Table<u64, VersionedCommitLogEntry> =
    Table::new("__sema_engine_versioned_commit_log");
const SEMA_CHAIN_HEAD: Table<&'static str, EntryDigest> = Table::new("__sema_engine_chain_head");
const SEMA_CHECKPOINTS: Table<u64, CheckpointMetadata> = Table::new("__sema_engine_checkpoints");
const SEMA_CHECKPOINT_SEGMENTS: Table<&'static [u8; 32], CheckpointSegment> =
    Table::new("__sema_engine_checkpoint_segments");
const SEMA_OUTBOX: Table<u64, OutboxEntry> = Table::new("__sema_engine_outbox");
const SEMA_MIRROR_CURSOR: Table<&'static str, MirrorHead> =
    Table::new("__sema_engine_mirror_cursor");
const SEMA_STAGING_SLOT: Table<&'static str, StagedOperationGroup> =
    Table::new("__sema_engine_staging_slot");
const SEMA_COMPACTION_INTENT: Table<&'static str, CompactionIntent> =
    Table::new("__sema_engine_compaction_intent");
const SEMA_COMMIT_LOG: &str = "__sema_engine_commit_log";
const SEMA_SUBSCRIPTIONS: &str = "__sema_engine_subscriptions";
const SEMA_IDENTIFIED_COUNTERS: &str = "__sema_engine_identified_counters";
const SEMA_VERSIONING_POLICY: &str = "__sema_engine_versioning_policy";

/// A store file sema-engine wrote, to be converted in place into the
/// memory format. The caller converts a copy and puts it in place only
/// when [`Self::convert`] succeeds.
pub struct LegacySemaStore {
    path: PathBuf,
}

impl LegacySemaStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify the store under the sema labels, compute every converted
    /// row, then rename and rewrite in one write transaction. A store
    /// that is not a sema file, fails its own verification, holds an
    /// entry whose schema hash no family list it holds derives
    /// ([`Error::SemaSchemaHashUnmatched`]), or holds unsettled staging
    /// ([`Error::SemaStagingPending`]) is refused without a write.
    pub fn convert(self) -> Result<SemaConversion> {
        let file = SemaFile::open(&self.path)?;
        let stored = file.read(SemaRows::read)?;
        let converted = stored.converted()?;
        file.convert(|transaction| converted.write(transaction))?;
        Ok(converted.receipt())
    }
}

/// What a conversion rewrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemaConversion {
    entries: usize,
    checkpoints: usize,
    segments: usize,
    kept_digests: Vec<EntryDigest>,
}

impl SemaConversion {
    /// Versioned entries rewritten.
    pub fn entries(&self) -> usize {
        self.entries
    }

    /// Checkpoints rewritten.
    pub fn checkpoints(&self) -> usize {
        self.checkpoints
    }

    /// Checkpoint segments re-keyed.
    pub fn segments(&self) -> usize {
        self.segments
    }

    /// The entry digests compaction had cut off, kept in their sema form
    /// as chain origins.
    pub fn kept_digests(&self) -> &[EntryDigest] {
        &self.kept_digests
    }
}

/// The rows of a sema store the conversion reads, as stored.
struct SemaRows {
    families: Vec<FamilyIdentity>,
    latest_checkpoint: Option<u64>,
    entries: Vec<VersionedCommitLogEntry>,
    chain_head: Option<EntryDigest>,
    checkpoints: Vec<CheckpointMetadata>,
    segments: Vec<([u8; 32], CheckpointSegment)>,
    outbox: Vec<OutboxEntry>,
    mirror_head: Option<MirrorHead>,
    staging_pending: bool,
}

impl SemaRows {
    fn read(transaction: &ReadTransaction) -> ::memory::Result<Self> {
        Ok(Self {
            families: SEMA_CATALOG
                .iter(transaction)?
                .into_iter()
                .map(|(_table, registration)| registration.identity().clone())
                .collect(),
            latest_checkpoint: SEMA_COUNTERS
                .get(transaction, crate::engine::LATEST_CHECKPOINT_KEY)?,
            entries: SEMA_VERSIONED_COMMIT_LOG
                .iter(transaction)?
                .into_iter()
                .map(|(_sequence, entry)| entry)
                .collect(),
            chain_head: SEMA_CHAIN_HEAD
                .get(transaction, crate::commit_log::LATEST_ENTRY_DIGEST_KEY)?,
            checkpoints: SEMA_CHECKPOINTS
                .iter(transaction)?
                .into_iter()
                .map(|(_sequence, metadata)| metadata)
                .collect(),
            segments: SEMA_CHECKPOINT_SEGMENTS.iter(transaction)?,
            outbox: SEMA_OUTBOX
                .iter(transaction)?
                .into_iter()
                .map(|(_sequence, entry)| entry)
                .collect(),
            mirror_head: SEMA_MIRROR_CURSOR.get(transaction, crate::outbox::MIRROR_SHIPPED_KEY)?,
            staging_pending: !SEMA_STAGING_SLOT.iter(transaction)?.is_empty()
                || !SEMA_COMPACTION_INTENT.iter(transaction)?.is_empty(),
        })
    }

    fn converted(self) -> Result<ConvertedRows> {
        if self.staging_pending {
            return Err(Error::SemaStagingPending);
        }
        for (key, segment) in &self.segments {
            let computed = segment.digest_in(DigestNamespace::Sema);
            if computed.bytes() != key {
                return Err(Error::SegmentDigestMismatch {
                    referenced: SegmentDigest::new(*key),
                    computed,
                });
            }
        }
        let segments = SegmentRekeying::new(&self.segments);
        for metadata in &self.checkpoints {
            Checkpoint::new(metadata.clone(), segments.sema_segments(metadata)?)
                .verify_in(DigestNamespace::Sema)?;
            let derived = StoreSchemaHash::of_families(
                metadata.family_inventory().families(),
                DigestNamespace::Sema,
            );
            if derived != metadata.store_schema_hash() {
                return Err(Error::CheckpointSchemaMismatch {
                    checkpoint: metadata.store_schema_hash(),
                    current: derived,
                });
            }
        }
        let schemas = SchemaHashes::new(&self.families, &self.checkpoints);
        let present: BTreeMap<u64, EntryDigest> = self
            .entries
            .iter()
            .map(|entry| (entry.commit_sequence().value(), entry.entry_digest()))
            .collect();
        let mut kept_digests = Vec::new();
        for metadata in &self.checkpoints {
            let covered = metadata.covered().last().value();
            match present.get(&covered) {
                Some(digest) if *digest != metadata.covered_entry_digest() => {
                    return Err(Error::VersionedChainBroken { sequence: covered });
                }
                Some(_) => {}
                None => {
                    if !kept_digests.contains(&metadata.covered_entry_digest()) {
                        kept_digests.push(metadata.covered_entry_digest());
                    }
                }
            }
        }

        let mut digests: BTreeMap<EntryDigest, EntryDigest> = BTreeMap::new();
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut previous: Option<(EntryDigest, EntryDigest)> = None;
        for entry in &self.entries {
            let sequence = entry.commit_sequence().value();
            let recomputed = EntryDigest::from_entry_fields(
                DigestNamespace::Sema,
                entry.store_name(),
                entry.schema_hash(),
                entry.commit_sequence(),
                entry.snapshot(),
                entry.previous_entry_digest(),
                entry.operations(),
            );
            if recomputed != entry.entry_digest() {
                return Err(Error::VersionedChainBroken { sequence });
            }
            let predecessor = match (previous, entry.previous_entry_digest()) {
                (Some((sema, memory)), Some(named)) if named == sema => Some(memory),
                (None, None) => None,
                (None, Some(named)) if kept_digests.contains(&named) => Some(named),
                _ => return Err(Error::VersionedChainBroken { sequence }),
            };
            let schema_hash = schemas.converted(entry)?;
            let converted = VersionedCommitLogEntry::new(
                entry.store_name().clone(),
                schema_hash,
                entry.commit_sequence(),
                entry.snapshot(),
                predecessor,
                entry.operations().clone(),
            );
            digests.insert(entry.entry_digest(), converted.entry_digest());
            previous = Some((entry.entry_digest(), converted.entry_digest()));
            entries.push(converted);
        }
        let chain_head = match (entries.last(), self.chain_head) {
            (Some(last), _) => Some(last.entry_digest()),
            (None, head) => head,
        };

        let mut checkpoint_digests: BTreeMap<CheckpointDigest, CheckpointDigest> = BTreeMap::new();
        let mut checkpoints = Vec::with_capacity(self.checkpoints.len());
        for metadata in &self.checkpoints {
            let rows = segments.sema_segments(metadata)?;
            let view = CanonicalView::fold(
                &Checkpoint::new(metadata.clone(), rows).rows(),
                &[],
                ChainOrigin::Genesis,
            )?;
            let covered_entry_digest = digests
                .get(&metadata.covered_entry_digest())
                .copied()
                .unwrap_or(metadata.covered_entry_digest());
            let previous_checkpoint_digest = metadata
                .previous_checkpoint_digest()
                .map(|digest| checkpoint_digests.get(&digest).copied().unwrap_or(digest));
            let converted = CheckpointMetadata::new(
                metadata.sequence(),
                metadata.store_name().clone(),
                StoreSchemaHash::of_families(
                    metadata.family_inventory().families(),
                    DigestNamespace::Memory,
                ),
                metadata.family_inventory().clone(),
                metadata.covered(),
                metadata.covered_snapshot(),
                covered_entry_digest,
                view.digest(),
                previous_checkpoint_digest,
                segments.memory_references(metadata)?,
            );
            checkpoint_digests.insert(metadata.checkpoint_digest(), converted.checkpoint_digest());
            checkpoints.push(converted);
        }

        let by_sequence: BTreeMap<u64, &VersionedCommitLogEntry> = entries
            .iter()
            .map(|entry| (entry.commit_sequence().value(), entry))
            .collect();
        let mut outbox = Vec::new();
        for row in &self.outbox {
            let sequence = row.commit_sequence().value();
            if let Some(entry) = by_sequence.get(&sequence) {
                if present.get(&sequence) != Some(&row.entry_digest()) {
                    return Err(Error::OutboxEntryMismatch { sequence });
                }
                outbox.push(OutboxEntry::from(*entry));
            }
        }
        // A mirror head past compacted history names nothing to convert
        // and stays as recorded.
        let mut mirror_head = None;
        if let Some(head) = self.mirror_head {
            let sequence = head.commit_sequence().value();
            if let (Some(entry), Some(recorded)) =
                (by_sequence.get(&sequence), present.get(&sequence))
            {
                if *recorded != head.entry_digest() {
                    return Err(Error::MirrorHeadForked {
                        sequence,
                        recorded: *recorded,
                        acknowledged: head.entry_digest(),
                    });
                }
                mirror_head = Some(MirrorHead::new(
                    entry.commit_sequence(),
                    entry.entry_digest(),
                ));
            }
        }

        let converted = ConvertedRows {
            entries,
            chain_head,
            checkpoints,
            sema_segment_keys: self.segments.iter().map(|(key, _segment)| *key).collect(),
            segments: self
                .segments
                .into_iter()
                .map(|(_key, segment)| segment)
                .collect(),
            outbox,
            mirror_head,
            kept_digests,
        };
        converted.verify(self.latest_checkpoint)?;
        Ok(converted)
    }
}

/// The sema store schema hash of every family list the store holds — its
/// catalog and each checkpoint's inventory — mapped to the memory hash of
/// the same list. A store schema hash is not reversible; an entry's list
/// is found by matching.
struct SchemaHashes {
    memory_by_sema: BTreeMap<StoreSchemaHash, StoreSchemaHash>,
}

impl SchemaHashes {
    fn new(catalog: &[FamilyIdentity], checkpoints: &[CheckpointMetadata]) -> Self {
        let lists = std::iter::once(catalog).chain(
            checkpoints
                .iter()
                .map(|metadata| metadata.family_inventory().families()),
        );
        Self {
            memory_by_sema: lists
                .map(|families| {
                    (
                        StoreSchemaHash::of_families(families, DigestNamespace::Sema),
                        StoreSchemaHash::of_families(families, DigestNamespace::Memory),
                    )
                })
                .collect(),
        }
    }

    fn converted(&self, entry: &VersionedCommitLogEntry) -> Result<StoreSchemaHash> {
        self.memory_by_sema
            .get(&entry.schema_hash())
            .copied()
            .ok_or(Error::SemaSchemaHashUnmatched {
                sequence: entry.commit_sequence().value(),
                schema_hash: entry.schema_hash(),
            })
    }
}

/// The store's checkpoint segments under their sema keys and their
/// memory digests.
struct SegmentRekeying<'rows> {
    by_sema: BTreeMap<SegmentDigest, (&'rows CheckpointSegment, SegmentDigest)>,
}

impl<'rows> SegmentRekeying<'rows> {
    fn new(segments: &'rows [([u8; 32], CheckpointSegment)]) -> Self {
        Self {
            by_sema: segments
                .iter()
                .map(|(key, segment)| (SegmentDigest::new(*key), (segment, segment.digest())))
                .collect(),
        }
    }

    fn sema_segments(&self, metadata: &CheckpointMetadata) -> Result<Vec<CheckpointSegment>> {
        metadata
            .segments()
            .iter()
            .map(|reference| {
                self.by_sema
                    .get(&reference.digest())
                    .map(|(segment, _memory)| (*segment).clone())
                    .ok_or(Error::SegmentMissing {
                        digest: reference.digest(),
                    })
            })
            .collect()
    }

    fn memory_references(&self, metadata: &CheckpointMetadata) -> Result<Vec<SegmentReference>> {
        metadata
            .segments()
            .iter()
            .map(|reference| {
                self.by_sema
                    .get(&reference.digest())
                    .map(|(_segment, memory)| SegmentReference::new(*memory, reference.row_count()))
                    .ok_or(Error::SegmentMissing {
                        digest: reference.digest(),
                    })
            })
            .collect()
    }
}

/// Every row the conversion writes, computed and verified before the
/// write transaction opens.
struct ConvertedRows {
    entries: Vec<VersionedCommitLogEntry>,
    chain_head: Option<EntryDigest>,
    checkpoints: Vec<CheckpointMetadata>,
    sema_segment_keys: Vec<[u8; 32]>,
    segments: Vec<CheckpointSegment>,
    outbox: Vec<OutboxEntry>,
    mirror_head: Option<MirrorHead>,
    kept_digests: Vec<EntryDigest>,
}

impl ConvertedRows {
    /// Check the converted rows the way the engine will: every
    /// checkpoint verifies, and the log folds from the latest
    /// checkpoint's origin.
    fn verify(&self, latest_checkpoint: Option<u64>) -> Result<()> {
        for metadata in &self.checkpoints {
            Checkpoint::new(metadata.clone(), self.segments_of(metadata)?).verify()?;
        }
        let latest = match latest_checkpoint {
            Some(sequence) => Some(
                self.checkpoints
                    .iter()
                    .find(|metadata| metadata.sequence().value() == sequence)
                    .ok_or(Error::CheckpointRowMissing { sequence })?,
            ),
            None => None,
        };
        let rows = match latest {
            Some(metadata) => Checkpoint::new(metadata.clone(), self.segments_of(metadata)?).rows(),
            None => Vec::new(),
        };
        let after = latest.map_or(0, |metadata| metadata.covered().last().value());
        let suffix: Vec<VersionedCommitLogEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.commit_sequence().value() > after)
            .cloned()
            .collect();
        CanonicalView::fold(&rows, &suffix, ChainOrigin::after(latest))?;
        Ok(())
    }

    fn segments_of(&self, metadata: &CheckpointMetadata) -> Result<Vec<CheckpointSegment>> {
        metadata
            .segments()
            .iter()
            .map(|reference| {
                self.segments
                    .iter()
                    .find(|segment| segment.digest() == reference.digest())
                    .cloned()
                    .ok_or(Error::SegmentMissing {
                        digest: reference.digest(),
                    })
            })
            .collect()
    }

    fn write(&self, transaction: &WriteTransaction) -> ::memory::Result<()> {
        use crate::commit_log::VERSIONED_COMMIT_LOG;
        use crate::commit_log::{CHAIN_HEAD, COMMIT_LOG, LATEST_ENTRY_DIGEST_KEY};
        use crate::engine::{
            CATALOG, CHECKPOINT_SEGMENTS, CHECKPOINTS, COMPACTION_INTENT, COUNTERS,
            IDENTIFIED_COUNTERS, SUBSCRIPTIONS, VERSIONING_POLICY,
        };
        use crate::outbox::{MIRROR_CURSOR, MIRROR_SHIPPED_KEY, OUTBOX};
        use crate::staging::STAGING_SLOT;

        CATALOG.rename_from(transaction, SEMA_CATALOG.name())?;
        COUNTERS.rename_from(transaction, SEMA_COUNTERS.name())?;
        COMMIT_LOG.rename_from(transaction, SEMA_COMMIT_LOG)?;
        VERSIONED_COMMIT_LOG.rename_from(transaction, SEMA_VERSIONED_COMMIT_LOG.name())?;
        CHAIN_HEAD.rename_from(transaction, SEMA_CHAIN_HEAD.name())?;
        SUBSCRIPTIONS.rename_from(transaction, SEMA_SUBSCRIPTIONS)?;
        IDENTIFIED_COUNTERS.rename_from(transaction, SEMA_IDENTIFIED_COUNTERS)?;
        CHECKPOINTS.rename_from(transaction, SEMA_CHECKPOINTS.name())?;
        CHECKPOINT_SEGMENTS.rename_from(transaction, SEMA_CHECKPOINT_SEGMENTS.name())?;
        VERSIONING_POLICY.rename_from(transaction, SEMA_VERSIONING_POLICY)?;
        COMPACTION_INTENT.rename_from(transaction, SEMA_COMPACTION_INTENT.name())?;
        OUTBOX.rename_from(transaction, SEMA_OUTBOX.name())?;
        MIRROR_CURSOR.rename_from(transaction, SEMA_MIRROR_CURSOR.name())?;
        STAGING_SLOT.rename_from(transaction, SEMA_STAGING_SLOT.name())?;

        for entry in &self.entries {
            VERSIONED_COMMIT_LOG.insert(transaction, entry.commit_sequence().value(), entry)?;
        }
        if let Some(head) = &self.chain_head {
            CHAIN_HEAD.insert(transaction, LATEST_ENTRY_DIGEST_KEY, head)?;
        }
        for key in &self.sema_segment_keys {
            CHECKPOINT_SEGMENTS.remove(transaction, key)?;
        }
        for segment in &self.segments {
            CHECKPOINT_SEGMENTS.insert(transaction, segment.digest().bytes(), segment)?;
        }
        for metadata in &self.checkpoints {
            CHECKPOINTS.insert(transaction, metadata.sequence().value(), metadata)?;
        }
        for row in &self.outbox {
            OUTBOX.insert(transaction, row.commit_sequence().value(), row)?;
        }
        if let Some(head) = &self.mirror_head {
            MIRROR_CURSOR.insert(transaction, MIRROR_SHIPPED_KEY, head)?;
        }
        Ok(())
    }

    fn receipt(&self) -> SemaConversion {
        SemaConversion {
            entries: self.entries.len(),
            checkpoints: self.checkpoints.len(),
            segments: self.segments.len(),
            kept_digests: self.kept_digests.clone(),
        }
    }
}
