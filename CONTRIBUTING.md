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
cargo +nightly miri test --lib
cargo +nightly miri test --test behaviour -- --skip file_backed --skip open_rejects
```

CI runs all of these plus an MSRV check against Rust 1.89 and `cargo deny check`.

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
- **Malformed input gets a unit test, not just an integration test.** `tests/malformed.rs`
  crafts queue *files*, which Miri cannot map, so those tests only prove that an `Err` came
  back. The equivalents in `src/consumer.rs` craft the same states over anonymous memory
  and therefore run under Miri, which is what proves no pointer was formed out of bounds.
  A sanitiser is not an alternative: ASan guards `malloc` allocations with redzones and
  knows nothing about the logical end of an `mmap` region, so a read past the ring but
  inside the mapping is invisible to it. Verified — removing the ring-end check makes Miri
  report undefined behaviour at `copy.rs` one word past the mapping, and makes ASan report
  nothing at all.
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

## Working on `main`

`main` is protected. Changes arrive by pull request:

- **A pull request is required.** No approvals are needed — there is one maintainer, and
  GitHub does not let anyone approve their own pull request, so requiring one would lock
  the repository. Review still happens; it just is not enforced by a counter.
- **All six checks must pass**: `fmt + clippy`, `test`, `miri (sound copy)`, `rustdoc`,
  `msrv (1.89)`, `cargo-deny`. The branch must also be up to date with `main` first.
- **History stays linear.** Merge commits are disabled, so merge by **rebase** when the
  commits are each worth keeping, and by **squash** when they are `wip`-and-fixup noise.
  Prefer rebase: every commit on `main` should build and pass on its own, because
  bisecting a soundness regression is a real thing that happens here.
- Conversations must be resolved before merging, and merged branches delete themselves.
- Force pushes and branch deletion are blocked.

Administrators can push to `main` directly. That exists so `cargo release` can push a
release commit without opening a pull request for it; it is not an invitation to skip the
checks.

If a job is ever renamed, the protection rule must be updated in the same breath — a
required check that no longer reports blocks every pull request forever, waiting for
something that cannot arrive.

## Releasing

Releases are cut from `main` by pushing a tag. The tag is the trigger; everything else is
one ordinary commit beforehand.

### The steps

1. **Write the changelog as you go.** `cargo release` moves the `## [Unreleased]` heading
   but never writes what goes under it. Releasing with that section empty produces a dated
   heading with nothing beneath it and a GitHub release with no notes.
2. **Decide the version.** This is the judgement call and it is not automated. Pre-1.0, a
   breaking change bumps the minor — and breaking includes things no tool infers from a
   commit subject: a bumped MSRV, or a bumped `layout::VERSION`, which stops every
   existing queue file opening.
3. **Run `cargo release`.** It bumps `Cargo.toml`, dates the `## [Unreleased]` section and
   opens a fresh one, moves the changelog links, runs the tests, commits as
   `chore: release vX.Y.Z`, tags, and pushes.

   ```bash
   cargo release 0.3.0            # dry run: prints every edit and changes nothing
   cargo release 0.3.0 --execute  # do it
   ```

   Pull `main` first. Rebase and squash merges both rewrite commits, so a local `main`
   that merged a pull request through the web interface has diverged, and `cargo release`
   refuses to run from there.

`release.toml` holds the configuration, including `publish = false`.

### What happens next

Pushing the tag triggers `.github/workflows/release.yml`, which:

1. refuses the tag if it does not match the version in `Cargo.toml` — the release action
   reads `CHANGELOG.md` but never looks at `Cargo.toml`, so nothing else would catch it;
2. runs `cargo package`, which builds the crate from exactly the files that would ship;
3. hands over to [`taiki-e/create-gh-release-action`], which takes the notes from the
   matching `CHANGELOG.md` section and fails if there is no such section, so the release
   and the changelog cannot disagree.

It deliberately does **not** re-run the test suite. Releases come from `main`, every commit
there has passed the full matrix, and this workflow runs once per release — its build cache
is always cold, so repeating the suite costs ten minutes to learn what CI reported minutes
earlier. There is no binary matrix either, unlike repositories that ship executables: this
crate is a library and has no `[[bin]]`.

A pre-release tag such as `v0.2.0-rc1` is published as a pre-release automatically.

[`taiki-e/create-gh-release-action`]: https://github.com/taiki-e/create-gh-release-action

### Doing it by hand

The same two files — `Cargo.toml`'s `version`, and `CHANGELOG.md`'s heading plus the two
link definitions at the bottom — committed as `chore: release vX.Y.Z`, then:

```bash
git tag -a vX.Y.Z -m vX.Y.Z
git push origin main vX.Y.Z
```

Push the branch before or with the tag. The workflow checks out the tag independently, but
a tag that lands first publishes notes whose `[Unreleased]` compare link 404s until the
branch catches up.

### Two things that will surprise you

**A tag event uses the workflow file from the tagged commit**, not from `main`. Fixing
`release.yml` does nothing for tags that already exist, and a tag created before a fix
lands will keep running the old version. `v0.1.0` and `v0.2.0` were both released by hand
with `gh release create` for this reason.

**Nothing is published to crates.io.** If that changes it becomes a step in this workflow
behind a `CARGO_REGISTRY_TOKEN` secret, and it is worth remembering that a published
version can be yanked but never replaced or reused — which is why it is not a command
anyone can run locally.

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`,
`perf:`, `refactor:`, `test:`, `chore:`.

## Licence

Contributions are dual-licensed under MIT and Apache-2.0, matching the crate.
