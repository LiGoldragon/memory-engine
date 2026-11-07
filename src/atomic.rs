//! Heterogeneous typed writes that share one engine transaction.
//!
//! A group is deliberately opaque: callers can add records from any registered
//! table, while the engine retains the typed table handles needed to materialize
//! every operation in the same SEMA write transaction.

use std::collections::HashSet;

use crate::non_empty::NonEmpty;
use rkyv::api::high::HighDeserializer;
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::{self, Strategy};
use rkyv::validation::Validator;
use rkyv::validation::archive::ArchiveValidator;
use rkyv::validation::shared::SharedValidator;
use signal_sema::SemaOperation;

use crate::commit_log::CommitLog;
use crate::engine::{COUNTERS, LATEST_COMMIT_SEQUENCE_KEY, LATEST_SNAPSHOT_KEY};
use crate::{
    CommitLogEntry, CommitLogOperation, CommitSequence, DeltaKind, Engine, EngineStoredRecord,
    EngineStoredValue, Error, RecordKey, Result, SnapshotIdentifier, TableName, TableReference,
    VersionedLogOperation, VersionedPayload,
};

/// A heterogeneous write group. Construct it with [`Engine::begin_atomic_commit`].
pub struct AtomicCommit {
    writes: Vec<Box<dyn AtomicWrite>>,
}

impl AtomicCommit {
    pub(crate) fn new() -> Self {
        Self { writes: Vec::new() }
    }

    pub fn assert<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        record: RecordValue,
    ) -> Self
    where
        RecordValue: EngineStoredRecord + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let key = record.record_key();
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Assert,
            table,
            key,
            Some(record),
        )));
        self
    }

    pub fn mutate<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        record: RecordValue,
    ) -> Self
    where
        RecordValue: EngineStoredRecord + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let key = record.record_key();
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Mutate,
            table,
            key,
            Some(record),
        )));
        self
    }

    pub fn retract<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        key: RecordKey,
    ) -> Self
    where
        RecordValue: EngineStoredValue + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Retract,
            table,
            key,
            None,
        )));
        self
    }

    pub fn operation_count(&self) -> usize {
        self.writes.len()
    }
}

/// Receipt for one heterogeneous commit. All listed effects share this marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtomicCommitReceipt {
    commit_sequence: CommitSequence,
    snapshot: SnapshotIdentifier,
    operation_count: usize,
}

impl AtomicCommitReceipt {
    pub(crate) fn new(
        commit_sequence: CommitSequence,
        snapshot: SnapshotIdentifier,
        operation_count: usize,
    ) -> Self {
        Self {
            commit_sequence,
            snapshot,
            operation_count,
        }
    }

    pub fn commit_sequence(&self) -> CommitSequence {
        self.commit_sequence
    }
    pub fn snapshot(&self) -> SnapshotIdentifier {
        self.snapshot
    }
    pub fn operation_count(&self) -> usize {
        self.operation_count
    }
}

#[derive(Clone, Copy)]
enum AtomicKind {
    Assert,
    Mutate,
    Retract,
}

impl AtomicKind {
    fn operation(self) -> SemaOperation {
        match self {
            Self::Assert => SemaOperation::Assert,
            Self::Mutate => SemaOperation::Mutate,
            Self::Retract => SemaOperation::Retract,
        }
    }

    fn delta(self) -> DeltaKind {
        match self {
            Self::Assert => DeltaKind::Assert,
            Self::Mutate => DeltaKind::Mutate,
            Self::Retract => DeltaKind::Retract,
        }
    }
}

trait AtomicWrite: Send + Sync {
    fn table(&self) -> TableName;
    fn key(&self) -> &RecordKey;
    fn prepare(&self, engine: &Engine) -> Result<PreparedAtomicWrite>;
    fn materialize(&self, transaction: &sema::WriteTransaction) -> sema::Result<()>;
}

struct TypedAtomicWrite<RecordValue> {
    kind: AtomicKind,
    table: TableReference<RecordValue>,
    key: RecordKey,
    record: Option<RecordValue>,
}

impl<RecordValue> TypedAtomicWrite<RecordValue> {
    fn new(
        kind: AtomicKind,
        table: TableReference<RecordValue>,
        key: RecordKey,
        record: Option<RecordValue>,
    ) -> Self {
        Self {
            kind,
            table,
            key,
            record,
        }
    }
}

