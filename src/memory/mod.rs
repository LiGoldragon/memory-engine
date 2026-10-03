//! Memorable: the memory kind's two capabilities over a stored record type.
//!
//! A record type that is [`Memorable`] owns its typed change: the engine
//! hands it the stored record (or none) and the requested change, and the
//! record answers ready with its successor or refused with its own typed
//! reason. The engine then commits, and the caller receives one of three
//! distinguishable outcomes: changed with a durable receipt, refused
//! with the record's reason and no byte touched, or failed with a
//! structured storage failure.
//!
//! A record type that names a pinned predecessor definition implements
//! [`UpgradeFrom`] for it: the authored, fallible conversion that is the
//! very edit the type needs to reach its new memory format. The manifest
//! edge — the declared version naming its predecessor — is mapped to the
//! old and new per-family schema hashes the engine stores, through
//! [`MemoryDefinition`] on both definitions.

mod change;
mod upgrade;

use std::fmt;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::{
    CommitSequence, DatabaseMarker, Error, RecordKey, SchemaHash, SnapshotIdentifier,
    TableReference,
};

pub use change::MemoryChanges;
pub use upgrade::{MemoryUpgrades, UpgradeFault};

/// The authored version a manifest declares for one record definition.
/// A version is not a physical layout: the schema hash is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryVersion(u64);

impl MemoryVersion {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for MemoryVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "v{}", self.0)
    }
}

/// One record definition as its manifest declares it: the authored
/// version and the per-family schema hash its stored rows carry.
pub trait MemoryDefinition {
    fn memory_version() -> MemoryVersion;

    fn schema_hash() -> SchemaHash;

    fn declared_version() -> DeclaredVersion {
        DeclaredVersion::new(Self::memory_version(), Self::schema_hash())
    }
}

/// A declared version paired with the physical schema hash it maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeclaredVersion {
    version: MemoryVersion,
    schema_hash: SchemaHash,
}

impl DeclaredVersion {
    pub const fn new(version: MemoryVersion, schema_hash: SchemaHash) -> Self {
        Self {
            version,
            schema_hash,
        }
    }

    pub fn version(&self) -> MemoryVersion {
        self.version
    }

    pub fn schema_hash(&self) -> SchemaHash {
        self.schema_hash
    }
}

/// The manifest edge from a pinned predecessor definition to the current
/// one, mapped to the old and new schema hashes the engine stores.
/// Only a forward edge is admitted; downgrades are never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestEdge {
    predecessor: DeclaredVersion,
    current: DeclaredVersion,
}

impl ManifestEdge {
    pub fn declared<Predecessor, Current>() -> Self
    where
        Predecessor: MemoryDefinition,
        Current: MemoryDefinition,
    {
        Self {
            predecessor: Predecessor::declared_version(),
            current: Current::declared_version(),
        }
    }

    pub fn predecessor(&self) -> DeclaredVersion {
        self.predecessor
    }

    pub fn current(&self) -> DeclaredVersion {
        self.current
    }

    pub fn is_forward(&self) -> bool {
        self.predecessor.version < self.current.version
    }
}

/// A record's own answer to a change or an upgrade: its successor value,
/// or its typed reason for refusing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision<Value, Refusal> {
    Ready(Value),
    Refused(Refusal),
}

/// A stored record type that carries its standard typed change.
pub trait Memorable: Sized {
    /// The typed change this record accepts.
    type Change;
    /// The record's own reason for refusing a change.
    type Refusal;

    /// Decide the successor of `current` (none when no record is stored
    /// under the key) under `change`. Pure: nothing is durable until the
    /// engine commits the ready value.
    fn change(current: Option<&Self>, change: &Self::Change) -> Decision<Self, Self::Refusal>;
}

/// The authored, fallible conversion from a pinned predecessor definition.
pub trait UpgradeFrom<Predecessor>: MemoryDefinition + Sized
where
    Predecessor: MemoryDefinition,
{
    /// The record's own reason for refusing to upgrade a predecessor row.
    type Refusal;

    fn upgrade_from(predecessor: Predecessor) -> Decision<Self, Self::Refusal>;
}

/// The caller-chosen stable identity of one change. Operation names a
/// change before it is sent, so after an uncertain outcome it resolves
/// the receipt by this identity instead of replaying blindly; a change
/// whose identity already committed answers its recorded receipt.
#[derive(
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
)]
#[rkyv(derive(Debug))]
pub struct ChangeIdentifier([u8; 32]);

impl ChangeIdentifier {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn for_label(label: impl AsRef<[u8]>) -> Self {
        Self(*blake3::hash(label.as_ref()).as_bytes())
    }

    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One typed change addressed to one record.
pub struct MemoryChange<RecordValue: Memorable> {
    table: TableReference<RecordValue>,
    key: RecordKey,
    identifier: ChangeIdentifier,
    change: RecordValue::Change,
}

impl<RecordValue: Memorable> MemoryChange<RecordValue> {
    pub fn new(
        table: TableReference<RecordValue>,
        key: RecordKey,
        identifier: ChangeIdentifier,
        change: RecordValue::Change,
    ) -> Self {
        Self {
            table,
            key,
            identifier,
            change,
        }
    }

    pub fn table(&self) -> &TableReference<RecordValue> {
        &self.table
    }

