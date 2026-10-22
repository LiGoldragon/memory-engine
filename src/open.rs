//! What an open asks of a store: its path, schema version, versioning
//! policy, and the record families the component declares up front, so
//! the engine can stamp the store and register those families in the one
//! write transaction that makes the open durable.

use std::path::{Path, PathBuf};

use ::memory::{Schema, SchemaVersion};

use crate::{
    FamilyIdentity, IdentifiedTableDescriptor, SchemaHash, TableDescriptor, TableName,
    VersioningPolicy,
};

#[derive(Debug, Clone)]
pub struct EngineOpen {
    path: PathBuf,
    schema: Schema,
    versioning_policy: Option<VersioningPolicy>,
    families: Vec<FamilyDeclaration>,
    fault: Option<OpenFault>,
}

impl EngineOpen {
    pub fn new(path: impl Into<PathBuf>, version: SchemaVersion) -> Self {
        Self {
            path: path.into(),
            schema: Schema { version },
            versioning_policy: None,
            families: Vec::new(),
            fault: None,
        }
    }

    pub fn with_versioning(mut self, policy: VersioningPolicy) -> Self {
        self.versioning_policy = Some(policy);
        self
    }

    /// Declare a record family the component will register. A declared
    /// family is validated against the stored catalog before the open
    /// writes anything, and a new one is registered in the same write
    /// transaction as the store's layout stamp and versioning policy, so
    /// an open that fails on a family leaves the store unstamped.
    pub fn with_family(mut self, family: &impl DeclaresFamily) -> Self {
        self.families.push(family.family_declaration());
        self
    }

    /// Interrupt the open at a named point inside its durable write, as a
    /// crash there would. Test instrumentation, like `CompactionFault`.
    pub fn interrupted_at(mut self, fault: OpenFault) -> Self {
        self.fault = Some(fault);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn versioning_policy(&self) -> Option<&VersioningPolicy> {
        self.versioning_policy.as_ref()
    }

    pub fn families(&self) -> &[FamilyDeclaration] {
        &self.families
    }

    pub fn fault(&self) -> Option<OpenFault> {
        self.fault
    }
}

/// Deterministic interruption points inside an open's durable write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFault {
    /// The layout stamp is written; the versioning policy and declared
    /// families are not yet.
    AfterLayoutStamped,
}

/// How a declared family's rows are keyed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyKeying {
    /// Rows carry their own domain key.
    Domain,
    /// The engine mints numeric identifiers; registration seeds the counter.
    Identified,
}

/// One record family as an open declares it: the identity it registers
/// under, how its rows are keyed, and the schema hashes of the predecessor
/// definitions it upgrades from (a stored predecessor is left for
/// `MemoryUpgrades::upgrade_table` to upgrade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyDeclaration {
    table: TableName,
    identity: FamilyIdentity,
    keying: FamilyKeying,
    priors: Vec<SchemaHash>,
}

impl FamilyDeclaration {
    pub fn new(
        table: TableName,
        identity: FamilyIdentity,
        keying: FamilyKeying,
        priors: Vec<SchemaHash>,
    ) -> Self {
        Self {
            table,
            identity,
            keying,
            priors,
        }
    }

    pub fn table(&self) -> TableName {
        self.table
    }

    pub fn identity(&self) -> &FamilyIdentity {
        &self.identity
    }

    pub fn keying(&self) -> FamilyKeying {
        self.keying
    }

    /// Whether a stored identity at this family's table is a predecessor
    /// definition this declaration upgrades from.
    pub fn evolves_from(&self, stored: &FamilyIdentity) -> bool {
        stored.family() == self.identity.family() && self.priors.contains(&stored.schema_hash())
    }
}

/// A table descriptor that can declare its family to an open.
pub trait DeclaresFamily {
    fn family_declaration(&self) -> FamilyDeclaration;
}

impl<RecordValue> DeclaresFamily for TableDescriptor<RecordValue> {
    fn family_declaration(&self) -> FamilyDeclaration {
        FamilyDeclaration::new(
            *self.name(),
            self.family_identity(),
            FamilyKeying::Domain,
            self.predecessor_schema_hashes(),
        )
    }
}

impl<RecordValue> DeclaresFamily for IdentifiedTableDescriptor<RecordValue> {
    fn family_declaration(&self) -> FamilyDeclaration {
        FamilyDeclaration::new(
            *self.name(),
            self.family_identity(),
            FamilyKeying::Identified,
            Vec::new(),
        )
    }
}
