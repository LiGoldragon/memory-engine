// Hand-authored engine TableSpecification contract fixture.

#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Domain(std::string::String);
impl Domain {
    /// Construct the fixture newtype from its declared payload.
    pub fn new(payload: std::string::String) -> Self {
        Self(payload)
    }
    /// Borrow the one declared payload without decoding archive bytes.
    pub fn payload(&self) -> &std::string::String {
        &self.0
    }
    /// Consume the newtype and recover its declared payload.
    pub fn into_payload(self) -> std::string::String {
        self.0
    }
}
#[derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DomainSnapshot {
    pub field_0: Domain,
    pub field_1: u64,
    pub field_2: std::string::String,
}
pub struct DomainSnapshots;
impl memory_engine::TableSpecification for DomainSnapshots {
    type Record = DomainSnapshot;
    type Key = Domain;
    const TABLE_NAME: memory_engine::TableName = memory_engine::TableName::new("domain_snapshots");
    const FAMILY_NAME: &'static str = "domain_snapshot";
    const SCHEMA_HASH: memory_engine::SchemaHash = memory_engine::SchemaHash::new([
        199, 207, 67, 102, 195, 131, 30, 219, 59, 19, 153, 4, 179, 56, 222, 133, 25, 80, 186, 112,
        200, 223, 251, 94, 183, 111, 171, 172, 194, 216, 16, 218,
    ]);
    fn record_key(key: &Self::Key) -> memory_engine::Result<memory_engine::RecordKey> {
        Ok(memory_engine::RecordKey::new(key.payload().to_string()))
    }
}
