# CLAUDE.md

Guidance for Claude Code working in this repository.

## What this is

`outcry` is a single-writer, many-reader broadcast queue over POSIX shared memory: a Rust
implementation of the FastQueue design from David Gross's CppCon 2024 talk *When
Nanoseconds Matter*. Readers never block the writer; a reader that falls too far behind is
overrun and told so, rather than handed a torn frame.

It was extracted from a private trading monorepo as a standalone crate. Nothing about
trading, market data, or that monorepo belongs in here — it is a queue.

## Quality gates

Everything below must pass before a change is done. CI runs all of it plus `cargo deny`.

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test && cargo test --features fast-copy
cargo doc --no-deps --all-features
cargo +nightly miri test --test behaviour -- --skip file_backed --skip open_rejects
```

`rust-toolchain.toml` pins stable. Miri needs nightly, so the miri job deletes that file
before `cargo miri setup`; do the same locally or the pin silently overrides `+nightly`.
Miri also needs `MIRIFLAGS=-Zmiri-disable-isolation` and cannot map files, which is why
the file-backed tests are skipped there.

## What this crate is careful about

Read `CONTRIBUTING.md` first; it states the review rules. In short:

- **`unsafe` has two homes and two callers.** The unsafe functions live in `mapping.rs`
  and `copy.rs`. `producer.rs` and `consumer.rs` call them, each call in its own `unsafe`
  block with a `// SAFETY:` comment naming the invariant. Nothing else may contain the
  keyword.
- **Every mapping pointer derives from one write-capable base.** `mapping.rs` takes a
  single `*mut u8` from `as_mut_ptr()` and offsets from it. Taking a pointer from a shared
  slice reference instead is undefined behaviour under Stacked Borrows, and Miri catches
  it. This was a real bug, fixed in 0f79bed.
- **The fences are load-bearing.** A `fence(Release)` after storing the reserved counter
  and a `fence(Acquire)` before the second overrun check are what make the double-check
  sound. They look removable and are not.
- **Miri is the gate, not an advisory.** If a change makes Miri unhappy on default
  features, the change is wrong. The sound copy path existing at all is the reason this is
  a Rust crate rather than a port.
- **The overrun property is the contract.** `tests/overrun_property.rs` stalls a reader
  until the writer has lapped it, then checks every accepted frame. A frame accepted as
  valid that was not written as one frame is the most serious class of bug here.
- **`reserve_block` is derived, not constant.** It is `capacity / 16`. A fixed block
  larger than a small ring reserves past the end and reports overruns that never happened.

## Benchmarks

`cargo bench` and the README table. Two standing rules from Hicham:

- Run them on an **idle** machine and say so, with the CPU and toolchain, next to the
  numbers.
- Every number in the README must be reproducible by the reader with a command that is
  written down. No unsourced figures.

The bench harness reports writer rate, slowest-reader rate, and overrun count. Reporting a
single aggregate rate hides the thing the queue is about.

## Repository conventions

- Dual licensed MIT OR Apache-2.0. Fresh history; never reference the monorepo it came
  from, by name or by path.
- `CHANGELOG.md` follows Keep a Changelog. **Dependency bumps get a changelog entry too**,
  saying what the upgrade needed, not just the version pair.
- Conventional-commit subjects. Do not commit unless Hicham asks.
- Run `gitleaks protect --staged` before every push.
- Dependabot must not bump `dtolnay/rust-toolchain`: that tag names a Rust release, not an
  action version, so a bump asks CI to install a toolchain that does not exist. The ignore
  rule is in `.github/dependabot.yml` and stays.
- `gh pr checks` returns nothing for this repo. Read status from
  `gh run list --json conclusion` and `gh run view <id> --json jobs` instead.
