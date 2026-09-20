//! Dev probe for Tier C item C2 (the drizzle phase table, ruling C-3):
//! times `drizzle_group`'s deposit on REAL calibrated frames with their
//! REAL registration maps, alternating the two overlap arms in ONE process
//! so the two numbers are comparable on a machine that drifts 10-15 %
//! across a session (rulings R-TA-8/R-TA-9 — only interleaved before/after
//! brackets count, and the arbiter is a release build).
//!
//! It reads nothing it writes: the catalog is opened read-only for the
//! frames' `transform_json`, the calibrated planes are read off disk, and
//! the drizzled output is thrown away (the probe reports `deposit_ms`, not
//! pixels — `the_phase_table_tracks_the_exact_clip_on_a_rotated_frame` and
//! its TPS sibling are what pin the two arms' VALUES against each other).
//!
//! ```text
//! cargo run --release -p athenaeum-core --example drizzle_probe -- \
//!   <athenaeum.db> <dir-with-c_*.fits> \
//!   [--limit N] [--scale S] [--drop-shrink D] [--rounds R] [--threads T]
//! ```
//!
//! `--rounds 2` (the default) runs `B A B A`: exact, tabulated, exact,
//! tabulated.
//!
//! `--coverage-out <dir>` (ruling C-23) turns the probe into a COVERAGE
//! comparison as well as a timing one: each arm's union coverage mask
//! (output pixels with `W > 0` after every deposited frame) is accumulated,
//! written bit-packed to `<dir>/coverage_<arm>_<W>x<H>.bits`, and the two
//! are diffed in-process — zero-pixel count per arm, pixels covered by
//! exactly one arm, and for those the split between the covered region's
//! 2-px RIM and its interior. The question it answers is the one the
//! absolute `coverage` row in the acceptance table cannot: that row reads
//! 0.993484 on the mono group for FOOTPRINT reasons (the frames do not
//! cover the whole co-registered canvas), which says nothing about whether
//! the phase table CHANGED the covered set.
//!
//! `--ref <W>x<H>` overrides the reference geometry, which otherwise comes
//! from the first frame's own. A co-registered run's reference is the
//! LARGEST group's frame, so comparing against a real run's weight map
//! needs the run's geometry, not this group's.
//!
//! `--tiled` wraps every frame's map in a small cubic distortion so the
//! deposit resolves to the per-`TILE` arm instead of the per-frame one.
//! The catalog's own registrations carry `distortion: null`, so this is
//! the only way to put the tiled machinery — the tile-row cache, the
//! rebuild at each tile crossing, the local Jacobian by finite differences
//! — on real 6224x4168 geometry and time it. The distortion layer slows
//! BOTH arms equally (every `ForwardEval::at` pays it), so the two numbers
//! stay comparable with each other; they are NOT comparable with the
//! undistorted run's.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use athenaeum_core::geometry::PixelMap;
use athenaeum_core::integration::stats::NormalizationPair;
use athenaeum_core::stacking::drizzle::phase_table::{PhaseTable, PHASES, TILE};
use athenaeum_core::stacking::drizzle::{
    drizzle_group, geom, DrizzleFrame, DrizzleInput, DrizzleKernel, DrizzleProgress,
};
use athenaeum_core::stacking::measure::MeasureOptions;

struct Args {
    db: PathBuf,
    dir: PathBuf,
    limit: usize,
    scale: u32,
    drop_shrink: f64,
    rounds: usize,
    threads: Option<usize>,
    tiled: bool,
    coverage_out: Option<PathBuf>,
    reference: Option<(usize, usize)>,
}

fn parse_args() -> Args {
    let mut positional: Vec<String> = Vec::new();
    let mut limit = 10usize;
    let mut scale = 2u32;
    let mut drop_shrink = 0.9f64;
    let mut rounds = 2usize;
    let mut threads = None;
    let mut tiled = false;
    let mut coverage_out = None;
    let mut reference = None;

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" => limit = it.next().expect("--limit N").parse().expect("N"),
            "--scale" => scale = it.next().expect("--scale S").parse().expect("S"),
            "--drop-shrink" => {
                drop_shrink = it.next().expect("--drop-shrink D").parse().expect("D")
            }
            "--rounds" => rounds = it.next().expect("--rounds R").parse().expect("R"),
            "--threads" => threads = Some(it.next().expect("--threads T").parse().expect("T")),
            "--tiled" => tiled = true,
            "--coverage-out" => {
                coverage_out = Some(PathBuf::from(it.next().expect("--coverage-out DIR")))
            }
            "--ref" => {
                let v = it.next().expect("--ref WxH");
                let (w, h) = v.split_once('x').expect("--ref WxH");
                reference = Some((w.parse().expect("W"), h.parse().expect("H")));
            }
            other => positional.push(other.to_string()),
        }
    }
    assert!(
        positional.len() == 2,
        "usage: drizzle_probe <athenaeum.db> <dir-with-c_*.fits> [--limit N] \
         [--scale S] [--drop-shrink D] [--rounds R] [--threads T]"
    );
    Args {
        db: PathBuf::from(&positional[0]),
        dir: PathBuf::from(&positional[1]),
        limit,
        scale,
        drop_shrink,
        rounds,
        threads,
        tiled,
        coverage_out,
        reference,
    }
}

