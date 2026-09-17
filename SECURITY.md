# Security Policy

## Supported versions

The latest released version receives security fixes. This crate has not yet reached 1.0;
the API may change between minor versions.

## Reporting a vulnerability

Report security issues privately to **security@anaxo.io**. Please do not open a public
issue.

Include a description of the issue, the version or commit affected, and a reproduction —
ideally a failing test.

You will receive an acknowledgement within 72 hours. Fixes are disclosed publicly once
released, or after 90 days, whichever comes first.

## Scope

This crate maps memory shared between processes and reads it concurrently with a writer.
That is its whole job, and it is where the risk is.

In scope:

- **Memory safety.** Any way to make the default (sound) configuration read or write
  outside the mapping, or to exhibit undefined behaviour under Miri.
- **A corrupt frame delivered as valid.** The overrun check must catch every lapped read.
  A sequence of writes and reads in which `try_read` returns `Ok` with bytes that were not
  written as one frame is a bug of the most serious kind for this crate.
- **Header parsing.** `Queue::open` reads an untrusted file. A crafted header that causes
  a panic, an out-of-bounds access, or a mapping of the wrong size is in scope.

Out of scope:

- Another process with write access to the queue file can corrupt it. Shared memory has
  the trust model of a shared file; protect it with file permissions.
- Two producers on one file corrupt it. That is documented and enforced only within a
  process.
