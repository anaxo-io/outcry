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

```rust
use outcry::Queue;

fn main() -> Result<(), outcry::Error> {
    // In one process: create the queue in shared memory and write to it.
    let queue = Queue::create("/dev/shm/prices", 8 << 20)?;
    let mut producer = queue.producer()?;
    producer.write(b"BTC-USD 79134.01")?;

    // In any other process: attach and read. A reader starts at the current head.
    let queue = Queue::open("/dev/shm/prices")?;
    let mut consumer = queue.consumer();
    let mut buf = [0u8; 256];
    while let Some(n) = consumer.try_read(&mut buf)? {
        println!("{}", String::from_utf8_lossy(&buf[..n]));
    }
    Ok(())
}
```

`Queue::anon(capacity)` gives the same thing inside one process, for tests or for threads.
`examples/pubsub.rs` runs a producer and consumers as separate processes.

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

**The copy is sound by default.** In the C++ original the consumer `memcpy`s a frame the
producer may be overwriting at that moment. That is a data race in the abstract machine —
the talk says so on its own slide — and a data race is undefined behaviour in Rust as in
C++, however well it behaves on real hardware. So the default copy goes word by word
through `AtomicU64` with `Relaxed` ordering: concurrent atomic accesses are never a data
race, and the acquire/release on the counters orders them. Miri runs this path in CI.

The `fast-copy` feature restores `ptr::copy_nonoverlapping` and the original's trade. The
section below is the measured cost of choosing soundness; decide with the numbers.

**Two smaller differences.** The consumer's second overrun check compares against the
*start* of the frame it read, not the advanced position; the original's check leaves a
window of one frame in which an overwrite of the frame's first bytes goes unnoticed. And
overrun is an error value, not an `EXPECT`: the reader decides whether to resync or stop.

## Performance

```bash
cargo bench --bench throughput                       # sound copy (default)
cargo bench --bench throughput --features fast-copy
```

The benchmark reproduces the talk's setup: 73-byte messages, an 8 MiB ring, N readers each
on its own thread, the producer writing as fast as it can and never waiting. Three figures
per row: the writer's rate, the *slowest* reader's sustained rate, and how many times any
reader was lapped. Zero overruns means every reader saw every frame, so the writer's
figure is a real broadcast rate rather than a writer talking to itself.

Measured on an AMD Ryzen 7 1800X (Zen 1, 2017; 8 cores / 16 threads), Rust 1.98.1, a
desktop with no core isolation or tuning, 3 million messages per row. Both modes, same
run:

| readers | sound — writer msg/s | sound — slowest reader | `fast-copy` — writer msg/s | `fast-copy` — slowest reader | overruns |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 10.46 M | 10.42 M | 17.04 M | 17.04 M | 0 |
| 2 | 5.41 M | 5.41 M | 6.06 M | 6.06 M | 0 |
| 3 | 4.81 M | 4.81 M | 3.97 M | 3.97 M | 0 |
| 4 | 4.31 M | 4.31 M | 4.04 M | 4.04 M | 0 |
| 6 | 3.93 M | 3.93 M | 3.40 M | 3.40 M | 0 |
| 8 | 3.88 M | 3.88 M | 3.20 M | 3.20 M | 0 |
| 12 | 3.74 M | 3.74 M | 2.97 M | 2.97 M | 0 |

What to read off it:

- **The cost of soundness is real with one reader and gone with several.** At one reader
  the atomic word copy costs about 40% (96 ns against 59 ns per message). From three
  readers on, the two modes are within run-to-run noise of each other: once several cores
  are pulling the same cache lines, coherence traffic dominates and how the bytes are
  copied stops mattering. That is the same convergence the talk shows for every queue it
  compares. Unless you have measured a single-reader workload where 40 ns per message
  matters, keep the default.
- **These are not the talk's numbers, and are not meant to compete with them.** His 31 M
  msg/s at two readers is on an isolated, tuned EPYC 9474F; this is a 2017 desktop part
  with a browser open. The shape of the curve is what transfers, not the height.
- **Slowest reader ≈ writer, always.** No reader was ever lapped in this run. The overrun
  path is exercised by the property test instead, where lapping is forced on purpose.

The benchmark harness reports overruns rather than hiding them; if you see a non-zero
count, the writer figure is not a rate your readers can sustain and the reader column is
the honest one.

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
