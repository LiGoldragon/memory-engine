# Upgrades

## 0.19.1 — bounded first-N key-range reads

`QueryPlan::first_in_key_range` creates a distinct `BoundedKeyRangePlan`,
executed with `Engine::match_first_in_key_range`. It reads the inclusive
string-key range in ascending order and returns at most the caller's
`KeyRangeLimit`. A zero limit returns no rows without visiting a table range.
Committed rows use sema's deferred `visit_range` visitor and stop after the
requested number of visible results. In a staged session, the latest
replacement or tombstone for each key is merged before a committed payload is
decoded; buffered operations are traversed to build that overlay, so total
staged-history work is separate from the result limit. Sparse deletions can
require visiting additional committed keys before the result is full.

The storage kernel dependency now resolves through the authoritative
`LiGoldragon/memory` repository at sema 0.1.2 revision
`712a4a9177e71dc2b397cf4e5a5c65b4fa3c0ec8`; this is the published source of
the deferred ordered-range visitor. Existing unbounded `QueryPlan` reads are
unchanged.

## 0.19.0 — one Memory edit may evolve several families

`AtomicEvolution` carries evolved predecessor inventories, new families, and
writes to unchanged families through one storage transaction.
`Engine::inspect_predecessor` reads a declared predecessor without opening it
as the current type; `commit_atomic_evolution` validates every witnessed
catalog identity before it lands all row changes, catalog registrations, and
one shared commit sequence and snapshot. A failed validation or interrupted
transaction leaves every family at its predecessor identity and contents.

The existing `MemoryUpgrades::upgrade_table` remains the smaller operation for
a record-local, one-family `UpgradeFrom`.

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
stored in the new internal table `__sema_engine_change_receipts`. The
engine storage layout number is unchanged; an existing store opens as is
and gains that table on its first typed change. No other repository in
the workspace used `with_prior`, so no dependent repository needs a
code change to repin.
