//! The engine's typed change path for a [`Memorable`] record.
//!
//! The record decides; the engine admits, reads, commits and answers.
//! The change's receipt lands under its change identity in the same
//! write transaction as the record, the commit log entries and the
//! counters, so a receipt exists exactly when the change is durable.

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
    CommitLogEntry, CommitLogOperation, DeltaKind, Engine, EngineStoredValue, Error, Memorable,
    Result,
};

use super::{
    ChangeEffect, ChangeIdentifier, ChangeOutcome, ChangeReceipt, Decision, MemoryChange,
    StorageFailure, StoragePhase,
};

/// Every committed change's receipt, keyed by its change identity.
const CHANGE_RECEIPTS: ::memory::Table<&'static [u8; 32], ChangeReceipt> =
    ::memory::Table::new("__memory_engine_change_receipts");

/// Committing a record's typed change and resolving it by its identity.
pub trait MemoryChanges {
    /// Apply one typed change: changed with the durable receipt, refused
    /// with the record's own reason and nothing written, or failed with a
    /// structured storage failure. A change whose identity already
    /// committed answers its recorded receipt and is not applied again.
    fn change<RecordValue>(
        &self,
        request: MemoryChange<RecordValue>,
    ) -> ChangeOutcome<RecordValue::Refusal>
    where
        RecordValue: Memorable + EngineStoredValue + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >;

    /// The receipt of the change committed under `change`, if any. This is
    /// how an uncertain outcome resolves: present means durable.
    fn change_receipt(&self, change: ChangeIdentifier) -> Result<Option<ChangeReceipt>>;
}

impl MemoryChanges for Engine {
    fn change<RecordValue>(
        &self,
        request: MemoryChange<RecordValue>,
    ) -> ChangeOutcome<RecordValue::Refusal>
    where
        RecordValue: Memorable + EngineStoredValue + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let write_guard = self.write_guard();
        if let Err(error) = self.admit_change(&request) {
            return ChangeOutcome::Failed(StorageFailure::new(StoragePhase::Admission, error));
        }
        let table = *request.table();
        let key = request.key().clone();
        let stored = self.storage.read(|transaction| {
            let receipt = CHANGE_RECEIPTS.get(transaction, request.identifier().bytes())?;
            let current = table
                .memory_table()
                .get(transaction, key.to_owned_string())?;
            Ok((receipt, current))
        });
        let current = match stored {
            Ok((Some(receipt), _)) => return ChangeOutcome::Changed(receipt),
            Ok((None, current)) => current,
            Err(error) => {
                return ChangeOutcome::Failed(StorageFailure::new(
                    StoragePhase::Read,
                    error.into(),
                ));
            }
        };
        let next = match RecordValue::change(current.as_ref(), request.change()) {
            Decision::Ready(next) => next,
            Decision::Refused(refusal) => return ChangeOutcome::Refused(refusal),
        };
        let effect = match current {
            None => ChangeEffect::Created,
            Some(_) => ChangeEffect::Replaced,
        };
        let receipt = match self.commit_change(&request, effect, &next) {
            Ok(receipt) => receipt,
            Err(error) => {
                return ChangeOutcome::Failed(StorageFailure::new(StoragePhase::Commit, error));
            }
        };
        let delta = match effect {
            ChangeEffect::Created => DeltaKind::Assert,
            ChangeEffect::Replaced => DeltaKind::Mutate,
        };
        self.subscriptions
            .deliver_delta(delta, *table.name(), &key, receipt.snapshot(), &next);
        drop(write_guard);
        match self.maintain_versioned_history() {
            Ok(()) => ChangeOutcome::Changed(receipt),
            Err(error) => {
                ChangeOutcome::Failed(StorageFailure::new(StoragePhase::AfterCommit, error))
            }
        }
    }

    fn change_receipt(&self, change: ChangeIdentifier) -> Result<Option<ChangeReceipt>> {
        Ok(self
            .storage
            .read(|transaction| CHANGE_RECEIPTS.get(transaction, change.bytes()))?)
    }
}

impl Engine {
    fn admit_change<RecordValue: Memorable>(
        &self,
        request: &MemoryChange<RecordValue>,
    ) -> Result<()> {
        self.ensure_registered(request.table())?;
        if self.staging_guard().is_some() {
            return Err(Error::StagedChangeUnsupported {
                table: request.table().name().as_str().to_owned(),
            });
        }
        Ok(())
    }

    /// Land the successor, its log entries, the counters and the change
    /// receipt in one write transaction.
    fn commit_change<RecordValue>(
        &self,
        request: &MemoryChange<RecordValue>,
        effect: ChangeEffect,
        next: &RecordValue,
    ) -> Result<ChangeReceipt>
    where
        RecordValue: Memorable + EngineStoredValue + Send + Sync + 'static,
        <RecordValue as rkyv::Archive>::Archived: rkyv::Deserialize<RecordValue, HighDeserializer<rancor::Error>>
            + for<'validation> CheckBytes<
                Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
            >,
    {
        let table = *request.table();
        let key = request.key().clone();
        let operation = match effect {
            ChangeEffect::Created => SemaOperation::Assert,
            ChangeEffect::Replaced => SemaOperation::Mutate,
        };
        let commit_sequence = self.next_commit_sequence()?;
        let snapshot = self.next_snapshot()?;
        let entry = CommitLogEntry::single(
            commit_sequence,
            snapshot,
            CommitLogOperation::new(operation, *table.name(), Some(key.clone())),
        );
        let versioned_entry = self.versioned_record_entry(
            commit_sequence,
            snapshot,
            operation,
            *table.name(),
            Some(key.clone()),
            next,
        )?;
        let receipt = ChangeReceipt::new(
            request.identifier(),
            effect,
            key.clone(),
            commit_sequence,
            snapshot,
        );
        let counts = self.log_counts()?;
        self.storage.write(|transaction| {
            table
                .memory_table()
                .insert(transaction, key.to_owned_string(), next)?;
            CommitLog::append_commit(transaction, &entry, counts.next_commit())?;
            self.insert_versioned_entry(transaction, &versioned_entry, counts.next_versioned())?;
            COUNTERS.insert(
                transaction,
                LATEST_COMMIT_SEQUENCE_KEY,
                &commit_sequence.value(),
            )?;
            COUNTERS.insert(transaction, LATEST_SNAPSHOT_KEY, &snapshot.value())?;
            CHANGE_RECEIPTS.insert(transaction, request.identifier().bytes(), &receipt)?;
            Ok(())
        })?;
        Ok(receipt)
    }
}