impl<RecordValue> AtomicWrite for TypedAtomicWrite<RecordValue>
where
    RecordValue: EngineStoredValue + Send + Sync + 'static,
    <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
{
    fn table(&self) -> TableName {
        *self.table.name()
    }
    fn key(&self) -> &RecordKey {
        &self.key
    }
    fn prepare(&self, engine: &Engine) -> Result<PreparedAtomicWrite> {
        engine.ensure_atomic_registered(&self.table)?;
        let prior = engine.atomic_read(&self.table, &self.key)?;
        match (self.kind, prior.as_ref()) {
            (AtomicKind::Assert, Some(_)) => {
                return Err(engine.atomic_duplicate_assert(&self.table, &self.key));
            }
            (AtomicKind::Mutate | AtomicKind::Retract, None) => {
                return Err(engine.atomic_missing(&self.table, &self.key));
            }
            _ => {}
        }
        let payload = match self.kind {
            AtomicKind::Retract => VersionedPayload::tombstone(),
            AtomicKind::Assert | AtomicKind::Mutate => engine.atomic_payload(
                *self.table.name(),
                self.record.as_ref().expect("record-bearing write"),
            )?,
        };
        let delta_record = match self.kind {
            AtomicKind::Retract => prior.expect("validated present retract"),
            AtomicKind::Assert | AtomicKind::Mutate => {
                self.record.as_ref().expect("record-bearing write").clone()
            }
        };
        Ok(PreparedAtomicWrite {
            operation: CommitLogOperation::new(
                self.kind.operation(),
                *self.table.name(),
                Some(self.key.clone()),
            ),
            versioned: engine.atomic_versioned_operation(
                self.kind.operation(),
                *self.table.name(),
                self.key.clone(),
                payload,
            )?,
            delta: Box::new(TypedAtomicDelta {
                kind: self.kind.delta(),
                table: *self.table.name(),
                key: self.key.clone(),
                record: delta_record,
            }),
        })
    }

    fn materialize(&self, transaction: &sema::WriteTransaction) -> sema::Result<()> {
        match self.kind {
            AtomicKind::Assert | AtomicKind::Mutate => self.table.sema_table().insert(
                transaction,
                self.key.to_owned_string(),
                self.record.as_ref().expect("record-bearing write"),
            ),
            AtomicKind::Retract => {
                self.table
                    .sema_table()
                    .remove(transaction, self.key.to_owned_string())?;
                Ok(())
            }
        }
    }
}

struct PreparedAtomicWrite {
    operation: CommitLogOperation,
    versioned: Option<VersionedLogOperation>,
    delta: Box<dyn AtomicDelta>,
}

trait AtomicDelta {
    fn announce(&self, engine: &Engine, snapshot: SnapshotIdentifier);
}

struct TypedAtomicDelta<RecordValue> {
    kind: DeltaKind,
    table: TableName,
    key: RecordKey,
    record: RecordValue,
}

impl<RecordValue> AtomicDelta for TypedAtomicDelta<RecordValue>
where
    RecordValue: EngineStoredValue + Send + Sync + 'static,
    <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
{
    fn announce(&self, engine: &Engine, snapshot: SnapshotIdentifier) {
        engine.atomic_announce(self.kind, self.table, &self.key, snapshot, &self.record);
    }
}

impl Engine {
    /// Begin a transaction that may contain writes for multiple registered tables.
    pub fn begin_atomic_commit(&self) -> AtomicCommit {
        AtomicCommit::new()
    }

