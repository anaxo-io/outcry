# Contributing

Thanks for taking the time to contribute.

## Development setup

```bash
git clone https://github.com/anaxo-io/outcry
cd outcry
cargo test
```

The toolchain is pinned in `rust-toolchain.toml`. Tests need no network and no special
permissions; the file-backed tests use a temporary directory.

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps --all-features
cargo +nightly miri test --test behaviour -- --skip file_backed --skip open_rejects
```

CI runs all of these plus an MSRV check against Rust 1.85 and `cargo deny check`.

## What this crate is careful about

- **`unsafe` has two homes and two callers.** The unsafe *functions* live in `mapping.rs`
  (owns the mapping, hands out atomics) and `copy.rs` (moves bytes in and out of the
  ring). `producer.rs` and `consumer.rs` call them, each call in an `unsafe {}` block with
  a `// SAFETY:` comment naming the invariant that makes it sound. Nothing else may
  contain the keyword; `grep -c unsafe src/*.rs` is part of review.
- **Every mapping pointer derives from one write-capable base.** `mapping.rs` takes a
  single `*mut u8` from `as_mut_ptr()` and offsets from it. Taking a pointer from a shared
  reborrow of the mapping instead strips write permission: undefined behaviour under
  Stacked Borrows, which Miri catches and hardware does not. This was a real bug, fixed in
  0f79bed.
- **The fences are load-bearing.** A `fence(Release)` after storing the reserved counter
  and a `fence(Acquire)` before the second overrun check are what make the double-check
  sound. They look removable and are not.
- **`reserve_block` is derived, not constant.** It is `capacity / 16`. A fixed block
  larger than a small ring reserves past the end and reports overruns that never happened.
- **`Queue::open` reads a file it does not trust.** Header *and* counters are validated
  before any pointer is formed from them; `tests/malformed.rs` crafts the states that
  matter. An unaligned or out-of-range position turns a safe call into undefined
  behaviour, so a new field read from the mapping needs a new check and a new test there.
- **The sound copy path must pass Miri.** It is the reason this crate exists as a Rust
  crate rather than a port. If a change makes Miri unhappy on the default features, the
  change is wrong, not Miri.
- **The overrun property is the contract.** `tests/overrun_property.rs` stalls a reader
  until the writer has lapped it and then checks every accepted frame against its own
  checksum. A change that weakens that test needs a written argument for why the
  guarantee still holds.
- **Benchmarks are part of a change to the hot path.** If you touch `producer.rs`,
  `consumer.rs` or `copy.rs`, run both benchmarks before and after, pinned
  (`OUTCRY_PIN=...`, see the README for choosing cores), at least five runs each, and put
  the medians in the pull request with the machine and the core list named. An unpinned
  single run on a multi-L3 CPU is a coin flip and will be asked to be redone.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`,
`perf:`, `refactor:`, `test:`, `chore:`.

## Licence

Contributions are dual-licensed under MIT and Apache-2.0, matching the crate.
