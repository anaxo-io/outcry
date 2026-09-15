# Contributing

Thanks for taking the time to contribute.

## Development setup

```bash
git clone https://github.com/anaxo-io/outcry
cd outcry
cargo test
cargo test --features fast-copy
```

The toolchain is pinned in `rust-toolchain.toml`. Tests need no network and no special
permissions; the file-backed tests use a temporary directory.

## Before opening a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test && cargo test --features fast-copy
cargo doc --no-deps --all-features
cargo +nightly miri test --test behaviour -- --skip file_backed --skip open_rejects
```

CI runs all of these plus an MSRV check against Rust 1.85 and `cargo deny check`.

## What this crate is careful about

- **`unsafe` is confined to two files.** `mapping.rs` owns the mapping and hands out
  atomics; `copy.rs` moves bytes in and out of the ring. Every `unsafe` block states its
  invariant in a `// SAFETY:` comment. Do not add a third file.
- **The sound copy path must pass Miri.** It is the reason this crate exists as a Rust
  crate rather than a port. If a change makes Miri unhappy on the default features, the
  change is wrong, not Miri.
- **The overrun property is the contract.** `tests/overrun_property.rs` stalls a reader
  until the writer has lapped it and then checks every accepted frame against its own
  checksum. A change that weakens that test needs a written argument for why the
  guarantee still holds.
- **Benchmarks are part of a change to the hot path.** If you touch `producer.rs`,
  `consumer.rs` or `copy.rs`, run `cargo bench --bench throughput` before and after and
  put both tables in the pull request, with the machine named.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`,
`perf:`, `refactor:`, `test:`, `chore:`.

## Licence

Contributions are dual-licensed under MIT and Apache-2.0, matching the crate.