    /// Preflight and materialize a heterogeneous group in one SEMA transaction.
    /// Deltas are delivered only after that transaction commits.
    pub fn commit_atomic(&self, commit: AtomicCommit) -> Result<AtomicCommitReceipt> {
        let _write_guard = self.write_guard();
        if commit.writes.is_empty() {
            return Err(Error::EmptyAtomicCommit);
        }
        let mut coordinates = HashSet::new();
        let mut prepared = Vec::with_capacity(commit.writes.len());
        for write in &commit.writes {
            let coordinate = (write.table().as_str().to_owned(), write.key().clone());
            if !coordinates.insert(coordinate) {
                return Err(Error::DuplicateWriteKey {
                    table: write.table().as_str().to_owned(),
                    key: write.key().to_owned_string(),
                });
            }
            prepared.push(write.prepare(self)?);
        }
        let commit_sequence = self.next_commit_sequence()?;
        let snapshot = self.next_snapshot()?;
        let operations =
            NonEmpty::try_from_vec(prepared.iter().map(|item| item.operation.clone()).collect())
                .map_err(|_| Error::EmptyAtomicCommit)?;
        let entry = CommitLogEntry::new(commit_sequence, snapshot, operations);
        let versioned_operations: Vec<_> = prepared
            .iter()
            .filter_map(|item| item.versioned.clone())
            .collect();
        let versioned_entry = if self.versioning_policy.is_some() {
            self.versioned_entry(
                commit_sequence,
                snapshot,
                NonEmpty::try_from_vec(versioned_operations)
                    .map_err(|_| Error::EmptyAtomicCommit)?,
            )?
        } else {
            None
        };
        let counts = self.log_counts()?;
        self.storage.write(|transaction| {
            for write in &commit.writes {
                write.materialize(transaction)?;
            }
            CommitLog::append_commit(transaction, &entry, counts.next_commit())?;
            self.insert_versioned_entry(transaction, &versioned_entry, counts.next_versioned())?;
            COUNTERS.insert(
                transaction,
                LATEST_COMMIT_SEQUENCE_KEY,
                &commit_sequence.value(),
            )?;
            COUNTERS.insert(transaction, LATEST_SNAPSHOT_KEY, &snapshot.value())?;
            Ok(())
        })?;
        for item in &prepared {
            item.delta.announce(self, snapshot);
        }
        let receipt = AtomicCommitReceipt::new(commit_sequence, snapshot, prepared.len());
        drop(_write_guard);
        self.maintain_versioned_history()?;
        Ok(receipt)
    }
}

/// A typed snapshot of one predecessor family, read before a heterogeneous
/// Memory evolution is prepared. The catalog identity is retained so commit
/// can reject a stale plan before writing.
pub struct PredecessorInventory<RecordValue> {
    table: TableName,
    identity: crate::FamilyIdentity,
    rows: Vec<(RecordKey, RecordValue)>,
}

impl<RecordValue> PredecessorInventory<RecordValue> {
    pub fn table(&self) -> TableName {
        self.table
    }

    pub fn identity(&self) -> &crate::FamilyIdentity {
        &self.identity
    }

    pub fn rows(&self) -> &[(RecordKey, RecordValue)] {
        &self.rows
    }
}

/// One startup Memory edit spanning any number of evolved, new, and unchanged
/// families. It is committed by [`Engine::commit_atomic_evolution`] in one
/// storage-kernel transaction.
pub struct AtomicEvolution {
    families: Vec<Box<dyn EvolutionFamily>>,
    writes: Vec<Box<dyn AtomicWrite>>,
}

impl AtomicEvolution {
    fn new() -> Self {
        Self {
            families: Vec::new(),
            writes: Vec::new(),
        }
    }

    pub fn evolve<Predecessor, Current>(
        mut self,
        inventory: PredecessorInventory<Predecessor>,
        descriptor: crate::TableDescriptor<Current>,
        records: Vec<Current>,
    ) -> Self
    where
        Predecessor: crate::MemoryDefinition + EngineStoredValue + Send + Sync + 'static,
        <Predecessor as rkyv::Archive>::Archived: rkyv::Deserialize<Predecessor, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
        Current: crate::MemoryDefinition + EngineStoredRecord + Send + Sync + 'static,
        <Current as rkyv::Archive>::Archived: rkyv::Deserialize<Current, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        self.families.push(Box::new(TypedEvolutionFamily {
            inventory,
            descriptor,
            records,
        }));
        self
    }

    pub fn register<RecordValue>(
        mut self,
        descriptor: crate::TableDescriptor<RecordValue>,
        records: Vec<RecordValue>,
    ) -> Self
    where
        RecordValue: crate::MemoryDefinition + EngineStoredRecord + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        self.families.push(Box::new(TypedNewFamily {
            descriptor,
            records,
        }));
        self
    }

    pub fn assert<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        record: RecordValue,
    ) -> Self
    where
        RecordValue: EngineStoredRecord + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let key = record.record_key();
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Assert,
            table,
            key,
            Some(record),
        )));
        self
    }

    pub fn mutate<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        record: RecordValue,
    ) -> Self
    where
        RecordValue: EngineStoredRecord + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let key = record.record_key();
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Mutate,
            table,
            key,
            Some(record),
        )));
        self
    }

    pub fn retract<RecordValue>(
        mut self,
        table: TableReference<RecordValue>,
        key: RecordKey,
    ) -> Self
    where
        RecordValue: EngineStoredValue + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        self.writes.push(Box::new(TypedAtomicWrite::new(
            AtomicKind::Retract,
            table,
            key,
            None,
        )));
        self
    }
}