    pub fn key(&self) -> &RecordKey {
        &self.key
    }

    pub fn identifier(&self) -> ChangeIdentifier {
        self.identifier
    }

    pub fn change(&self) -> &RecordValue::Change {
        &self.change
    }
}

/// What a committed change did to its record.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum ChangeEffect {
    /// No record was stored under the key; the change created one.
    Created,
    /// A stored record was replaced by its successor.
    Replaced,
}

/// The durable commit receipt of one change, stored under its change
/// identity in the same write transaction as the change itself.
#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct ChangeReceipt {
    change: ChangeIdentifier,
    effect: ChangeEffect,
    key: RecordKey,
    commit_sequence: u64,
    snapshot: u64,
}

impl ChangeReceipt {
    pub(crate) fn new(
        change: ChangeIdentifier,
        effect: ChangeEffect,
        key: RecordKey,
        commit_sequence: CommitSequence,
        snapshot: SnapshotIdentifier,
    ) -> Self {
        Self {
            change,
            effect,
            key,
            commit_sequence: commit_sequence.value(),
            snapshot: snapshot.value(),
        }
    }

    pub fn change(&self) -> ChangeIdentifier {
        self.change
    }

    pub fn effect(&self) -> ChangeEffect {
        self.effect
    }

    pub fn key(&self) -> &RecordKey {
        &self.key
    }

    pub fn commit_sequence(&self) -> CommitSequence {
        CommitSequence::new(self.commit_sequence)
    }

    pub fn snapshot(&self) -> SnapshotIdentifier {
        SnapshotIdentifier::new(self.snapshot)
    }

    pub fn marker(&self) -> DatabaseMarker {
        DatabaseMarker::new(self.commit_sequence(), self.snapshot())
    }
}

/// Where a storage failure struck, which says what is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoragePhase {
    /// Refused before reading: unregistered table, open staging group,
    /// undeclared or backward manifest edge. Nothing was written.
    Admission,
    /// Reading the stored state failed. Nothing was written.
    Read,
    /// The write transaction did not commit. Nothing was written.
    Commit,
    /// The write committed; work after it failed. The change is durable
    /// and its receipt resolves by its change identity.
    AfterCommit,
}

/// A structured storage failure: the phase it struck in and the engine
/// error that caused it. Never carries a record's domain refusal.
#[derive(Debug)]
pub struct StorageFailure {
    phase: StoragePhase,
    cause: Box<Error>,
}

impl StorageFailure {
    pub fn new(phase: StoragePhase, cause: Error) -> Self {
        Self {
            phase,
            cause: Box::new(cause),
        }
    }

    pub fn phase(&self) -> StoragePhase {
        self.phase
    }

    pub fn cause(&self) -> &Error {
        &self.cause
    }

    /// Whether the store is known unchanged by the failed effect.
    pub fn is_unchanged(&self) -> bool {
        !matches!(self.phase, StoragePhase::AfterCommit)
    }
}

/// The three distinguishable outcomes of one typed change.
#[derive(Debug)]
pub enum ChangeOutcome<Refusal> {
    Changed(ChangeReceipt),
    Refused(Refusal),
    Failed(StorageFailure),
}

/// One predecessor row the current definition refused to upgrade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeRefusal<Refusal> {
    key: RecordKey,
    reason: Refusal,
}

impl<Refusal> UpgradeRefusal<Refusal> {
    pub(crate) fn new(key: RecordKey, reason: Refusal) -> Self {
        Self { key, reason }
    }

    pub fn key(&self) -> &RecordKey {
        &self.key
    }

    pub fn reason(&self) -> &Refusal {
        &self.reason
    }
}

/// The durable evidence of one landed upgrade.
#[derive(Debug, Clone)]
pub struct UpgradeReceipt<RecordValue> {
    table: TableReference<RecordValue>,
    edge: ManifestEdge,
    rows: usize,
    commit_sequence: Option<CommitSequence>,
}

impl<RecordValue> UpgradeReceipt<RecordValue> {
    pub(crate) fn new(
        table: TableReference<RecordValue>,
        edge: ManifestEdge,
        rows: usize,
        commit_sequence: Option<CommitSequence>,
    ) -> Self {
        Self {
            table,
            edge,
            rows,
            commit_sequence,
        }
    }

    pub fn table(&self) -> TableReference<RecordValue> {
        self.table
    }

    pub fn edge(&self) -> ManifestEdge {
        self.edge
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The commit that carried the rows; none for an empty family,
    /// whose upgrade is a catalog-only rewrite.
    pub fn commit_sequence(&self) -> Option<CommitSequence> {
        self.commit_sequence
    }
}

/// The outcomes of upgrading one family across its manifest edge.
#[derive(Debug)]
pub enum UpgradeOutcome<RecordValue, Refusal> {
    /// Every predecessor row converted; rows and format marker landed together.
    Upgraded(UpgradeReceipt<RecordValue>),
    /// The store already holds the current definition, or none at all;
    /// the family is registered and nothing was converted.
    AlreadyCurrent(TableReference<RecordValue>),
    /// A predecessor row refused; the predecessor bytes and marker stand.
    Refused(UpgradeRefusal<Refusal>),
    Failed(StorageFailure),
}