/// One arm's union coverage mask, one byte per output pixel (`1` = some
/// frame deposited a positive weight there).
struct CoverageMask {
    mask: Vec<u8>,
    width: usize,
    height: usize,
}

impl CoverageMask {
    /// Builds the mask from a `DrizzleOutput`'s weight plane. The plane is
    /// `W / max(W)`, and `max(W) > 0` whenever anything was deposited, so
    /// `> 0` there is exactly `W > 0` — the coverage set itself, not a
    /// thresholded version of it.
    fn from_weight(weight: &[f32], width: usize, height: usize) -> CoverageMask {
        CoverageMask {
            mask: weight.iter().map(|&w| u8::from(w > 0.0)).collect(),
            width,
            height,
        }
    }

    fn zeros(&self) -> usize {
        self.mask.iter().filter(|&&v| v == 0).count()
    }

    /// Writes the mask bit-packed, LSB first within each byte, row-major —
    /// 13 MB for a 12496x8352 grid instead of 104 MB raw. The dimensions
    /// are in the caller's filename, so the file needs no header.
    fn write_bits(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut out = vec![0u8; self.mask.len().div_ceil(8)];
        for (i, &v) in self.mask.iter().enumerate() {
            if v != 0 {
                out[i / 8] |= 1 << (i % 8);
            }
        }
        std::fs::write(path, out)
    }

    /// True when `(x, y)` lies within `RIM_PX` of a pixel this mask does
    /// NOT cover — i.e. on the covered region's own edge. A disagreement
    /// there is the phase residual moving the boundary of the deposited
    /// area by up to `1 / (2 · PHASES)` of a pixel; a disagreement away
    /// from it would be a hole, which is the thing worth knowing.
    fn near_edge(&self, x: usize, y: usize, rim: usize) -> bool {
        let x0 = x.saturating_sub(rim);
        let y0 = y.saturating_sub(rim);
        let x1 = (x + rim).min(self.width - 1);
        let y1 = (y + rim).min(self.height - 1);
        if x0 == 0 || y0 == 0 || x1 == self.width - 1 || y1 == self.height - 1 {
            // The canvas border counts as an edge: there is no "outside"
            // to compare against.
            return true;
        }
        for yy in y0..=y1 {
            for xx in x0..=x1 {
                if self.mask[yy * self.width + xx] == 0 {
                    return true;
                }
            }
        }
        false
    }
}

/// Ruling C-23's rim width, in output pixels.
const RIM_PX: usize = 2;

/// Wraps `map`'s linear part in a small cubic polynomial distortion — a
/// few tenths of a pixel across the frame, the scale a real `auto` fit
/// produces — purely so the deposit takes the per-`TILE` arm. Only
/// `--tiled` calls it.
fn with_small_cubic_distortion(map: &PixelMap, width: usize, height: usize) -> PixelMap {
    use athenaeum_core::geometry::{Distortion, Polynomial2D};
    let terms = athenaeum_core::geometry::polynomial::term_exponents(3).len();
    let mut ax = vec![0.0; terms];
    let mut ay = vec![0.0; terms];
    // One quadratic and one cubic term each, in NORMALIZED coordinates
    // (|u|, |v| <= 1 over the frame), so the displacement stays under a
    // pixel — the point is the code path, not a pathological warp.
    ax[0] = 0.35;
    ay[1] = -0.28;
    ax[terms - 1] = 0.12;
    ay[terms - 2] = 0.09;
    let scale = (width.max(height) as f64) / 2.0;
    let d = Distortion {
        order: 3,
        center: ((width as f64 - 1.0) / 2.0, (height as f64 - 1.0) / 2.0),
        scale,
        domain: None,
        forward: Polynomial2D {
            order: 3,
            ax: ax.clone(),
            ay: ay.clone(),
        },
        inverse: Polynomial2D {
            order: 3,
            ax: ax.iter().map(|v| -v).collect(),
            ay: ay.iter().map(|v| -v).collect(),
        },
    };
    PixelMap::with_distortion(map.linear.clone(), d).expect("an invertible linear part")
}