struct PreparedEvolutionFamily {
    table: TableName,
    registration: crate::TableRegistration,
    operations: Vec<CommitLogOperation>,
    versioned: Vec<VersionedLogOperation>,
    deltas: Vec<Box<dyn AtomicDelta>>,
}

trait EvolutionFamily: Send + Sync {
    fn table(&self) -> TableName;
    fn prepare(&self, engine: &Engine) -> Result<PreparedEvolutionFamily>;
    fn materialize(&self, transaction: &sema::WriteTransaction) -> sema::Result<()>;
}

struct TypedEvolutionFamily<Predecessor, Current> {
    inventory: PredecessorInventory<Predecessor>,
    descriptor: crate::TableDescriptor<Current>,
    records: Vec<Current>,
}

impl<Predecessor, Current> EvolutionFamily for TypedEvolutionFamily<Predecessor, Current>
where
    Predecessor: crate::MemoryDefinition + EngineStoredValue + Send + Sync + 'static,
    <Predecessor as rkyv::Archive>::Archived: rkyv::Deserialize<Predecessor, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
    Current: crate::MemoryDefinition + EngineStoredRecord + Send + Sync + 'static,
    <Current as rkyv::Archive>::Archived: rkyv::Deserialize<Current, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
{
    fn table(&self) -> TableName {
        *self.descriptor.name()
    }

    fn prepare(&self, engine: &Engine) -> Result<PreparedEvolutionFamily> {
        let table = *self.descriptor.name();
        if table != self.inventory.table {
            return Err(Error::TableNotRegistered {
                table: table.as_str().to_owned(),
            });
        }
        let stored = engine.registered_family(&table)?;
        if stored != self.inventory.identity {
            return Err(Error::FamilyIdentityMismatch {
                table: table.as_str().to_owned(),
                stored: stored.to_string(),
                declared: self.inventory.identity.to_string(),
            });
        }
        let Some(_edge) = self.descriptor.edge_from(&stored) else {
            return Err(Error::PredecessorUndeclared {
                table: table.as_str().to_owned(),
                predecessor: Predecessor::memory_version(),
            });
        };
        let registration = crate::TableRegistration::new(self.descriptor.family_identity());
        let mut keys = HashSet::new();
        let mut operations = Vec::with_capacity(self.inventory.rows.len() + self.records.len());
        let mut versioned = Vec::with_capacity(operations.capacity());
        for (key, _) in &self.inventory.rows {
            operations.push(CommitLogOperation::new(
                SemaOperation::Retract,
                table,
                Some(key.clone()),
            ));
            if engine.versioning_policy.is_some() {
                versioned.push(VersionedLogOperation::new(
                    SemaOperation::Retract,
                    stored.clone(),
                    Some(key.clone()),
                    VersionedPayload::tombstone(),
                ));
            }
        }
        let mut deltas: Vec<Box<dyn AtomicDelta>> = Vec::with_capacity(self.records.len());
        for record in &self.records {
            let key = record.record_key();
            if !keys.insert(key.clone()) {
                return Err(Error::DuplicateWriteKey {
                    table: table.as_str().to_owned(),
                    key: key.to_owned_string(),
                });
            }
            operations.push(CommitLogOperation::new(
                SemaOperation::Assert,
                table,
                Some(key.clone()),
            ));
            if engine.versioning_policy.is_some() {
                versioned.push(VersionedLogOperation::new(
                    SemaOperation::Assert,
                    registration.identity().clone(),
                    Some(key.clone()),
                    engine.atomic_payload(table, record)?,
                ));
            }
            deltas.push(Box::new(TypedAtomicDelta {
                kind: DeltaKind::Assert,
                table,
                key,
                record: record.clone(),
            }));
        }
        Ok(PreparedEvolutionFamily {
            table,
            registration,
            operations,
            versioned,
            deltas,
        })
    }

    fn materialize(&self, transaction: &sema::WriteTransaction) -> sema::Result<()> {
        let table = self.descriptor.name().as_str();
        for (key, _) in &self.inventory.rows {
            sema::Table::<String, Predecessor>::new(table)
                .remove(transaction, key.to_owned_string())?;
        }
        for record in &self.records {
            sema::Table::<String, Current>::new(table).insert(
                transaction,
                record.record_key().to_owned_string(),
                record,
            )?;
        }
        Ok(())
    }
}

