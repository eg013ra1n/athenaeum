# ZIP probe — phase-0 measurement

Date: 2026-10-27. For `docs/superpowers/specs/2026-10-27-zip-archives-in-catalog-design.md` §12.
Probe: `crates/athenaeum-core/examples/zip_probe.rs`
(`cargo run --release -p athenaeum-core --no-default-features --example zip_probe -- --buf KIB ZIP...`).

## Setup

- Share: the owner's SMB share Universe (`/Volumes/Universe`), dev Mac on the home network.
- The probe opens the zip with `F_NOCACHE`, so the Mac's page cache does not serve the reads. It counts every byte and
  read call below its `BufReader`, so "read" means bytes that crossed the network.
- The zips were written to the share just before the run (`athenaeum-temp/zip-probe`, deleted afterwards). The NAS
  RAM probably still held them, so **the times are optimistic**. A first scan of an old archive reads from cold NAS
  disks. Byte counts do not depend on this.

| Zip | Contents | Size | Method |
| ---- | ---- | ---- | ---- |
| `flats-h-2022.zip` | the owner's real 2022 archive: 20 H flats, 38.5 MiB each | 438.5 MiB | Deflate (macOS Archive Utility) |
| `gaia-stored.zip` | a real capture folder: 83 lights of 49.8 MiB plus 84 jpg thumbnails | 4132.7 MiB | Stored (`zip -0`) |
| `gaia-deflate.zip` | the same folder | 2195.8 MiB | Deflate (`zip -6`) |

## Results

Network bytes per zip, by the header-pass buffer size:

| Zip | Buffer | Central directory | Headers (all frames) | Per frame | Total, % of zip |
| ---- | ---- | ---- | ---- | ---- | ---- |
| flats-h-2022 | 4 KiB | 33 KB, 23 reads, 21 ms | 0.23 MiB, 28 ms | 12 KiB for 2.8 KiB of header | 0.06% |
| flats-h-2022 | 16 KiB | 33 KB | 0.62 MiB | 32 KiB | 0.15% |
| flats-h-2022 | 64 KiB | 33 KB | 2.50 MiB | 128 KiB | 0.58% |
| gaia-stored | 4 KiB | 0.64 MB, 172 reads, 90 ms | 1.29 MiB, 190 ms | 16 KiB for 8.4 KiB of header | 0.05% |
| gaia-stored | 64 KiB | 2.30 MB, 174 ms | 10.38 MiB | 128 KiB | 0.30% |
| gaia-deflate | 4 KiB | 0.64 MB, 170 reads, 90 ms | 0.97 MiB, 87 ms | 12 KiB | 0.07% |
| gaia-deflate | 64 KiB | 2.32 MB, 171 ms | 10.38 MiB | 128 KiB | 0.57% |

An earlier local run on `flats-h-2022` (external SATA disk) with a 1 MiB buffer read 40 MiB, which is 9.1% of the zip.

Full extraction of one frame entry. Here the reader is separate and uses a 1 MiB buffer; CRC is checked and xxh3
streamed alongside:

| Zip | Entry | Time | Entry rate | Rate off the share |
| ---- | ---- | ---- | ---- | ---- |
| flats-h-2022 | 38.5 MiB (21.9 compressed, ratio 0.57) | 0.34 s | 112 MiB/s | 67 MiB/s |
| gaia-stored | 49.8 MiB (ratio 1.00) | 0.53 s | 94 MiB/s | 97 MiB/s |
| gaia-deflate | 49.8 MiB (29.7 compressed, ratio 0.60) | 0.46 s | 109 MiB/s | 68 MiB/s |

The extracted bytes hash identically from the Stored and the Deflate zip (xxh3 `4141311a942bb77d`).

## Findings

1. **The header pass needs a small read buffer.** Every entry costs about two buffer fills: the local header, then the
   data start after a seek. That makes the per-frame cost about 2 × buffer. At 4 KiB the whole metadata scan reads
   0.05–0.07% of the zip; at 64 KiB it reads 0.3–0.6%; at 1 MiB it reads 9%. The spec's "1–4 MB buffer" line was wrong
   for the scan and right only for extraction. Use about 4 KiB for the header pass and about 1 MiB for extraction.
2. **Opening a zip costs one read per entry.** `zip` 8.6 `ZipArchive::new` touches every entry: 23 reads for 20
   entries, 172 for 167. With a 4 KiB buffer that is about 4 KB per entry, roughly 0.5 ms per entry on this LAN. A zip
   of 1000 entries opens in about half a second. That is acceptable, but it is O(entries) and not "a few kilobytes".
3. **Deflate barely shrinks 16-bit FITS.** Ratio 0.57–0.60 (Archive Utility and `zip -6` alike), so Deflate zips are
   real in the wild: the owner's own is one.
4. **Extraction runs at about the share's line rate.** Stored gives 94–97 MiB/s off the share, which is about gigabit.
   Deflate gives 109–112 MiB/s of entry for 68 MiB/s off the share, so it beats Stored per frame because less crosses
   the wire. A 500-light night of 50 MiB frames (about 25 GB) takes roughly 4 minutes to extract on this LAN, and
   longer from cold NAS disks.
5. **Header sizes.** 2.8 KiB for the flats (one FITS block), 8.4 KiB for the lights (three blocks). Reading until `END`
   is the right stop rule; a fixed read size would either waste bytes or cut headers.

## Bars for acceptance (spec §13)

- Metadata scan of a zip: total network reads at most 64 KiB per entry plus the central directory, and under 0.5% of
  the zip size for frames of 20 MiB or more. Measured: 12–16 KiB per frame, 0.05–0.07%.
- Extraction: at least 50% of the share's measured sequential read rate. Measured: about 70–100%.
