# outcry

[![CI](https://github.com/anaxo-io/outcry/actions/workflows/ci.yml/badge.svg)](https://github.com/anaxo-io/outcry/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![MSRV](https://img.shields.io/badge/MSRV-1.85-blue.svg)](https://blog.rust-lang.org/)

One writer, many readers, shared memory, nobody waits.

`outcry` is a broadcast queue: a single producer appends variable-length frames, and any
number of consumers — in the same process or in others — each read every frame in order,
at their own pace. Readers never slow the writer. A reader that falls more than a full
queue behind is **overrun**: it is told so, and it can resync to the head, but the writer
never knew it was there. The name is from *open outcry*, the trading-floor protocol: one
voice, everyone on the floor hears it, nobody repeats it for you.

It is a Rust implementation of the `FastQueue` design David Gross (Optiver) presented in
[*When Nanoseconds Matter: Ultrafast Trading Systems in C++*][talk] at CppCon 2024,
including the three cache-line optimisations that took his version from 12 to 31 million
messages per second. It differs from the original in one deliberate way, described below.

[talk]: https://www.youtube.com/watch?v=sX2nF1fW7kI

## Quick start

```toml
[dependencies]
outcry = { git = "https://github.com/anaxo-io/outcry", tag = "v0.1.0" }
```

One process writes:

```rust
use outcry::Queue;

fn main() -> Result<(), outcry::Error> {
    let queue = Queue::create("/dev/shm/prices", 8 << 20)?;   // 8 MiB ring
    let mut producer = queue.producer()?;
    for tick in 0..1_000u32 {
        producer.write(format!("BTC-USD {tick}").as_bytes())?;
    }
    Ok(())
}
```

Any number of others read, each getting every frame:

```rust
use outcry::Queue;

fn main() -> Result<(), outcry::Error> {
    let queue = Queue::open("/dev/shm/prices")?;
    // A consumer starts at the current head, so start it before the frames you want:
    // whatever the producer wrote earlier is already gone.
    let mut consumer = queue.consumer();

    let mut buf = [0u8; 256];
    loop {
        match consumer.try_read(&mut buf)? {   // `?` here exits on Overrun;
            Some(n) => println!("{}", String::from_utf8_lossy(&buf[..n])),
            None => std::hint::spin_loop(),    // a real reader calls consumer.resync()
        }
    }
}
```

`Queue::anon(capacity)` gives the same thing inside one process, for tests or for threads;
the crate docs open with that version, as a doctest. Both halves above are in the tree as
`examples/readme_writer.rs` and `examples/readme_reader.rs`, so they are compiled by
`cargo test`; `examples/pubsub.rs` runs a producer and consumers as separate processes.

## How it works

The ring is a file (or anonymous memory) with a 64-byte header and two counters, each on
its own cache line, **both written only by the producer**:

- `published` — bytes readers may consume;
- `reserved` — bytes the producer has claimed, including a write in flight.

Positions are monotonic byte offsets; the ring index is `pos & (capacity − 1)`. To write,
the producer bumps `reserved` (release), copies the length-prefixed frame, then bumps
`published` (release). To read, a consumer checks `published` (acquire), copies the frame,
and then checks `reserved` **before and after the copy**: if the producer's reservation
has passed a full ring beyond the frame's start at any point, the bytes it just copied may
have been overwritten underneath it, and it reports `Overrun` instead of returning them.
Torn reads are detected after the fact rather than prevented — the seqlock discipline,
with the version counter replaced by a byte offset.

The three optimisations from the talk, all aimed at keeping the hot path off shared cache
lines: the producer publishes `reserved` in blocks of one sixteenth of the ring rather
than per message; payloads are padded to 8 bytes; the consumer caches `published` and
only re-reads it once it has consumed everything it last saw.

## Where this differs from the original

**The copy is sound.** In the C++ original the consumer `memcpy`s a frame the
producer may be overwriting at that moment. That is a data race in the abstract machine —
the talk says so on its own slide — and a data race is undefined behaviour in Rust as in
C++, however well it behaves on real hardware. So the default copy goes word by word
through `AtomicU64` with `Relaxed` ordering: concurrent atomic accesses are never a data
race, and the acquire/release on the counters orders them. Miri runs this path in CI.

The `memcpy` version was implemented, benchmarked and then removed rather than shipped
behind a feature flag; the numbers below say why.

**Two smaller differences.** The consumer's second overrun check compares against the
*start* of the frame it read, not the advanced position; the original's check leaves a
window of one frame in which an overwrite of the frame's first bytes goes unnoticed. And
overrun is an error value, not an `EXPECT`: the reader decides whether to resync or stop.

## Performance

```bash
cargo bench --bench throughput                  # scaling across reader counts
cargo bench --bench latency                     # writer-to-reader hand-off, percentiles
OUTCRY_PIN=2,4 OUTCRY_READERS=1 cargo bench --bench throughput   # writer on core 2, reader on 4
OUTCRY_READERS=1,2,4 OUTCRY_MESSAGES=5000000 cargo bench --bench throughput
```

`OUTCRY_PIN` pins the writer to the first core listed and each reader to the next. Use it
for anything you intend to quote. Unpinned, the scheduler decides per run how far apart
the writer and reader sit in the cache hierarchy, and on a CPU with more than one L3 the
two outcomes differ by 5x — a single unpinned sample is close to worthless. Before
choosing cores, read `/sys/devices/system/cpu/cpu0/cache/index3/shared_cpu_list` for the
L3 grouping and `cpu0/topology/thread_siblings_list` for which logical CPUs are two
threads of one physical core.

The benchmark reproduces the talk's setup: 73-byte messages, an 8 MiB ring, N readers each
on its own thread, the producer writing as fast as it can and never waiting. Three figures
per row: the writer's rate, the *slowest* reader's sustained rate, and how many times any
reader was lapped. Zero overruns means every reader saw every frame, so the writer's
figure is a real broadcast rate rather than a writer talking to itself.

### Reference machine

Quote these. A Scaleway Elastic Metal EM-A116X, rentable by the hour for about €0.08, so
you can reproduce them rather than trust them:

- Intel Xeon E3-1231 v3 (Haswell, 4 cores / 8 threads, 8 MiB L3 in **one** instance, so
  every core pairing sits in the same cache domain)
- Ubuntu 26.04 LTS, kernel 7.0.0-15-generic, rustc 1.98.1
- booted with `isolcpus=2-7 nohz_full=2-7 rcu_nocbs=2-7`, governor `performance`; the
  operating system is confined to core 0, and cores 2, 4 and 6 run nothing else
- 3 million messages per row, 1 million samples per latency run, **median of 7 runs with
  the range across them**

| readers | pinning | writer msg/s | range | slowest reader | overruns |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | `2,4` | 20.37 M | 19.50 – 21.52 M | 20.37 M | 0 |
| 2 | `2,4,6` | 16.26 M | 15.32 – 17.90 M | 16.25 M | 0 |

Latency is the other half, and the half this queue is really for. The writer sends one
frame every microsecond so the reader is idle when each lands; every frame carries a
timestamp and the reader records its age on arrival. Two `clock_gettime` calls through the
vDSO, roughly 20 ns each, are inside the figure and cannot be separated from it.

| placement | p50 | p90 | p99 | p99.9 | p99.99 | max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| separate cores (`2,4`) | **149 ns** | 161 ns | 179 ns | 1.65 µs | 12.3 µs | 72 µs |
| SMT siblings (`2,3`) | **96 ns** | 105 ns | 116 ns | 1.71 µs | 19.0 µs | 85 µs |

p50 moved by 3 ns across the seven separate-core runs and by 1 ns across the sibling runs.
That stability is what the isolated cores buy; everything from p99.9 upward is the
operating system, and on a kernel without `isolcpus` it is worse.

**Two hyperthreads of one core are the fastest placement, not a mistake.** They share L1,
so the hand-off never reaches L3: 96 ns against 149 ns. The cost is predictability — across
seven runs the sibling p99.99 ranged from 5 µs to 50 µs while separate cores held 11.9 to
16.6 µs — and throughput, since the two threads then compete for one core's execution
resources. Pick siblings for latency, separate cores for throughput and for calm tails.

### Scaling with readers

The reference machine has four cores, one of them reserved for the operating system, so it
cannot hold more than a writer and two readers on isolated cores. The shape of the curve
comes from a second machine and is reported separately rather than blended in: an AMD Ryzen
7 1800X (Zen 1, 8 cores / 16 threads), Rust 1.98.1, an idle desktop with no isolation and
no pinning, median of 7 runs.

| readers | writer msg/s | range | slowest reader | overruns |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 7.44 M | 7.24 – 8.16 M | 7.44 M | 0 |
| 2 | 5.52 M | 5.38 – 7.16 M | 5.52 M | 0 |
| 3 | 5.32 M | 5.00 – 5.44 M | 5.32 M | 0 |
| 4 | 4.70 M | 4.45 – 5.13 M | 4.70 M | 0 |
| 6 | 4.09 M | 3.93 – 4.16 M | 4.09 M | 0 |
| 8 | 3.92 M | 3.89 – 4.04 M | 3.92 M | 0 |
| 12 | 3.86 M | 3.79 – 3.98 M | 3.86 M | 0 |

Read the shape, not the heights: the cost per reader falls away as readers are added,
because the ring's cache lines are already being pulled by somebody. These are unpinned
numbers on a two-complex CPU, which is exactly the situation the section above warns about;
they are here because no single-socket four-core machine can produce a twelve-reader row.

The same desktop, pinned, shows how much of that is placement rather than the queue: with
writer and reader in one complex the one-reader figure is 57.2 M (29.7 – 63.9 M, and two of
the seven runs recorded overruns, meaning the writer had lapped the reader and the number
is no longer an end-to-end rate); across complexes it is 8.1 M (7.8 – 8.8 M, no overruns).
The reference machine's 20.37 M is slower than that best case and is the honest figure,
because every frame in it was delivered.

### What to read off it

- **The unsound copy was slower, everywhere.** A `ptr::copy_nonoverlapping` variant — the
  original's trade, a data race caught after the fact — was measured against the sound path
  from the same commit, 7 runs each, and then deleted. It never won a single row:

  | readers | 1 | 2 | 3 | 4 | 6 | 8 | 12 |
  | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
  | `memcpy` ÷ atomic words | 0.65x | 0.72x | 0.77x | 0.83x | 0.85x | 0.81x | 0.80x |

  The mechanism is that these payloads are small. A 73-byte `copy_nonoverlapping` is a call
  into a generic `memcpy` that dispatches on length; the atomic word loop inlines to ten
  plain `mov`s, because a `Relaxed` load or store of a `u64` on x86-64 *is* a `mov`. The
  soundness is free here, and the race bought nothing. The code is in history
  (`git log -S copy_nonoverlapping -- src/copy.rs`) — reproduce with
  `cargo bench --bench throughput --features fast-copy` at that revision.
- **These are not the talk's numbers, and are not meant to compete with them.** The talk's
  31 M msg/s at two readers is on an isolated, tuned EPYC 9474F. The shape of the curve is
  what transfers, not the height.
- **Slowest reader ≈ writer, always.** Across every row measured for this README the
  slowest reader stayed within 0.04% of the writer, and no reader was ever lapped except in
  the pinned desktop runs noted above. The overrun path is exercised by the property test
  instead, where lapping is forced on purpose.
- **Measure on your own topology.** Every figure here is a statement about one cache
  hierarchy. The commands are written down so that you can replace them with yours, which
  is the only version that should inform a decision.

## When not to use this

- **More than one writer.** Not supported, and only enforced within a process. Two
  processes calling `producer()` on the same file corrupt it.
- **Work distribution.** Every reader gets every frame. For "each message to one worker",
  use an MPMC queue: [`crossbeam`](https://crates.io/crates/crossbeam) in-process,
  [`shaq`](https://crates.io/crates/shaq) across processes.
- **Readers that must not lose data.** A slow reader is overrun, not waited for. If the
  writer must back off, you want a bounded channel.
- **Across hosts.** Shared memory stops at the machine. UDP multicast or
  [Aeron](https://aeron.io) from there.
- **Durability.** The ring is memory. Nothing survives the file being removed.
- **A full IPC framework** — service discovery, request/response, typed payloads:
  [`iceoryx2`](https://crates.io/crates/iceoryx2). This crate is the transport only.

## Tests

`cargo test` covers round trips, frames at every offset relative to the ring end, late
joiners, many readers, `BufferTooSmall`, and overrun-then-resync. The test that matters is
`tests/overrun_property.rs`: a reader repeatedly stalls until the writer has lapped it,
resumes while the writer is still rewriting that region, and every frame it accepts is
verified against a checksum in its payload. It runs in both copy modes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Changes to the hot path come with before/after
benchmark tables.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this crate by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