struct TypedNewFamily<RecordValue> {
    descriptor: crate::TableDescriptor<RecordValue>,
    records: Vec<RecordValue>,
}

impl<RecordValue> EvolutionFamily for TypedNewFamily<RecordValue>
where
    RecordValue: crate::MemoryDefinition + EngineStoredRecord + Send + Sync + 'static,
    <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
{
    fn table(&self) -> TableName {
        *self.descriptor.name()
    }

    fn prepare(&self, engine: &Engine) -> Result<PreparedEvolutionFamily> {
        let table = *self.descriptor.name();
        let registration =
            match engine.family_registration_state(&self.descriptor.family_identity())? {
                crate::catalog::FamilyRegistration::New(value) => value,
                crate::catalog::FamilyRegistration::Existing => {
                    return Err(Error::TableAlreadyRegistered {
                        table: table.as_str().to_owned(),
                    });
                }
            };
        let mut keys = HashSet::new();
        let mut operations = Vec::with_capacity(self.records.len());
        let mut versioned = Vec::with_capacity(self.records.len());
        let mut deltas: Vec<Box<dyn AtomicDelta>> = Vec::with_capacity(self.records.len());
        for record in &self.records {
            let key = record.record_key();
            if !keys.insert(key.clone()) {
                return Err(Error::DuplicateWriteKey {
                    table: table.as_str().to_owned(),
                    key: key.to_owned_string(),
                });
            }
            operations.push(CommitLogOperation::new(
                SemaOperation::Assert,
                table,
                Some(key.clone()),
            ));
            if engine.versioning_policy.is_some() {
                versioned.push(VersionedLogOperation::new(
                    SemaOperation::Assert,
                    registration.identity().clone(),
                    Some(key.clone()),
                    engine.atomic_payload(table, record)?,
                ));
            }
            deltas.push(Box::new(TypedAtomicDelta {
                kind: DeltaKind::Assert,
                table,
                key,
                record: record.clone(),
            }));
        }
        Ok(PreparedEvolutionFamily {
            table,
            registration,
            operations,
            versioned,
            deltas,
        })
    }

    fn materialize(&self, transaction: &sema::WriteTransaction) -> sema::Result<()> {
        let table = self.descriptor.name().as_str();
        for record in &self.records {
            sema::Table::<String, RecordValue>::new(table).insert(
                transaction,
                record.record_key().to_owned_string(),
                record,
            )?;
        }
        Ok(())
    }
}

impl Engine {
    pub fn inspect_predecessor<Predecessor, Current>(
        &self,
        descriptor: &crate::TableDescriptor<Current>,
    ) -> Result<PredecessorInventory<Predecessor>>
    where
        Predecessor: crate::MemoryDefinition + EngineStoredValue + Send + Sync + 'static,
        <Predecessor as rkyv::Archive>::Archived: rkyv::Deserialize<Predecessor, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
        Current: crate::MemoryDefinition,
    {
        let table = *descriptor.name();
        let stored = self.registered_family(&table)?;
        if descriptor.edge_from(&stored).is_none() {
            return Err(Error::PredecessorUndeclared {
                table: table.as_str().to_owned(),
                predecessor: Predecessor::memory_version(),
            });
        }
        if stored.schema_hash() != Predecessor::declared_version().schema_hash() {
            return Err(Error::FamilyIdentityMismatch {
                table: table.as_str().to_owned(),
                stored: stored.to_string(),
                declared: format!(
                    "{}@{}",
                    stored.family().as_str(),
                    Predecessor::declared_version().schema_hash()
                ),
            });
        }
        let rows = self.storage.read(|transaction| {
            sema::Table::<String, Predecessor>::new(table.as_str()).iter(transaction)
        })?;
        Ok(PredecessorInventory {
            table,
            identity: stored,
            rows: rows
                .into_iter()
                .map(|(key, value)| (RecordKey::new(key), value))
                .collect(),
        })
    }

