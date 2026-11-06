//! Writes the legacy sema-engine stores memory-engine's conversion tests read.
//!
//! This file is not part of memory-engine's build. It is an example of
//! sema-engine 27e814a (0.15.1, storage layout 7, sema kernel 51d7927); the
//! stores beside it are its output, written through Nix:
//!
//! 1. `git archive 27e814a | tar -x -C <dir>` from this repository;
//! 2. copy this file to `<dir>/examples/write_legacy_fixtures.rs`;
//! 3. add to that flake's `packages`:
//!    `legacy-fixtures = craneLib.mkCargoDerivation (commonArgs // { inherit cargoArtifacts; pname = "sema-engine-legacy-fixtures"; buildPhaseCargoCommand = "cargo run --release --example write_legacy_fixtures -- $out"; installPhaseCommand = ""; doInstallCargoArtifacts = false; });`
//! 4. `nix build <dir>#legacy-fixtures` and copy the `.sema` files here.
//!
//! The output is deterministic: two builds wrote byte-identical stores.
//!
//! Every store but `grown.sema` holds the same ten-entry history (see
//! `write_history`).
//!
//! - `entries.sema`: versioned history, no checkpoint.
//! - `compacted.sema`: compacted after entry 6, so one root checkpoint whose
//!   covered entries are gone, and entries 7 to 10 after it.
//! - `chained.sema`: compacted after entry 6, checkpointed after entry 9 (a
//!   second checkpoint chained to the root, covering present entries), and
//!   entry 10 after both.
//! - `mirrored.sema`: mirror topology, outbox rows, head acknowledged at 4.
//! - `grown.sema`: two thought entries, then the tally family registered and
//!   one tally entry, no checkpoint: the first entries carry the schema hash
//!   of a catalog the store no longer holds.
//! - `staged.sema`: the ten entries and a parked staged group.

use std::path::Path;

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use sema_engine::{
    Assertion, Engine, EngineOpen, EngineRecord, FamilyName, IdentifiedAssertion,
    IdentifiedMutation, IdentifiedTableDescriptor, MirrorHead, Mutation, RecordKey, Retraction,
    SchemaHash, SchemaVersion, TableDescriptor, TableName, TableReference, VersionedHistoryAcknowledgement,
    VersionedHistoryRetention, VersionedRecoveryTopology, VersionedStoreName, VersioningPolicy,
};

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
struct Thought {
    key: String,
    body: String,
}

impl Thought {
    fn new(key: &str, body: &str) -> Self {
        Self {
            key: key.to_owned(),
            body: body.to_owned(),
        }
    }
}

