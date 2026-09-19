# CLAUDE.md

Guidance for Claude Code working in this repository.

## What this is

`outcry` is a single-writer, many-reader broadcast queue over POSIX shared memory: a Rust
implementation of the FastQueue design from David Gross's CppCon 2024 talk *When
Nanoseconds Matter*. Readers never block the writer; a reader that falls too far behind is
overrun and told so, rather than handed a torn frame.

Nothing about trading or market data belongs in here — it is a queue. Keep the examples
and the prose generic; the trading-floor metaphor in the name is as far as it goes.

## Quality gates

Everything below must pass before a change is done. CI runs all of it plus `cargo deny`.

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo doc --no-deps --all-features
cargo +nightly miri test --lib
cargo +nightly miri test --test behaviour -- --skip file_backed --skip open_rejects
```

`rust-toolchain.toml` pins stable. Miri needs nightly, so the miri job deletes that file
before `cargo miri setup`; do the same locally or the pin silently overrides `+nightly`.
Miri also needs `MIRIFLAGS=-Zmiri-disable-isolation` and cannot map files, which is why
the file-backed tests are skipped there.

## What this crate is careful about

`CONTRIBUTING.md` has the review rules in full, and they apply here too — read it before
touching `src/`. The short version: `unsafe` lives only in `mapping.rs` and `copy.rs`, with
a `// SAFETY:` comment at every call site in `producer.rs` and `consumer.rs`; every mapping
pointer derives from one write-capable base; the fences around the overrun double-check are
load-bearing; `Queue::open` validates a file it does not trust before forming a pointer
from it; Miri on the default features is a gate, not an advisory; and
`tests/overrun_property.rs` plus `tests/malformed.rs` are the contract.

## Benchmarks

`cargo bench` and the README table. Two standing rules:

- Run them on an **idle** machine and say so, with the CPU and toolchain, next to the
  numbers.
- Every number in the README must be reproducible by the reader with a command that is
  written down. No unsourced figures.

The bench harness reports writer rate, slowest-reader rate, and overrun count. Reporting a
single aggregate rate hides the thing the queue is about.

## Repository conventions

- Dual licensed MIT OR Apache-2.0.
- `CHANGELOG.md` follows Keep a Changelog. **Dependency bumps get a changelog entry too**,
  saying what the upgrade needed, not just the version pair.
- Conventional-commit subjects. Do not commit unless asked.
- Releases go out by pushing a `vX.Y.Z` tag; `.github/workflows/release.yml` builds the
  GitHub release from the matching `CHANGELOG.md` section and fails if the tag, the
  `Cargo.toml` version and that section disagree. The procedure is in `CONTRIBUTING.md`.
- Run `gitleaks protect --staged` before every push.
- Dependabot must not bump `dtolnay/rust-toolchain`: that tag names a Rust release, not an
  action version, so a bump asks CI to install a toolchain that does not exist. The ignore
  rule is in `.github/dependabot.yml` and stays.
- `gh pr checks` returns nothing for this repo. Read status from
  `gh run list --json conclusion` and `gh run view <id> --json jobs` instead.
