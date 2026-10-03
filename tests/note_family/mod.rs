//! One ordinary family across two manifest versions, shared by the
//! Memorable witnesses.
//!
//! Version 1 stores a note as one `text`. Version 2 stores a `title` and a
//! `body`. The authored split rule, v1 to v2: the title is the first line
//! of `text` with surrounding whitespace trimmed; the body is everything
//! after the first line break, verbatim, and empty when there is none. A
//! text whose first line is empty or blank — the empty text included —
//! has no title to carry, and refuses with `InvalidLegacyTitle` naming the
//! note.
//!
//! Version 2's typed change is `Write` (create or replace) or `Rename`; a
//! blank title refuses with `EmptyTitle`, and renaming a note that is not
//! stored refuses with `Missing`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use sema_engine::{
    Decision, Engine, EngineOpen, FamilyName, Memorable, MemoryDefinition, MemoryVersion,
    SchemaHash, SchemaVersion, TableDescriptor, TableName, TableRegistration, UpgradeFrom,
};
use tempfile::TempDir;

pub const NOTES: TableName = TableName::new("notes");
const CATALOG: sema::Table<&'static str, TableRegistration> =
    sema::Table::new("__sema_engine_catalog");

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct NoteV1 {
    pub id: String,
    pub text: String,
}

impl NoteV1 {
    pub fn new(id: &str, text: &str) -> Self {
        Self {
            id: id.to_owned(),
            text: text.to_owned(),
        }
    }
}

impl MemoryDefinition for NoteV1 {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(1)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("note-v1")
    }
}

#[derive(Archive, RkyvSerialize, RkyvDeserialize, Debug, Clone, PartialEq, Eq)]
#[rkyv(derive(Debug))]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
}

impl Note {
    pub fn new(id: &str, title: &str, body: &str) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
        }
    }
}

impl MemoryDefinition for Note {
    fn memory_version() -> MemoryVersion {
        MemoryVersion::new(2)
    }

    fn schema_hash() -> SchemaHash {
        SchemaHash::for_label("note-v2")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteChange {
    Write {
        id: String,
        title: String,
        body: String,
    },
    Rename {
        title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteRefusal {
    EmptyTitle,
    Missing,
}

impl Memorable for Note {
    type Change = NoteChange;
    type Refusal = NoteRefusal;

    fn change(current: Option<&Self>, change: &NoteChange) -> Decision<Self, NoteRefusal> {
        match (change, current) {
            (NoteChange::Write { title, .. }, _) | (NoteChange::Rename { title }, Some(_))
                if title.trim().is_empty() =>
            {
                Decision::Refused(NoteRefusal::EmptyTitle)
            }
            (NoteChange::Write { id, title, body }, _) => {
                Decision::Ready(Note::new(id, title, body))
            }
            (NoteChange::Rename { .. }, None) => Decision::Refused(NoteRefusal::Missing),
            (NoteChange::Rename { title }, Some(note)) => Decision::Ready(Note {
                title: title.clone(),
                ..note.clone()
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteUpgradeRefusal {
    InvalidLegacyTitle { note: String },
}

impl UpgradeFrom<NoteV1> for Note {
    type Refusal = NoteUpgradeRefusal;

    fn upgrade_from(predecessor: NoteV1) -> Decision<Self, NoteUpgradeRefusal> {
        let (first_line, body) = predecessor
            .text
            .split_once('\n')
            .unwrap_or((predecessor.text.as_str(), ""));
        let title = first_line.trim();
        if title.is_empty() {
            return Decision::Refused(NoteUpgradeRefusal::InvalidLegacyTitle {
                note: predecessor.id,
            });
        }
        Decision::Ready(Note::new(&predecessor.id, title, body))
    }
}

pub struct NoteStore {
    directory: TempDir,
}

impl NoteStore {
    pub fn new() -> Self {
        Self {
            directory: tempfile::tempdir().expect("temp dir"),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.directory.path().join("notes.sema")
    }

    pub fn open(&self) -> Engine {
        Engine::open(EngineOpen::new(self.path(), SchemaVersion::new(1))).expect("engine opens")
    }

    pub fn descriptor_v1(&self) -> TableDescriptor<NoteV1> {
        TableDescriptor::new(NOTES, FamilyName::new("note"), NoteV1::schema_hash())
    }

    pub fn descriptor_v2(&self) -> TableDescriptor<Note> {
        TableDescriptor::new(NOTES, FamilyName::new("note"), Note::schema_hash())
            .with_predecessor::<NoteV1>()
    }

    pub fn file_bytes(&self) -> Vec<u8> {
        std::fs::read(self.path()).expect("store file reads")
    }

    /// The raw store as the kernel holds it, the engine closed: the
    /// catalog's format marker for the notes table, and every notes row
    /// decoded (validated) in the predecessor shape.
    pub fn raw_v1(&self) -> (SchemaHash, Vec<(String, NoteV1)>) {
        raw::<NoteV1>(&self.path())
    }

    pub fn raw_v2(&self) -> (SchemaHash, Vec<(String, Note)>) {
        raw::<Note>(&self.path())
    }
}

fn raw<Row>(path: &Path) -> (SchemaHash, Vec<(String, Row)>)
where
    Row: sema_engine::EngineStoredValue,
    <Row as Archive>::Archived: rkyv::Deserialize<Row, rkyv::api::high::HighDeserializer<rkyv::rancor::Error>>
        + for<'validation> rkyv::bytecheck::CheckBytes<
            rkyv::rancor::Strategy<
                rkyv::validation::Validator<
                    rkyv::validation::archive::ArchiveValidator<'validation>,
                    rkyv::validation::shared::SharedValidator,
                >,
                rkyv::rancor::Error,
            >,
        >,
{
    let kernel = sema::Sema::open_with_schema(
        path,
        &sema::Schema {
            version: SchemaVersion::new(1),
        },
    )
    .expect("kernel opens");
    kernel
        .read(|transaction| {
            let marker = CATALOG
                .get(transaction, NOTES.as_str())?
                .expect("notes are catalogued")
                .identity()
                .schema_hash();
            let rows = sema::Table::<String, Row>::new(NOTES.as_str()).iter(transaction)?;
            Ok((marker, rows))
        })
        .expect("raw store reads")
}
