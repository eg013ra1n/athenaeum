# iroh 1.0.3 -> 1.2.0 swarm-fetch bench (Wave 1 task A7)

Machine: Apple M4, macOS 26.5.2 (Darwin 25.5.0, arm64), 2026-09-21.

Command (both runs, `--label` only difference):

```
cargo run -p athenaeum-core --release --example swarm_bench -- \
  --label <before|after> --files 12 --size-mb 32 --out target/swarm-bench
```

`single` = one provider (A); `multi` = two providers (A+B), same package
served by both — this is the number the 10 % regression gate in the task
brief is keyed on.

| label  | iroh   | single_mb_s | multi_two_providers_mb_s |
| ------ | ------ | -----------:| -------------------------:|
| before | 1.0.3  |       96.71 |                      70.35 |
| after  | 1.2.0  |       84.45 |                      99.67 |

`multi_two_providers_mb_s` moved +41.7 % (no regression; several interleaved
re-runs on both sides of the bump landed in the 70-113 MB/s band — this Mac's
short-run noise floor for a 384 MB in-process localhost transfer, see
`before.json`/`after.json` here for the exact numbers this table is built
from). `iroh-blobs` stayed pinned at `=0.103.0` per the task brief; only
`iroh`/`noq`/`iroh-relay`/`iroh-base`/`iroh-dns`/`noq-proto`/`noq-udp` moved.