    pub fn begin_atomic_evolution(&self) -> AtomicEvolution {
        AtomicEvolution::new()
    }

    pub fn commit_atomic_evolution(
        &mut self,
        evolution: AtomicEvolution,
    ) -> Result<AtomicCommitReceipt> {
        if evolution.families.is_empty() {
            return Err(Error::EmptyAtomicCommit);
        }
        let mut tables = HashSet::new();
        let mut prepared_families = Vec::with_capacity(evolution.families.len());
        for family in &evolution.families {
            if !tables.insert(family.table().as_str().to_owned()) {
                return Err(Error::TableAlreadyRegistered {
                    table: family.table().as_str().to_owned(),
                });
            }
            prepared_families.push(family.prepare(self)?);
        }
        for write in &evolution.writes {
            if tables.contains(write.table().as_str()) {
                return Err(Error::DuplicateWriteKey {
                    table: write.table().as_str().to_owned(),
                    key: write.key().to_owned_string(),
                });
            }
        }
        let mut coordinates = HashSet::new();
        let mut prepared_writes = Vec::with_capacity(evolution.writes.len());
        for write in &evolution.writes {
            let coordinate = (write.table().as_str().to_owned(), write.key().clone());
            if !coordinates.insert(coordinate) {
                return Err(Error::DuplicateWriteKey {
                    table: write.table().as_str().to_owned(),
                    key: write.key().to_owned_string(),
                });
            }
            prepared_writes.push(write.prepare(self)?);
        }
        let mut planned_catalog = self.catalog.clone();
        for family in &prepared_families {
            if planned_catalog.is_registered(&family.table) {
                planned_catalog.evolve(family.table.as_str(), family.registration.clone())?;
            } else {
                planned_catalog.insert(family.registration.clone())?;
            }
        }
        let operations: Vec<_> = prepared_families
            .iter()
            .flat_map(|family| family.operations.iter().cloned())
            .chain(prepared_writes.iter().map(|write| write.operation.clone()))
            .collect();
        let operation_count = operations.len();
        let operations =
            NonEmpty::try_from_vec(operations).map_err(|_| Error::EmptyAtomicCommit)?;
        let versioned_operations: Vec<_> = prepared_families
            .iter()
            .flat_map(|family| family.versioned.iter().cloned())
            .chain(
                prepared_writes
                    .iter()
                    .filter_map(|write| write.versioned.clone()),
            )
            .collect();
        let commit_sequence = self.next_commit_sequence()?;
        let snapshot = self.next_snapshot()?;
        let entry = CommitLogEntry::new(commit_sequence, snapshot, operations);
        let versioned_entry = if self.versioning_policy.is_some() {
            self.versioned_entry_with_schema(
                commit_sequence,
                snapshot,
                crate::StoreSchemaHash::from(&planned_catalog),
                NonEmpty::try_from_vec(versioned_operations)
                    .map_err(|_| Error::EmptyAtomicCommit)?,
            )?
        } else {
            None
        };
        let counts = self.log_counts()?;
        let fault = self.take_upgrade_fault();
        self.storage.write(|transaction| {
            for family in &evolution.families {
                family.materialize(transaction)?;
            }
            for write in &evolution.writes {
                write.materialize(transaction)?;
            }
            if let Some(fault) = fault {
                return Err(Error::UpgradeInterrupted { fault }.into_storage());
            }
            for family in &prepared_families {
                crate::engine::CATALOG.insert(
                    transaction,
                    family.table.as_str(),
                    &family.registration,
                )?;
            }
            CommitLog::append_commit(transaction, &entry, counts.next_commit())?;
            self.insert_versioned_entry(transaction, &versioned_entry, counts.next_versioned())?;
            COUNTERS.insert(
                transaction,
                LATEST_COMMIT_SEQUENCE_KEY,
                &commit_sequence.value(),
            )?;
            COUNTERS.insert(transaction, LATEST_SNAPSHOT_KEY, &snapshot.value())?;
            Ok(())
        })?;
        self.catalog = planned_catalog;
        for family in &prepared_families {
            for delta in &family.deltas {
                delta.announce(self, snapshot);
            }
        }
        for write in &prepared_writes {
            write.delta.announce(self, snapshot);
        }
        self.maintain_versioned_history()?;
        Ok(AtomicCommitReceipt::new(
            commit_sequence,
            snapshot,
            operation_count,
        ))
    }
}
