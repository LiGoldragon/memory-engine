# Upgrades

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
