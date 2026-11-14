# Upgrades

## 0.20.0 — A sema store is upgraded on open

`Engine::open` (and `open_recovering`) now converts a store sema-engine
wrote when it finds one at its path, in place and in one write
transaction, before the open goes on as for any memory store. A
consumer whose deployed store sema-engine wrote needs no conversion step
of its own: repin and open. The file is recognised by its content — the
kernel's `__sema_meta` and `__sema_headers` under this build's rkyv
header, engine storage layout 7, and a log and checkpoints that verify
under the sema labels — and keeps the schema version its writer stamped.

- `Engine::sema_conversion()` returns the `SemaConversion` receipt of a
  conversion this open made, and `None` for a new store, a memory store,
  and every later open.
- `LegacySemaStore::convert` now refuses a store at any storage layout
  but 7 with `Error::StorageLayoutMismatch`, before any write.
- An unversioned file that is not a sema file is refused as before,
  with the kernel's `LegacyFileLacksSchema`.

## 0.19.0 — Convert a store sema-engine wrote

A store written by sema-engine 0.15 (storage layout 7, over the sema
kernel) does not open on 0.18.0: its tables carry `__sema_` names and
its five content hashes carry `sema-engine-…` labels. 0.19.0 converts
it in place with one call, in one write transaction:

```rust
let conversion = LegacySemaStore::new(&copy).convert()?;
```

To move a consumer whose deployed store sema-engine wrote:

1. Copy the legacy file, convert the copy, and on `Ok` rename the copy
   to the store's memory path; on `Err` the copy is as it was and the
   original stands. Then open with `Engine::open` as before.
2. Handle the refusals: `Error::SemaSchemaHashUnmatched` (an entry's
   store schema hash matches neither the catalog nor any checkpoint's
   family list), `Error::SemaStagingPending` (settle the parked staged
   group or compaction intent under the engine that wrote the store
   first), the store's own verification errors (`VersionedChainBroken`,
   `CheckpointDigestMismatch`, `SegmentDigestMismatch`,
   `ViewDigestMismatch`, `CheckpointSchemaMismatch`,
   `OutboxEntryMismatch`, `MirrorHeadForked`, `SegmentMissing`), and
   the kernel's `NotSemaFile` / `AlreadyMemoryFile` inside
   `Error::Memory`.
3. The receipt `SemaConversion` names the entries, checkpoints and
   segments rewritten and the entry digests compaction had cut off,
   kept in sema form as chain origins; the next compaction replaces
   them.

`Error` gains `SemaSchemaHashUnmatched` and `SemaStagingPending`;
exhaustive matches over it need the new arms. The storage layout stays
7 and a store in the memory format opens as before.

memory-engine now pins memory 0.2.0 (`e78519c`), which adds `SemaFile`
and `Table::rename_from`. A consumer that also depends on memory
directly pins the same revision.

## 0.18.0 — Memorable: typed change and typed upgrade replace `with_prior`

`TableDescriptor::with_prior(SchemaHash, Fn(Prior) -> Current)` and
`EvolutionStep` are removed. A family's evolution is now the record's own
authored, fallible conversion across a declared manifest edge, and
`register_table` no longer migrates rows.

To move a consumer that declared a prior generation:

1. Implement `MemoryDefinition` (manifest version and per-family schema
   hash) on the predecessor record type and on the current one. The
   hashes are the ones the store's catalog already carries, so stored
   bytes do not change.
2. Implement `UpgradeFrom<Predecessor>` on the current type: the old
   closure's body, returning `Decision::Ready(current)` or
   `Decision::Refused(reason)` with a typed reason of the record's own.
   A byte-compatible predecessor needs its own type, since one type
   declares one version.
3. Replace `.with_prior::<Prior>(hash, convert)` with
   `.with_predecessor::<Prior>()`.
4. At startup, call `engine.upgrade_table::<Prior, Current>(descriptor)`
   (trait `MemoryUpgrades`) where the code called `register_table`, and
   handle its outcome: `Upgraded` or `AlreadyCurrent` carry the table
   reference; `Refused` names the refusing row's key and reason with the
   predecessor bytes and marker intact; `Failed` carries a
   `StorageFailure`. `register_table` against a store still holding a
   declared predecessor now refuses with `Error::UpgradePending`.

`Error` gains `StagedChangeUnsupported`, `UpgradePending`,
`PredecessorUndeclared`, `ManifestEdgeBackward`, `ManifestEdgeMismatch`
and `UpgradeInterrupted`; exhaustive matches over it need the new arms.

New, additive: `Memorable` and `MemoryChanges::change` /
`change_receipt` give a record a typed change with a three-branch
outcome and durable receipts under a caller-chosen `ChangeIdentifier`,
stored in the new internal table `__memory_engine_change_receipts`. The
engine storage layout number is unchanged; an existing store opens as is
and gains that table on its first typed change. No other repository in
the workspace used `with_prior`, so no dependent repository needs a
code change to repin.
