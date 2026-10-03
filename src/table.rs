use std::marker::PhantomData;

use crate::{
    FamilyIdentity, FamilyName, KeyedAssertion, ManifestEdge, MemoryDefinition, QueryPlan,
    RecordIdentifier, RecordKey, Result, SchemaHash, UpgradeFrom,
};

/// Key suffix for one identified table's durable next-record-identifier
/// counter row. Checkpoint inventories reuse it so an imported store
/// restores the exact counter keys the original engine wrote.
pub(crate) const IDENTIFIED_COUNTER_SUFFIX: &str = "next_record_identifier";

/// The generated declaration of one domain-keyed Sema table.
///
/// Implementations provide only structural data: the current redb coordinate,
/// stable family identity, schema identity, stored record type, and authored
/// key type. The provided constructors are the sole shared lowering from that
/// declaration into the engine API.
pub trait TableSpecification {
    /// The value stored in the table.
    type Record;

    /// The source-declared type that addresses one record.
    ///
    /// This carries no blanket archive-to-string conversion.  The generated
    /// declaration below owns the exact key projection that its source
    /// archive contract declares.
    type Key;

    /// Current redb table coordinate.
    const TABLE_NAME: TableName;

    /// Stable encoded family identity; independent of the current spelling.
    const FAMILY_NAME: &'static str;

    /// Content identity of the table declaration's record/key shape.
    const SCHEMA_HASH: SchemaHash;

    /// Build the descriptor registered by the owning component's engine.
    fn descriptor() -> TableDescriptor<Self::Record> {
        TableDescriptor::new(
            Self::TABLE_NAME,
            FamilyName::new(Self::FAMILY_NAME),
            Self::SCHEMA_HASH,
        )
    }

    /// Build the typed table coordinate used by read and write requests.
    fn table_reference() -> TableReference<Self::Record> {
        TableReference::new(Self::TABLE_NAME)
    }

    /// Project one source-declared key into its durable domain coordinate.
    ///
    /// Implementors must provide this explicitly.  The engine deliberately
    /// has no generic rkyv-key encoder: archive bytes are evidence of a value
    /// layout, not an unlicensed replacement for its source key semantics.
    fn record_key(key: &Self::Key) -> Result<RecordKey>;

    /// Build a keyed assertion whose key type is fixed by this declaration.
    fn assertion(key: &Self::Key, record: Self::Record) -> Result<KeyedAssertion<Self::Record>> {
        Ok(KeyedAssertion::new(
            Self::table_reference(),
            Self::record_key(key)?,
            record,
        ))
    }

