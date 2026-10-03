//! The engine's typed upgrade of a family across its manifest edge.
//!
//! The current definition converts each predecessor row through its
//! authored [`UpgradeFrom`]; the engine reads, converts every row before
//! writing any, and lands the converted rows with the family's format
//! marker (its catalog registration) in one write transaction. A refused
//! row or a failed commit leaves the predecessor bytes and marker as they
//! were. This is an eager, whole-family upgrade at registration time.

use rkyv::api::high::HighDeserializer;
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::{self, Strategy};
use rkyv::validation::Validator;
use rkyv::validation::archive::ArchiveValidator;
use rkyv::validation::shared::SharedValidator;

use crate::catalog::FamilyRegistration;
use crate::{
    Engine, EngineStoredValue, Error, ManifestEdge, MemoryDefinition, RecordKey, Result,
    TableDescriptor, TableReference, TableRegistration, UpgradeFrom,
};

use super::{
    Decision, StorageFailure, StoragePhase, UpgradeOutcome, UpgradeReceipt, UpgradeRefusal,
};

/// Deterministic interruption points inside an upgrade's durable write.
/// Test instrumentation, like `CompactionFault` and `OpenFault`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeFault {
    /// The converted rows are written; the log and format marker are not yet.
    AfterRowsWritten,
}

/// Upgrading a stored family from a pinned predecessor definition.
pub trait MemoryUpgrades {
    /// Upgrade the family `descriptor` declares from `Predecessor`, or
    /// register it when the store holds the current definition or none.
    fn upgrade_table<Predecessor, Current>(
        &mut self,
        descriptor: TableDescriptor<Current>,
    ) -> UpgradeOutcome<Current, <Current as UpgradeFrom<Predecessor>>::Refusal>
    where
        Predecessor: MemoryDefinition + EngineStoredValue + Send + Sync + 'static,
        <Predecessor as rkyv::Archive>::Archived: rkyv::Deserialize<Predecessor, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
        Current: UpgradeFrom<Predecessor> + EngineStoredValue + Send + Sync + 'static,
        <Current as rkyv::Archive>::Archived: rkyv::Deserialize<Current, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >;

    /// Interrupt the next upgrade's durable write at `fault`, as a crash
    /// there would.
    fn interrupt_upgrade_at(&self, fault: UpgradeFault);
}

impl MemoryUpgrades for Engine {
    fn upgrade_table<Predecessor, Current>(
        &mut self,
        descriptor: TableDescriptor<Current>,
    ) -> UpgradeOutcome<Current, <Current as UpgradeFrom<Predecessor>>::Refusal>
    where
        Predecessor: MemoryDefinition + EngineStoredValue + Send + Sync + 'static,
        <Predecessor as rkyv::Archive>::Archived: rkyv::Deserialize<Predecessor, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
        Current: UpgradeFrom<Predecessor> + EngineStoredValue + Send + Sync + 'static,
        <Current as rkyv::Archive>::Archived: rkyv::Deserialize<Current, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let failed = |phase, error| UpgradeOutcome::Failed(StorageFailure::new(phase, error));
        let edge = match Self::admitted_edge::<Predecessor, Current>(&descriptor) {
            Ok(edge) => edge,
            Err(error) => return failed(StoragePhase::Admission, error),
        };
        let name = *descriptor.name();
        let stored = match self.family_registration_state(&descriptor.family_identity()) {
            Ok(FamilyRegistration::Existing) => {
                return UpgradeOutcome::AlreadyCurrent(TableReference::new(name));
            }
            Ok(FamilyRegistration::New(_)) => {
                return match self.register_table(descriptor) {
                    Ok(table) => UpgradeOutcome::AlreadyCurrent(table),
                    Err(error) => failed(StoragePhase::Commit, error),
                };
            }
            Err(mismatch @ Error::FamilyIdentityMismatch { .. }) => {
                match self.registered_family(&name) {
                    Ok(stored) if descriptor.edge_from(&stored) == Some(edge) => stored,
                    Ok(_) => return failed(StoragePhase::Admission, mismatch),
                    Err(error) => return failed(StoragePhase::Admission, error),
                }
            }
            Err(error) => return failed(StoragePhase::Admission, error),
        };
        let predecessors = match self.storage.read(|transaction| {
            sema::Table::<String, Predecessor>::new(name.as_str()).iter(transaction)
        }) {
            Ok(rows) => rows,
            Err(error) => return failed(StoragePhase::Read, error.into()),
        };
        let mut carried = Vec::with_capacity(predecessors.len());
        for (key, predecessor) in predecessors {
            match Current::upgrade_from(predecessor) {
                Decision::Ready(current) => carried.push((key, current)),
                Decision::Refused(reason) => {
                    return UpgradeOutcome::Refused(UpgradeRefusal::new(
                        RecordKey::new(key),
                        reason,
                    ));
                }
            }
        }
        // The in-memory catalog moves first so the store schema hash stamped
        // into the versioned entry names the upgraded inventory; a failed
        // persist restores it, keeping memory equal to disk.
        let previous = TableRegistration::new(stored.clone());
        let upgraded = TableRegistration::new(descriptor.family_identity());
        if let Err(error) = self.catalog.evolve(name.as_str(), upgraded.clone()) {
            return failed(StoragePhase::Admission, error);
        }
        match self.persist_family_evolution(&descriptor, &stored, &carried, &upgraded) {
            Ok(commit_sequence) => UpgradeOutcome::Upgraded(UpgradeReceipt::new(
                TableReference::new(name),
                edge,
                carried.len(),
                commit_sequence,
            )),
            Err(error) => {
                if let Err(restore) = self.catalog.evolve(name.as_str(), previous) {
                    return failed(StoragePhase::Commit, restore);
                }
                failed(StoragePhase::Commit, error)
            }
        }
    }

    fn interrupt_upgrade_at(&self, fault: UpgradeFault) {
        *self
            .upgrade_fault
            .lock()
            .expect("upgrade fault lock poisoned") = Some(fault);
    }
}

impl Engine {
    /// The declared edge from `Predecessor` into `Current`, admitted only
    /// when it is forward and names the descriptor's own schema hash.
    fn admitted_edge<Predecessor, Current>(
        descriptor: &TableDescriptor<Current>,
    ) -> Result<ManifestEdge>
    where
        Predecessor: MemoryDefinition,
        Current: UpgradeFrom<Predecessor>,
    {
        let table = descriptor.name().as_str().to_owned();
        let Some(edge) = descriptor
            .predecessor_edges()
            .iter()
            .find(|edge| edge.predecessor() == Predecessor::declared_version())
            .copied()
        else {
            return Err(Error::PredecessorUndeclared {
                table,
                predecessor: Predecessor::memory_version(),
            });
        };
        if !edge.is_forward() {
            return Err(Error::ManifestEdgeBackward {
                table,
                predecessor: edge.predecessor().version(),
                current: edge.current().version(),
            });
        }
        if edge.current().schema_hash() != descriptor.schema_hash() {
            return Err(Error::ManifestEdgeMismatch {
                table,
                current: edge.current().version(),
            });
        }
        Ok(edge)
    }

    pub(crate) fn take_upgrade_fault(&self) -> Option<UpgradeFault> {
        self.upgrade_fault
            .lock()
            .expect("upgrade fault lock poisoned")
            .take()
    }
}
