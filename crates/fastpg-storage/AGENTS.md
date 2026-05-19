# Agent Notes

## Arena rules

When working in `fastpg-storage`, stick to the rules of the arena. By-reference
row data is owned by `StorageRegion` arenas, and changes must preserve those
lifetimes instead of smuggling in borrowed pointers or ad hoc heap ownership.

- Copy by-reference cell bytes into the active `StorageRegion` with
  `StorageRegion::alloc_bytes`; do not store pointers into caller-owned
  PostgreSQL memory, stack buffers, temporary `Vec`s, or scan buffers.
- Keep each `ValueRef.region_id` tied to the region that owns its bytes. When a
  row moves between transaction, savepoint, fixture, epoch, scan, and committed
  storage, move it through the region copy/promotion path so the destination
  arena owns the bytes.
- Preserve checkpoint and rewind semantics. Savepoint aborts and other rollback
  paths must use `StorageRegion::checkpoint`/`rewind_to` and leave
  `RegionAccounting` consistent with the arena bytes that remain live.
- Treat scan materialization as scan-region data and release it when the scan
  ends.
- If a change touches by-reference values, row promotion, savepoint rollback, or
  scan lifetime behavior, add or update focused `fastpg-storage` tests that
  prove arena ownership and accounting.

Useful validation after storage changes:

```sh
cargo test -p fastpg-storage
```