    /// Build an exact-key query whose key type is fixed by this declaration.
    fn query(key: &Self::Key) -> Result<QueryPlan<Self::Record>> {
        Ok(QueryPlan::key(
            Self::table_reference(),
            Self::record_key(key)?,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableName {
    value: &'static str,
}

impl TableName {
    pub const fn new(value: &'static str) -> Self {
        Self { value }
    }

    pub fn as_str(&self) -> &'static str {
        self.value
    }

    pub fn identified_counter_key(&self) -> String {
        format!("{}:{IDENTIFIED_COUNTER_SUFFIX}", self.value)
    }
}

impl From<TableName> for String {
    fn from(name: TableName) -> Self {
        name.as_str().to_owned()
    }
}

/// Declaration of a domain-keyed record family: the current table
/// coordinate plus the typed family identity the engine persists in
/// its catalog and stamps into every versioned log operation. A
/// declaration may additionally name its pinned predecessor definitions —
/// the manifest edges the engine may upgrade an existing store across —
/// via [`Self::with_predecessor`].
#[derive(Debug, Clone)]
pub struct TableDescriptor<RecordValue> {
    name: TableName,
    family: FamilyName,
    schema_hash: SchemaHash,
    predecessors: Vec<ManifestEdge>,
    record: PhantomData<RecordValue>,
}

#[derive(Debug, Clone)]
pub struct IdentifiedTableDescriptor<RecordValue> {
    name: TableName,
    family: FamilyName,
    schema_hash: SchemaHash,
    record: PhantomData<RecordValue>,
}

impl<RecordValue> TableDescriptor<RecordValue> {
    pub fn new(name: TableName, family: FamilyName, schema_hash: SchemaHash) -> Self {
        Self {
            name,
            family,
            schema_hash,
            predecessors: Vec::new(),
            record: PhantomData,
        }
    }

    /// Name a pinned predecessor definition this one upgrades from: the
    /// manifest edge, mapped to the predecessor's and this definition's
    /// schema hashes. A store holding the predecessor opens, and upgrades
    /// through [`crate::MemoryUpgrades::upgrade_table`] with the authored
    /// [`UpgradeFrom`] conversion; an undeclared stored identity keeps
    /// failing closed.
    pub fn with_predecessor<Predecessor>(mut self) -> Self
    where
        Predecessor: MemoryDefinition,
        RecordValue: UpgradeFrom<Predecessor>,
    {
        self.predecessors
            .push(ManifestEdge::declared::<Predecessor, RecordValue>());
        self
    }

    pub fn name(&self) -> &TableName {
        &self.name
    }

    pub fn family(&self) -> &FamilyName {
        &self.family
    }

    pub fn schema_hash(&self) -> SchemaHash {
        self.schema_hash
    }

    pub fn family_identity(&self) -> FamilyIdentity {
        FamilyIdentity::new(self.family.clone(), self.schema_hash, self.name)
    }

    /// Every declared manifest edge into this definition.
    pub fn predecessor_edges(&self) -> &[ManifestEdge] {
        &self.predecessors
    }

    /// The schema hashes of every declared predecessor definition.
    pub fn predecessor_schema_hashes(&self) -> Vec<SchemaHash> {
        self.predecessors
            .iter()
            .map(|edge| edge.predecessor().schema_hash())
            .collect()
    }

    /// The declared edge from a stored catalog identity: same family name,
    /// and a declared predecessor whose schema hash the store carries. A
    /// stored identity from a different family never matches — a table
    /// coordinate reused by another family is a genuine incompatibility.
    pub fn edge_from(&self, stored: &FamilyIdentity) -> Option<ManifestEdge> {
        if stored.family() != &self.family {
            return None;
        }
        self.predecessors
            .iter()
            .find(|edge| edge.predecessor().schema_hash() == stored.schema_hash())
            .copied()
    }
}

impl<RecordValue> IdentifiedTableDescriptor<RecordValue> {
    pub fn new(name: TableName, family: FamilyName, schema_hash: SchemaHash) -> Self {
        Self {
            name,
            family,
            schema_hash,
            record: PhantomData,
        }
    }

    pub fn name(&self) -> &TableName {
        &self.name
    }

    pub fn family(&self) -> &FamilyName {
        &self.family
    }

    pub fn schema_hash(&self) -> SchemaHash {
        self.schema_hash
    }

    pub fn family_identity(&self) -> FamilyIdentity {
        FamilyIdentity::new(self.family.clone(), self.schema_hash, self.name)
    }
}

#[derive(Debug)]
pub struct TableReference<RecordValue> {
    name: TableName,
    record: PhantomData<RecordValue>,
}

#[derive(Debug)]
pub struct IdentifiedTableReference<RecordValue> {
    name: TableName,
    record: PhantomData<RecordValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedRecord<RecordValue> {
    identifier: RecordIdentifier,
    value: RecordValue,
}

impl<RecordValue> TableReference<RecordValue> {
    pub const fn new(name: TableName) -> Self {
        Self {
            name,
            record: PhantomData,
        }
    }

    pub fn name(&self) -> &TableName {
        &self.name
    }

    pub fn sema_table(&self) -> sema::Table<String, RecordValue> {
        sema::Table::new(self.name.as_str())
    }
}

impl<RecordValue> IdentifiedTableReference<RecordValue> {
    pub const fn new(name: TableName) -> Self {
        Self {
            name,
            record: PhantomData,
        }
    }

    pub fn name(&self) -> &TableName {
        &self.name
    }

    pub fn sema_table(&self) -> sema::Table<u64, RecordValue> {
        sema::Table::new(self.name.as_str())
    }
}

impl<RecordValue> IdentifiedRecord<RecordValue> {
    pub fn new(identifier: RecordIdentifier, value: RecordValue) -> Self {
        Self { identifier, value }
    }

    pub fn identifier(&self) -> RecordIdentifier {
        self.identifier
    }

    pub fn value(&self) -> &RecordValue {
        &self.value
    }

    pub fn into_value(self) -> RecordValue {
        self.value
    }
}

impl<RecordValue> Clone for TableReference<RecordValue> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<RecordValue> Copy for TableReference<RecordValue> {}

impl<RecordValue> Clone for IdentifiedTableReference<RecordValue> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<RecordValue> Copy for IdentifiedTableReference<RecordValue> {}