impl EngineRecord for Thought {
    fn record_key(&self) -> RecordKey {
        RecordKey::new(self.key.clone())
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
struct Tally {
    note: String,
}

impl Tally {
    fn new(note: &str) -> Self {
        Self {
            note: note.to_owned(),
        }
    }
}

struct Writer;

impl Writer {
    fn open(path: &Path, topology: VersionedRecoveryTopology) -> Engine {
        Engine::open(
            EngineOpen::new(path, SchemaVersion::new(1)).with_versioning(
                VersioningPolicy::new(VersionedStoreName::new("legacy"))
                    .with_recovery_topology(topology),
            ),
        )
        .expect("legacy engine opens")
    }

    fn thoughts() -> TableDescriptor<Thought> {
        TableDescriptor::new(
            TableName::new("thoughts"),
            FamilyName::new("thought"),
            SchemaHash::for_label("thought-v1"),
        )
    }

    fn tallies() -> IdentifiedTableDescriptor<Tally> {
        IdentifiedTableDescriptor::new(
            TableName::new("tallies"),
            FamilyName::new("tally"),
            SchemaHash::for_label("tally-v1"),
        )
    }

    /// Ten entries: 1 assert alpha, 2 assert beta, 3 assert gamma,
    /// 4 mutate alpha, 5 assert tally 1, 6 retract beta, 7 assert delta,
    /// 8 mutate tally 1, 9 assert tally 2, 10 mutate gamma. The hooks run
    /// after entry 6 and after entry 9.
    fn write_history(
        engine: &mut Engine,
        after_six: impl FnOnce(&Engine),
        after_nine: impl FnOnce(&Engine),
    ) -> TableReference<Thought> {
        let thoughts = engine.register_table(Self::thoughts()).expect("thoughts");
        let tallies = engine
            .register_identified_table(Self::tallies())
            .expect("tallies");
        engine
            .assert(Assertion::new(thoughts, Thought::new("alpha", "first")))
            .expect("1");
        engine
            .assert(Assertion::new(thoughts, Thought::new("beta", "first")))
            .expect("2");
        engine
            .assert(Assertion::new(thoughts, Thought::new("gamma", "first")))
            .expect("3");
        engine
            .mutate(Mutation::new(thoughts, Thought::new("alpha", "second")))
            .expect("4");
        let tally = engine
            .assert_identified(IdentifiedAssertion::new(tallies, Tally::new("one")))
            .expect("5")
            .identifier();
        engine
            .retract(Retraction::new(thoughts, RecordKey::new("beta")))
            .expect("6");
        after_six(engine);
        engine
            .assert(Assertion::new(thoughts, Thought::new("delta", "first")))
            .expect("7");
        engine
            .mutate_identified(IdentifiedMutation::new(tallies, tally, Tally::new("one again")))
            .expect("8");
        engine
            .assert_identified(IdentifiedAssertion::new(tallies, Tally::new("two")))
            .expect("9");
        after_nine(engine);
        engine
            .mutate(Mutation::new(thoughts, Thought::new("gamma", "second")))
            .expect("10");
        thoughts
    }

    fn entries(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::LocalCheckpoint);
        let _thoughts = Self::write_history(&mut engine, |_| {}, |_| {});
    }

    fn compact(engine: &Engine) {
        engine
            .compact_versioned_history(
                VersionedHistoryRetention::new(0),
                VersionedHistoryAcknowledgement::LocalCheckpoint,
            )
            .expect("compaction through 6");
    }

    fn report(name: &str, engine: &Engine) {
        let checkpoint = engine
            .latest_checkpoint()
            .expect("checkpoint reads")
            .expect("checkpoint exists");
        println!(
            "{name}: latest checkpoint {} covers {:?}, log holds {} entries",
            checkpoint.metadata().sequence().value(),
            checkpoint.metadata().covered(),
            engine.versioned_commit_log().expect("log").len()
        );
    }

    fn compacted(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::LocalCheckpoint);
        let _thoughts = Self::write_history(&mut engine, Self::compact, |_| {});
        Self::report("compacted", &engine);
    }

    fn chained(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::LocalCheckpoint);
        let _thoughts = Self::write_history(&mut engine, Self::compact, |engine| {
            engine.checkpoint().expect("checkpoint through 9");
        });
        Self::report("chained", &engine);
    }

    fn grown(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::LocalCheckpoint);
        let thoughts = engine.register_table(Self::thoughts()).expect("thoughts");
        engine
            .assert(Assertion::new(thoughts, Thought::new("alpha", "first")))
            .expect("1");
        engine
            .assert(Assertion::new(thoughts, Thought::new("beta", "first")))
            .expect("2");
        let tallies = engine
            .register_identified_table(Self::tallies())
            .expect("tallies");
        engine
            .assert_identified(IdentifiedAssertion::new(tallies, Tally::new("one")))
            .expect("3");
    }

    fn staged(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::LocalCheckpoint);
        let thoughts = Self::write_history(&mut engine, |_| {}, |_| {});
        engine.begin_staged_group().expect("staging begins");
        engine
            .assert(Assertion::new(thoughts, Thought::new("epsilon", "staged")))
            .expect("staged assert");
        engine
            .park_staged_group()
            .expect("group parks")
            .expect("group is not empty");
    }

    fn mirrored(path: &Path) {
        let mut engine = Self::open(path, VersionedRecoveryTopology::Mirror);
        let _thoughts = Self::write_history(
            &mut engine,
            |engine| {
                let log = engine.versioned_commit_log().expect("log");
                let fourth = &log[3];
                engine
                    .acknowledge_mirror(MirrorHead::new(
                        fourth.commit_sequence(),
                        fourth.entry_digest(),
                    ))
                    .expect("mirror acknowledges 4");
            },
            |_| {},
        );
    }
}

fn main() {
    let directory = std::env::args().nth(1).expect("output directory");
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory).expect("output directory");
    Writer::entries(&directory.join("entries.sema"));
    Writer::compacted(&directory.join("compacted.sema"));
    Writer::chained(&directory.join("chained.sema"));
    Writer::mirrored(&directory.join("mirrored.sema"));
    Writer::grown(&directory.join("grown.sema"));
    Writer::staged(&directory.join("staged.sema"));
}
