# iroh 1.0.3 -> 1.2.0 swarm-fetch bench (Wave 1 task A7)

Machine: Apple M4, macOS 26.5.2 (Darwin 25.5.0, arm64). Runs made 2026-09-21
on the tree dated 2026-09-20 (this directory's date names the task/cycle,
not the calendar day the numbers were taken).

Command (both runs, `--label` only difference):

```
cargo run -p athenaeum-core --release --example swarm_bench -- \
  --label <before|after> --files 12 --size-mb 32 --out target/swarm-bench
```

Scratch cost: at these defaults each of the two runs (`single`, `multi`)
writes 384 MB of source payload PLUS a copy into its own package dir
(`write_package` copies, it never moves) — roughly 1.5 GB total for both
runs — plus three iroh blob stores (one per node), all under the OS temp
dir and removed when the process's tempdir guards drop.

`single` = one provider (A); `multi` = two providers (A+B), a DIFFERENT
package from `single` served by both. `multi_two_providers_mb_s` is the
number the 10 % regression gate in the task brief is keyed on. **`single`
and `multi` are not a controlled comparison with each other**: `single`
runs first and pays C's cold connection setup to A, `multi` reuses that
already-warm A connection and only pays a cold C<->B setup — only the same
column compared before vs. after (same code path, same warm/cold shape) is
controlled.

| label  | iroh   | single_mb_s | multi_two_providers_mb_s |
| ------ | ------ | -----------:| -------------------------:|
| before | 1.0.3  |       96.71 |                      70.35 |
| after  | 1.2.0  |       84.45 |                      99.67 |

`multi_two_providers_mb_s` moved +41.7 % (no regression; several interleaved
re-runs on both sides of the bump landed in the 70-113 MB/s band — this Mac's
short-run noise floor for a 384 MB in-process localhost transfer, see
`before.json`/`after.json` here for the exact numbers this table is built
from).

## Dependency record (Cargo.lock, `9503a13f..f8e5f96a`)

Generated from `git diff 9503a13f..f8e5f96a -- Cargo.lock` itself (every
`[[package]]` name/version the lockfile actually added, removed, or moved),
not a hand-picked subset. `iroh-blobs` stayed pinned at `=0.103.0` per the
task brief.

**Bumped**

| crate | before | after |
| ----- | ------ | ----- |
| `iroh` | 1.0.3 | 1.2.0 |
| `iroh-base` | 1.0.3 | 1.2.0 |
| `iroh-relay` | 1.0.3 | 1.2.0 |
| `iroh-dns` | 1.0.3 | 1.3.0 |
| `noq` | 1.1.0 | 1.3.0 |
| `noq-proto` | 1.1.0 | 1.3.0 |
| `noq-udp` | 1.1.0 | 1.3.0 |
| `netwatch` | 0.19.1 | 0.19.3 |
| `portmapper` | 0.19.1 | 0.19.3 |
| `simple-dns` | 0.11.3 | 0.12.0 |

**Added** (transitive, pulled in by `iroh-dns` 1.3.0's own dependency tree):
`n0-dns-resolver` 0.1.0, `netdev` 0.46.3, `netlink-packet-core` 0.9.0,
`netlink-packet-route` 0.33.0, `netlink-sys` 0.9.0, `system-configuration`
0.8.0, `zerocopy` 0.8.57, `zerocopy-derive` 0.8.57.

**Removed** (the old DNS-resolver stack `iroh-dns` 1.0.3 used, superseded by
the above): `hickory-net` 0.26.1, `hickory-proto` 0.26.1, `hickory-resolver`
0.26.1, `moka` 0.12.15, `prefix-trie` 0.8.4, `resolv-conf` 0.7.6, `tagptr`
0.2.0.

Unchanged: `iroh-tickets` 1.0.0, `iroh-util` 0.6.0, `irpc` 0.17.0,
`ed25519-dalek` 3.0.0, `n0-future` 0.3.2.

## Not covered by this bench

Every node in this bench binds `RelayMode::Disabled` and exchanges
addresses out of band via pairing tickets, so relay, NAT traversal/port
mapping (`netwatch`, `portmapper`, both bumped above) and DNS-based
discovery (the whole `hickory-*` -> `n0-dns-resolver`/netlink swap above)
are exercised by NO wave-1 gate — this bench included. A real-network smoke
across the test relay is owed (owner-run) before this branch is pushed.
