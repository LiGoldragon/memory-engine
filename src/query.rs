use std::{collections::BTreeMap, sync::Arc};

use signal_sema::SemaOperation;

use rkyv::api::high::HighDeserializer;
use rkyv::bytecheck::CheckBytes;
use rkyv::rancor::{self, Strategy};
use rkyv::validation::Validator;
use rkyv::validation::archive::ArchiveValidator;
use rkyv::validation::shared::SharedValidator;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

use crate::{
    DatabaseMarker, IdentifiedRecord, IdentifiedTableReference, RecordIdentifier, RecordKey,
    SnapshotIdentifier, TableName, TableReference,
};

#[derive(Debug, Clone)]
pub struct QueryPlan<RecordValue> {
    table: TableReference<RecordValue>,
    filter: QueryFilter,
    read_plan: ReadPlan<RecordValue>,
}

#[derive(Debug, Clone)]
pub struct IdentifiedQueryPlan<RecordValue> {
    table: IdentifiedTableReference<RecordValue>,
    read_plan: IdentifiedReadPlan,
}

impl<RecordValue> QueryPlan<RecordValue> {
    pub fn all(table: TableReference<RecordValue>) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::all_rows(),
        }
    }

    pub fn key(table: TableReference<RecordValue>, key: RecordKey) -> Self {
        Self {
            table,
            filter: QueryFilter::Key(key.clone()),
            read_plan: ReadPlan::by_key(key),
        }
    }

    pub fn key_range(table: TableReference<RecordValue>, range: KeyRange) -> Self {
        Self {
            table,
            filter: QueryFilter::KeyRange(range.clone()),
            read_plan: ReadPlan::by_key_range(range),
        }
    }

    /// Every row of the table that the typed predicate admits.
    pub fn filtered(
        table: TableReference<RecordValue>,
        predicate: impl RecordPredicate<RecordValue> + 'static,
    ) -> Self {
        Self::all(table).filtered_by(predicate)
    }

    /// Narrow this plan's rows to those the typed predicate admits. The
    /// key filter a subscription matches deltas by is unchanged, which is
    /// why a filtered plan is a read, never a subscription.
    pub fn filtered_by(self, predicate: impl RecordPredicate<RecordValue> + 'static) -> Self {
        Self {
            table: self.table,
            filter: self.filter,
            read_plan: ReadPlan::filter(self.read_plan, RecordFilter::new(predicate)),
        }
    }

    pub fn constrain(table: TableReference<RecordValue>, unify: UnificationPlan) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::constrain(unify),
        }
    }

    pub fn project(table: TableReference<RecordValue>, fields: FieldSelection) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::project(ReadPlan::all_rows(), fields),
        }
    }

    pub fn aggregate(table: TableReference<RecordValue>, reducer: AggregatePlan) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::aggregate(ReadPlan::all_rows(), reducer),
        }
    }

    pub fn infer(table: TableReference<RecordValue>, rules: RuleSetRef) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::infer(ReadPlan::all_rows(), rules),
        }
    }

    pub fn recurse(table: TableReference<RecordValue>, mode: RecursionMode) -> Self {
        Self {
            table,
            filter: QueryFilter::All,
            read_plan: ReadPlan::recurse(ReadPlan::all_rows(), mode),
        }
    }

    pub fn table(&self) -> &TableReference<RecordValue> {
        &self.table
    }

    pub fn filter(&self) -> &QueryFilter {
        &self.filter
    }

    pub fn read_plan(&self) -> &ReadPlan<RecordValue> {
        &self.read_plan
    }
}

impl<RecordValue> IdentifiedQueryPlan<RecordValue> {
    pub fn all(table: IdentifiedTableReference<RecordValue>) -> Self {
        Self {
            table,
            read_plan: IdentifiedReadPlan::all_rows(),
        }
    }

    pub fn identifier(
        table: IdentifiedTableReference<RecordValue>,
        identifier: RecordIdentifier,
    ) -> Self {
        Self {
            table,
            read_plan: IdentifiedReadPlan::by_identifier(identifier),
        }
    }

    pub fn identifier_range(
        table: IdentifiedTableReference<RecordValue>,
        range: RecordIdentifierRange,
    ) -> Self {
        Self {
            table,
            read_plan: IdentifiedReadPlan::by_identifier_range(range),
        }
    }