/// `original stem` → the frame's stored `PixelMap`, for every registered
/// frame of the catalog. The calibrated files are named
/// `c_<original stem>.fits`, so the stem is the join key.
fn load_maps(db: &std::path::Path) -> HashMap<String, PixelMap> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open the catalog read-only");
    let mut stmt = conn
        .prepare(
            "SELECT f.filename, r.transform_json \
             FROM registration_results r \
             JOIN frames fr ON fr.id = r.frame_id \
             JOIN files f ON f.id = fr.file_id \
             WHERE r.transform_json IS NOT NULL",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .expect("query");
    let mut out = HashMap::new();
    for row in rows {
        let (filename, json) = row.expect("row");
        let stem = filename
            .strip_suffix(".fits")
            .or_else(|| filename.strip_suffix(".xisf"))
            .unwrap_or(&filename)
            .to_string();
        if let Ok(map) = PixelMap::from_json(&json) {
            out.insert(stem, map);
        }
    }
    out
}

fn main() {
    // Same pattern as the other probes (ruling R-TA-1): a subscriber only
    // when `RUST_LOG` is set, on stderr, so the report lines below stay
    // clean.
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let args = parse_args();
    let threads = args.threads.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    });
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("pool");

    let maps = load_maps(&args.db);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&args.dir)
        .expect("read the calibrated dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().and_then(|s| s.to_str()) == Some("fits")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with("c_") && !n.ends_with("_d.fits"))
        })
        .collect();
    paths.sort();

    let mut kept: Vec<(PathBuf, PixelMap)> = Vec::new();
    for p in paths {
        if kept.len() == args.limit {
            break;
        }
        let stem = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("c_"))
            .expect("a c_-prefixed stem");
        match maps.get(stem) {
            Some(map) => kept.push((p, map.clone())),
            None => eprintln!("no registration row for {stem}; skipped"),
        }
    }
    assert!(
        !kept.is_empty(),
        "no calibrated frame matched a catalog map"
    );

    // Reference geometry: `--ref` when given (a co-registered run's
    // reference is the LARGEST group's frame, which for this catalog is the
    // OSC group's 6248x4176 — not the mono group's own), else the first
    // frame's own.
    let (ref_w, ref_h) = match args.reference {
        Some(wh) => wh,
        None => {
            let reader = athenaeum_core::integration::plane_reader::PlaneReader::open(&kept[0].0)
                .expect("open the first calibrated frame");
            (reader.width(), reader.height())
        }
    };

    if args.tiled {
        for (_, map) in kept.iter_mut() {
            *map = with_small_cubic_distortion(map, ref_w, ref_h);
        }
    }

    // One table build, timed and sized, so the per-frame (and per-tile)
    // cost the spec quotes is an observed number.
    let build_start = Instant::now();
    let quad = geom::map_drop_at(
        &kept[0].1,
        (ref_w as f64 - 1.0) / 2.0,
        (ref_h as f64 - 1.0) / 2.0,
        args.drop_shrink,
        args.scale,
    )
    .expect("a finite map");
    let table = PhaseTable::build(&quad, args.scale, PHASES).expect("a sane drop tabulates");
    let build_us = build_start.elapsed().as_secs_f64() * 1e6;

    let weight = [1.0f64];
    let pair = [NormalizationPair::IDENTITY];
    let frames: Vec<DrizzleFrame<'_>> = kept
        .iter()
        .map(|(p, map)| DrizzleFrame {
            path: p,
            map,
            weight: &weight,
            output_pair: &pair,
            ln: None,
            rej: None,
            cfa: None,
        })
        .collect();
    let measure = MeasureOptions::default();

    println!(
        "frames {} · reference {ref_w}x{ref_h} · scale {} · dropShrink {} · threads {threads}",
        frames.len(),
        args.scale,
        args.drop_shrink
    );
    let tiles = ref_w.div_ceil(TILE) * ref_h.div_ceil(TILE);
    println!(
        "phase table: {PHASES}x{PHASES} phases, {} bytes, built in {build_us:.0} us \
         ({tiles} tiles of {TILE} px cover the reference)",
        table.heap_bytes()
    );
    println!(
        "arm: {}",
        if args.tiled {
            "tiled (a cubic distortion was injected)"
        } else {
            "per frame (the catalog's own maps carry no distortion)"
        }
    );

    let want_coverage = args.coverage_out.is_some();
    let mut exact_ms: Vec<u64> = Vec::new();
    let mut table_ms: Vec<u64> = Vec::new();
    let mut exact_mask: Option<CoverageMask> = None;
    let mut table_mask: Option<CoverageMask> = None;
    for round in 0..args.rounds {
        for force_exact in [true, false] {
            let input = DrizzleInput {
                frames: &frames,
                width: ref_w,
                height: ref_h,
                channels: 1,
                scale: args.scale,
                drop_shrink: args.drop_shrink,
                kernel: DrizzleKernel::Square,
                use_weights: false,
                use_rejection: false,
                use_local_normalization: false,
                // The weight plane IS the coverage set (see
                // `CoverageMask::from_weight`); off otherwise so the
                // timing runs allocate nothing extra.
                write_weight_map: want_coverage,
                measure: &measure,
                ram_total_bytes: None,
                force_exact_overlap_for_measurement: force_exact,
            };
            let progress = DrizzleProgress {
                on_frame: &|_, _| {},
            };
            let out =
                drizzle_group(&input, &pool, &AtomicBool::new(false), &progress).expect("drizzle");
            let arm = if force_exact { "exact " } else { "table " };
            println!(
                "round {round} {arm}: deposit_ms {:>6} read_ms {:>6} coverage {:.6}",
                out.stats.deposit_ms,
                out.stats.read_ms,
                out.stats.coverage.first().copied().unwrap_or(0.0)
            );
            if force_exact {
                exact_ms.push(out.stats.deposit_ms);
            } else {
                table_ms.push(out.stats.deposit_ms);
            }
            if want_coverage {
                let weight = out.weight.as_ref().expect("write_weight_map was on");
                let m = CoverageMask::from_weight(
                    &weight[..out.stats.out_width * out.stats.out_height],
                    out.stats.out_width,
                    out.stats.out_height,
                );
                println!(
                    "round {round} {arm}: coverage zeros {} of {} ({:.6} covered)",
                    m.zeros(),
                    m.mask.len(),
                    1.0 - m.zeros() as f64 / m.mask.len() as f64
                );
                if force_exact {
                    exact_mask = Some(m);
                } else {
                    table_mask = Some(m);
                }
            }
        }
    }

    if let (Some(dir), Some(e), Some(t)) = (args.coverage_out.as_ref(), &exact_mask, &table_mask) {
        std::fs::create_dir_all(dir).expect("create the coverage dir");
        for (name, m) in [("exact", e), ("table", t)] {
            let path = dir.join(format!("coverage_{name}_{}x{}.bits", m.width, m.height));
            m.write_bits(&path).expect("write the coverage mask");
            println!(
                "wrote {} ({} bytes)",
                path.display(),
                m.mask.len().div_ceil(8)
            );
        }

        assert_eq!(e.mask.len(), t.mask.len(), "the two arms must share a grid");
        let (mut only_exact, mut only_table) = (0usize, 0usize);
        let (mut rim, mut interior) = (0usize, 0usize);
        for idx in 0..e.mask.len() {
            if e.mask[idx] == t.mask[idx] {
                continue;
            }
            if e.mask[idx] != 0 {
                only_exact += 1;
            } else {
                only_table += 1;
            }
            let (x, y) = (idx % e.width, idx / e.width);
            // Judged against the EXACT arm's own covered region — the
            // reference the table is being compared to.
            if e.near_edge(x, y, RIM_PX) {
                rim += 1;
            } else {
                interior += 1;
            }
        }
        println!(
            "COVERAGE zeros: exact {} · table {} · delta {}",
            e.zeros(),
            t.zeros(),
            t.zeros() as i64 - e.zeros() as i64
        );
        println!(
            "COVERAGE one-arm-only: {} total ({} exact-only, {} table-only) — \
             {rim} within {RIM_PX} px of the exact arm's coverage edge, {interior} interior",
            only_exact + only_table,
            only_exact,
            only_table
        );
    }

    let best = |v: &[u64]| v.iter().copied().min().unwrap_or(0);
    let (be, bt) = (best(&exact_ms), best(&table_ms));
    println!(
        "BEST exact {be} ms · table {bt} ms · change {:+.1} %",
        if be > 0 {
            (bt as f64 / be as f64 - 1.0) * 100.0
        } else {
            0.0
        }
    );
}
