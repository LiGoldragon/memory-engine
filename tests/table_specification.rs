use memory_engine::{Engine, EngineOpen, SchemaVersion, TableSpecification};

mod fixture {
    include!("fixtures/domain_snapshot_table.rs");
}

use fixture::{Domain, DomainSnapshot, DomainSnapshots};

#[test]
fn table_specification_declares_its_storage_contract() {
    assert_eq!(DomainSnapshots::TABLE_NAME.as_str(), "domain_snapshots");
    assert_eq!(DomainSnapshots::FAMILY_NAME, "domain_snapshot");
    let descriptor = DomainSnapshots::descriptor();
    assert_eq!(descriptor.name().as_str(), "domain_snapshots");
    assert_eq!(descriptor.family().as_str(), "domain_snapshot");
    assert_eq!(descriptor.schema_hash(), DomainSnapshots::SCHEMA_HASH);
}

#[test]
fn declared_domain_snapshot_table_writes_and_reads_a_typed_domain_key() {
    let temporary = tempfile::tempdir().expect("fresh Memory directory");
    let path = temporary.path().join("table-specification.memory");
    let mut engine = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1)))
        .expect("fresh redb-backed Memory opens");
    engine
        .register_table(DomainSnapshots::descriptor())
        .expect("declared table registers");

    let domain = Domain::new("software/code-generation".to_owned());
    assert_eq!(
        DomainSnapshots::record_key(&domain)
            .expect("declared source key projection")
            .to_owned_string(),
        "software/code-generation"
    );
    let domain_snapshot = DomainSnapshot {
        field_0: domain.clone(),
        field_1: 17,
        field_2: "typed value".to_owned(),
    };
    engine
        .assert_keyed(
            DomainSnapshots::assertion(&domain, domain_snapshot.clone()).expect("typed assertion"),
        )
        .expect("record commits through memory-engine");
    let snapshot = engine
        .match_records(DomainSnapshots::query(&domain).expect("typed key query"))
        .expect("record reads through memory-engine");

    assert_eq!(snapshot.records(), std::slice::from_ref(&domain_snapshot));
    drop(engine);

    let mut reopened = Engine::open(EngineOpen::new(&path, SchemaVersion::new(1)))
        .expect("redb-backed Memory reopens");
    reopened
        .register_table(DomainSnapshots::descriptor())
        .expect("declared table re-registers");
    assert_eq!(
        reopened
            .match_records(DomainSnapshots::query(&domain).expect("typed reopen query"))
            .expect("persisted record reads")
            .records(),
        &[domain_snapshot],
    );
}