    pub fn table(&self) -> &IdentifiedTableReference<RecordValue> {
        &self.table
    }

    pub fn read_plan(&self) -> &IdentifiedReadPlan {
        &self.read_plan
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub enum QueryFilter {
    All,
    Key(RecordKey),
    KeyRange(KeyRange),
}

impl QueryFilter {
    pub fn accepts(&self, key: &RecordKey) -> bool {
        match self {
            Self::All => true,
            Self::Key(expected) => expected == key,
            Self::KeyRange(range) => range.contains(key),
        }
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct KeyRange {
    start: Option<RecordKey>,
    end: Option<RecordKey>,
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct RecordIdentifierRange {
    start: Option<RecordIdentifier>,
    end: Option<RecordIdentifier>,
}

impl KeyRange {
    pub fn new(start: Option<RecordKey>, end: Option<RecordKey>) -> Self {
        Self { start, end }
    }

    pub fn from(start: RecordKey) -> Self {
        Self::new(Some(start), None)
    }

    pub fn through(end: RecordKey) -> Self {
        Self::new(None, Some(end))
    }

    pub fn between(start: RecordKey, end: RecordKey) -> Self {
        Self::new(Some(start), Some(end))
    }

    pub fn contains(&self, key: &RecordKey) -> bool {
        self.start.as_ref().is_none_or(|start| key >= start)
            && self.end.as_ref().is_none_or(|end| key <= end)
    }
}

impl RecordIdentifierRange {
    pub fn new(start: Option<RecordIdentifier>, end: Option<RecordIdentifier>) -> Self {
        Self { start, end }
    }

    pub fn from(start: RecordIdentifier) -> Self {
        Self::new(Some(start), None)
    }

    pub fn through(end: RecordIdentifier) -> Self {
        Self::new(None, Some(end))
    }

    pub fn between(start: RecordIdentifier, end: RecordIdentifier) -> Self {
        Self::new(Some(start), Some(end))
    }

    pub fn contains(&self, identifier: RecordIdentifier) -> bool {
        self.start
            .is_none_or(|start| identifier.value() >= start.value())
            && self.end.is_none_or(|end| identifier.value() <= end.value())
    }
}

#[derive(Debug, Clone)]
pub struct ReadPlan<RecordValue> {
    node: ReadPlanNode<RecordValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedReadPlan {
    node: IdentifiedReadPlanNode,
}

impl<RecordValue> ReadPlan<RecordValue> {
    pub fn all_rows() -> Self {
        Self::new(ReadPlanNode::AllRows)
    }

    pub fn by_key(key: RecordKey) -> Self {
        Self::new(ReadPlanNode::ByKey(key))
    }

    pub fn by_key_range(range: KeyRange) -> Self {
        Self::new(ReadPlanNode::ByKeyRange(range))
    }

    pub fn filter(source: Self, predicate: RecordFilter<RecordValue>) -> Self {
        Self::new(ReadPlanNode::Filter {
            source: Box::new(source.node),
            predicate,
        })
    }

    pub fn constrain(unify: UnificationPlan) -> Self {
        Self::new(ReadPlanNode::Constrain { unify })
    }

    pub fn project(source: Self, fields: FieldSelection) -> Self {
        Self::new(ReadPlanNode::Project {
            source: Box::new(source.node),
            fields,
        })
    }

    pub fn aggregate(source: Self, reducer: AggregatePlan) -> Self {
        Self::new(ReadPlanNode::Aggregate {
            source: Box::new(source.node),
            reducer,
        })
    }

    pub fn infer(source: Self, rules: RuleSetRef) -> Self {
        Self::new(ReadPlanNode::Infer {
            source: Box::new(source.node),
            rules,
        })
    }

    pub fn recurse(seed: Self, mode: RecursionMode) -> Self {
        Self::new(ReadPlanNode::Recurse {
            seed: Box::new(seed.node),
            mode,
        })
    }

    pub fn operator(&self) -> ReadOperator {
        self.node.operator()
    }

    pub fn node(&self) -> &ReadPlanNode<RecordValue> {
        &self.node
    }

    fn new(node: ReadPlanNode<RecordValue>) -> Self {
        Self { node }
    }
}

impl IdentifiedReadPlan {
    pub fn all_rows() -> Self {
        Self::new(IdentifiedReadPlanNode::AllRows)
    }

    pub fn by_identifier(identifier: RecordIdentifier) -> Self {
        Self::new(IdentifiedReadPlanNode::ByIdentifier(identifier))
    }

    pub fn by_identifier_range(range: RecordIdentifierRange) -> Self {
        Self::new(IdentifiedReadPlanNode::ByIdentifierRange(range))
    }

    pub fn operator(&self) -> ReadOperator {
        self.node.operator()
    }

    pub fn node(&self) -> &IdentifiedReadPlanNode {
        &self.node
    }

    fn new(node: IdentifiedReadPlanNode) -> Self {
        Self { node }
    }
}

#[derive(Debug, Clone)]
pub enum ReadPlanNode<RecordValue> {
    AllRows,
    ByKey(RecordKey),
    ByKeyRange(KeyRange),
    Filter {
        source: Box<ReadPlanNode<RecordValue>>,
        predicate: RecordFilter<RecordValue>,
    },
    Constrain {
        unify: UnificationPlan,
    },
    Project {
        source: Box<ReadPlanNode<RecordValue>>,
        fields: FieldSelection,
    },
    Aggregate {
        source: Box<ReadPlanNode<RecordValue>>,
        reducer: AggregatePlan,
    },
    Infer {
        source: Box<ReadPlanNode<RecordValue>>,
        rules: RuleSetRef,
    },
    Recurse {
        seed: Box<ReadPlanNode<RecordValue>>,
        mode: RecursionMode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentifiedReadPlanNode {
    AllRows,
    ByIdentifier(RecordIdentifier),
    ByIdentifierRange(RecordIdentifierRange),
}

impl<RecordValue> ReadPlanNode<RecordValue> {
    pub fn operator(&self) -> ReadOperator {
        match self {
            Self::AllRows => ReadOperator::AllRows,
            Self::ByKey(_) => ReadOperator::ByKey,
            Self::ByKeyRange(_) => ReadOperator::ByKeyRange,
            Self::Filter { .. } => ReadOperator::Filter,
            Self::Constrain { .. } => ReadOperator::Constrain,
            Self::Project { .. } => ReadOperator::Project,
            Self::Aggregate { .. } => ReadOperator::Aggregate,
            Self::Infer { .. } => ReadOperator::Infer,
            Self::Recurse { .. } => ReadOperator::Recurse,
        }
    }
}

impl IdentifiedReadPlanNode {
    pub fn operator(&self) -> ReadOperator {
        match self {
            Self::AllRows => ReadOperator::AllRows,
            Self::ByIdentifier(_) => ReadOperator::ByKey,
            Self::ByIdentifierRange(_) => ReadOperator::ByKeyRange,
        }
    }
}

#[derive(
    Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord,
)]
#[rkyv(derive(Debug))]
pub enum ReadOperator {
    AllRows,
    ByKey,
    ByKeyRange,
    Filter,
    Constrain,
    Project,
    Aggregate,
    Infer,
    Recurse,
}

/// A typed selection over stored records: what the `Filter` read plan
/// keeps. A component implements it on a data-bearing type naming what it
/// selects by, so the plan carries the component's own typed question
/// rather than an expression string the engine would have to parse.
pub trait RecordPredicate<RecordValue>: Send + Sync {
    fn admits(&self, record: &RecordValue) -> bool;
}

/// The shared, clonable holder of one typed predicate inside a read plan.
pub struct RecordFilter<RecordValue> {
    predicate: Arc<dyn RecordPredicate<RecordValue>>,
}

impl<RecordValue> RecordFilter<RecordValue> {
    pub fn new(predicate: impl RecordPredicate<RecordValue> + 'static) -> Self {
        Self {
            predicate: Arc::new(predicate),
        }
    }
}

impl<RecordValue> RecordPredicate<RecordValue> for RecordFilter<RecordValue> {
    fn admits(&self, record: &RecordValue) -> bool {
        self.predicate.admits(record)
    }
}

impl<RecordValue> Clone for RecordFilter<RecordValue> {
    fn clone(&self) -> Self {
        Self {
            predicate: Arc::clone(&self.predicate),
        }
    }
}

impl<RecordValue> std::fmt::Debug for RecordFilter<RecordValue> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RecordFilter")
    }
}

/// Where a read plan's leaves draw their rows: the committed store inside
/// one read transaction, or a staging session's overlay of it.
pub trait RowSource<RecordValue> {
    fn all_rows(&self) -> crate::Result<Vec<RecordValue>>;
    fn by_key(&self, key: &RecordKey) -> crate::Result<Vec<RecordValue>>;
    fn by_key_range(&self, range: &KeyRange) -> crate::Result<Vec<RecordValue>>;
}

/// Executes a read plan over a row source. Leaves read the source;
/// `Filter` keeps the rows its typed predicate admits from its source's
/// rows; the remaining operators answer `UnsupportedReadPlan`.
pub trait ReadPlanExecution<RecordValue> {
    fn select(&self, source: &dyn RowSource<RecordValue>) -> crate::Result<Vec<RecordValue>>;
}

impl<RecordValue> ReadPlanExecution<RecordValue> for ReadPlanNode<RecordValue> {
    fn select(&self, source: &dyn RowSource<RecordValue>) -> crate::Result<Vec<RecordValue>> {
        match self {
            Self::AllRows => source.all_rows(),
            Self::ByKey(key) => source.by_key(key),
            Self::ByKeyRange(range) => source.by_key_range(range),
            Self::Filter {
                source: rows,
                predicate,
            } => {
                let mut selected = rows.select(source)?;
                selected.retain(|record| predicate.admits(record));
                Ok(selected)
            }
            unsupported => Err(crate::Error::UnsupportedReadPlan {
                operator: unsupported.operator(),
            }),
        }
    }
}

/// Rows already folded in memory, keyed by their stored key: a staging
/// session's overlay of committed state.
pub(crate) struct OverlaidRows<RecordValue> {
    rows: BTreeMap<String, RecordValue>,
}

impl<RecordValue> OverlaidRows<RecordValue> {
    pub(crate) fn new(rows: BTreeMap<String, RecordValue>) -> Self {
        Self { rows }
    }
}

impl<RecordValue: Clone> RowSource<RecordValue> for OverlaidRows<RecordValue> {
    fn all_rows(&self) -> crate::Result<Vec<RecordValue>> {
        Ok(self.rows.values().cloned().collect())
    }

    fn by_key(&self, key: &RecordKey) -> crate::Result<Vec<RecordValue>> {
        Ok(self
            .rows
            .get(&key.to_owned_string())
            .cloned()
            .into_iter()
            .collect())
    }

    fn by_key_range(&self, range: &KeyRange) -> crate::Result<Vec<RecordValue>> {
        Ok(self
            .rows
            .iter()
            .filter(|(key, _)| range.contains(&RecordKey::new((*key).clone())))
            .map(|(_, record)| record.clone())
            .collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSelection {
    fields: Vec<String>,
}

impl FieldSelection {
    pub fn named(fields: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            fields: fields.into_iter().map(Into::into).collect(),
        }
    }

    pub fn fields(&self) -> &[String] {
        &self.fields
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnificationPlan {
    bindings: Vec<String>,
}

impl UnificationPlan {
    pub fn new(bindings: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            bindings: bindings.into_iter().map(Into::into).collect(),
        }
    }

    pub fn bindings(&self) -> &[String] {
        &self.bindings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatePlan {
    reducer: String,
}

impl AggregatePlan {
    pub fn new(reducer: impl Into<String>) -> Self {
        Self {
            reducer: reducer.into(),
        }
    }

    pub fn reducer(&self) -> &str {
        &self.reducer
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSetRef {
    name: String,
}

impl RuleSetRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursionMode {
    edge: String,
}

impl RecursionMode {
    pub fn new(edge: impl Into<String>) -> Self {
        Self { edge: edge.into() }
    }

    pub fn edge(&self) -> &str {
        &self.edge
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySnapshot<RecordValue> {
    operation: SemaOperation,
    table: TableName,
    database_marker: DatabaseMarker,
    records: Vec<RecordValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentifiedQuerySnapshot<RecordValue> {
    operation: SemaOperation,
    table: TableName,
    database_marker: DatabaseMarker,
    records: Vec<IdentifiedRecord<RecordValue>>,
}

impl<RecordValue> QuerySnapshot<RecordValue> {
    pub fn new(
        operation: SemaOperation,
        table: TableName,
        database_marker: DatabaseMarker,
        records: Vec<RecordValue>,
    ) -> Self {
        Self {
            operation,
            table,
            database_marker,
            records,
        }
    }

    pub fn operation(&self) -> SemaOperation {
        self.operation
    }

    pub fn table(&self) -> &TableName {
        &self.table
    }

    pub fn snapshot(&self) -> SnapshotIdentifier {
        self.database_marker.snapshot()
    }

    pub fn database_marker(&self) -> DatabaseMarker {
        self.database_marker
    }

    pub fn records(&self) -> &[RecordValue] {
        &self.records
    }
}

impl<RecordValue> IdentifiedQuerySnapshot<RecordValue> {
    pub fn new(
        operation: SemaOperation,
        table: TableName,
        database_marker: DatabaseMarker,
        records: Vec<IdentifiedRecord<RecordValue>>,
    ) -> Self {
        Self {
            operation,
            table,
            database_marker,
            records,
        }
    }

    pub fn operation(&self) -> SemaOperation {
        self.operation
    }

    pub fn table(&self) -> &TableName {
        &self.table
    }

    pub fn snapshot(&self) -> SnapshotIdentifier {
        self.database_marker.snapshot()
    }

    pub fn database_marker(&self) -> DatabaseMarker {
        self.database_marker
    }

    pub fn records(&self) -> &[IdentifiedRecord<RecordValue>] {
        &self.records
    }

    pub fn into_records(self) -> Vec<IdentifiedRecord<RecordValue>> {
        self.records
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReceipt {
    operation: SemaOperation,
    table: TableName,
    database_marker: DatabaseMarker,
    record_count: usize,
}

impl ValidationReceipt {
    pub fn new(
        operation: SemaOperation,
        table: TableName,
        database_marker: DatabaseMarker,
        record_count: usize,
    ) -> Self {
        Self {
            operation,
            table,
            database_marker,
            record_count,
        }
    }

    pub fn operation(&self) -> SemaOperation {
        self.operation
    }

    pub fn table(&self) -> &TableName {
        &self.table
    }

    pub fn snapshot(&self) -> SnapshotIdentifier {
        self.database_marker.snapshot()
    }

    pub fn database_marker(&self) -> DatabaseMarker {
        self.database_marker
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }
}

/// The committed rows of one registered table, read inside one storage
/// read transaction so the rows and the database marker agree.
pub(crate) struct CommittedRows<'transaction, RecordValue> {
    transaction: &'transaction ::memory::ReadTransaction,
    table: &'transaction TableReference<RecordValue>,
}

impl<'transaction, RecordValue> CommittedRows<'transaction, RecordValue> {
    pub(crate) fn new(
        transaction: &'transaction ::memory::ReadTransaction,
        table: &'transaction TableReference<RecordValue>,
    ) -> Self {
        Self { transaction, table }
    }
}

impl<RecordValue> RowSource<RecordValue> for CommittedRows<'_, RecordValue>
where
    RecordValue: crate::EngineStoredValue,
    <RecordValue as Archive>::Archived: RkyvDeserialize<RecordValue, HighDeserializer<rancor::Error>>
        + for<'validation> CheckBytes<
            Strategy<Validator<ArchiveValidator<'validation>, SharedValidator>, rancor::Error>,
        >,
{
    fn all_rows(&self) -> crate::Result<Vec<RecordValue>> {
        Ok(self
            .table
            .memory_table()
            .iter(self.transaction)?
            .into_iter()
            .map(|(_key, record)| record)
            .collect())
    }

    fn by_key(&self, key: &RecordKey) -> crate::Result<Vec<RecordValue>> {
        Ok(self
            .table
            .memory_table()
            .get(self.transaction, key.to_owned_string())?
            .into_iter()
            .collect())
    }

    fn by_key_range(&self, range: &KeyRange) -> crate::Result<Vec<RecordValue>> {
        Ok(self
            .table
            .memory_table()
            .iter(self.transaction)?
            .into_iter()
            .filter(|(key, _record)| range.contains(&RecordKey::new(key.clone())))
            .map(|(_key, record)| record)
            .collect())
    }
}
