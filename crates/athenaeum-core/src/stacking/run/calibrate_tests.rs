//! Tests of the calibrate stage; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::events::NullEmitter;
use crate::stacking::groups::{group_frames, set_slug};
use crate::stacking::test_fixtures::{
    self, date_obs_at, shifted_stars, star_light_spec, wait_for_run, STAR_FIELD_HEIGHT,
    STAR_FIELD_WIDTH,
};

/// Runs stage 1 on a fresh fixture whose three lights link the master
/// flat, and the master dark only when `keep_dark`; returns the context
/// (for a second run) and the group key.
fn pedestal_stage_one(
    ctx: &Arc<ServiceContext>,
    db_path: &Path,
    keep_dark: bool,
    flat_norm: bool,
) -> (RunContext, String, tempfile::TempDir, tempfile::TempDir) {
    let fixture_conn = rusqlite::Connection::open(db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let mut light_ids = Vec::new();
    for (i, t) in THREE_TIMES.iter().enumerate() {
        let (id, _) = test_fixtures::add_light(&fixture, &light_spec(&format!("f{i}"), t));
        light_ids.push(id);
    }
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);
    if !keep_dark {
        fixture
            .conn
            .execute(
                "DELETE FROM calibration_set_to_frames \
                     WHERE source_type = 'frame' AND calibration_type = 'Dark'",
                [],
            )
            .unwrap();
    }
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let mut cfg = StackingConfig::default();
    cfg.calibration.flat_norm = flat_norm;
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let group_key = plan_groups[0].key.clone();
    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout,
        output.path().to_path_buf(),
        HashMap::new(),
    );
    stage_calibrate(&mut rc).unwrap();
    (rc, group_key, working, output)
}

/// Spec 2026-10-04 §5.1-§5.3: a group whose lights link the master flat
/// only gets ONE run warning with the flat's corner ratio, again when
/// stage 1 is fully cached; a group with its dark links gets none.
#[test]
fn a_flat_only_group_gets_one_run_warning_with_the_corner_ratio() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (mut rc, key, _w, _o) = pedestal_stage_one(&ctx, &db_path, false, true);

    // The fixture flat is a constant 0.5 plane stamped ATH_FNRM = 0.5
    // and flat normalization is on: corners = centre -> ratio 1.
    let prefix =
        format!("group {key}: 3 lights were calibrated with a flat but no dark or bias");
    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
    assert!(
        hits[0].contains(
            "The flat's darkest corner reads 100 % of its centre, so the offset pedestal is 1.0\u{d7} larger there."
        ),
        "{}",
        hits[0]
    );

    // Second run: every frame's stage-1 artifact is fresh (all Reused).
    rc.warnings.clear();
    stage_calibrate(&mut rc).unwrap();
    assert!(
        rc.cached_calibrated.values().all(|c| *c),
        "stage 1 not cached"
    );
    let again = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .count();
    assert_eq!(again, 1, "{:?}", rc.warnings);

    // Dark links kept: no such warning.
    let tmp2 = tempfile::tempdir().unwrap();
    let db2 = tmp2.path().join("catalog.db");
    let ctx2 = Arc::new(ServiceContext::new_for_tests(db2.clone()));
    let (rc2, _k2, _w2, _o2) = pedestal_stage_one(&ctx2, &db2, true, true);
    assert!(
        !rc2.warnings
            .iter()
            .any(|w| w.contains("flat but no dark or bias")),
        "{:?}",
        rc2.warnings
    );
}

/// The corner ratio is corner over centre, so switching flat
/// normalization off must not change the sentence.
#[test]
fn the_corner_ratio_does_not_depend_on_flat_normalization() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (rc, key, _w, _o) = pedestal_stage_one(&ctx, &db_path, false, false);
    let prefix =
        format!("group {key}: 3 lights were calibrated with a flat but no dark or bias");
    let hits: Vec<&String> = rc
        .warnings
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", rc.warnings);
    assert!(
        hits[0].contains(
            "reads 100 % of its centre, so the offset pedestal is 1.0\u{d7} larger there."
        ),
        "{}",
        hits[0]
    );
}

// ── Task 13c L1: Calibrate working set + parallel master decode ─────

/// The measured per-frame working set, per colour mode: mono 3 planes,
/// a debayered OSC group 5 (was a flat 8). Admission on the hc11 and
/// set 204 geometries against a 16 GiB-machine (4 GiB) and a 128 GiB
/// -machine (32 GiB) budget.
#[test]
fn calibrate_working_set_is_per_colour_mode() {
    let (w, h) = (9576u64, 6388u64);
    let plane = w * h * 4;
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Mono, false), 3 * plane);
    // A mono group never holds a debayer output, whatever the flag says.
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Mono, true), 3 * plane);
    // An OSC group kept as mosaics (debayer off) is mono-sized too.
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Osc, false), 3 * plane);
    assert_eq!(calibrate_working_set_bytes(w, h, ColorMode::Osc, true), 5 * plane);

    let gib = 1u64 << 30;
    // hc11: 978 743 808 shared bytes, 4 GiB budget -> admission 4; 128 GiB machine -> the pool.
    let hc11_ws = calibrate_working_set_bytes(w, h, ColorMode::Mono, false);
    assert_eq!(admission_with_shared(978_743_808, hc11_ws, 16, Some(4 * gib)), 4);
    assert_eq!(admission_with_shared(978_743_808, hc11_ws, 10, Some(32 * gib)), 10);
    // set 204 OSC (6248x4176, 199 MB shared): 7 on 4 GiB; mono capped by the pool at 10.
    let (w2, h2) = (6248u64, 4176u64);
    let osc = calibrate_working_set_bytes(w2, h2, ColorMode::Osc, true);
    let mono = calibrate_working_set_bytes(w2, h2, ColorMode::Mono, false);
    assert_eq!(admission_with_shared(199 << 20, osc, 16, Some(4 * gib)), 7);
    assert_eq!(admission_with_shared(199 << 20, mono, 10, Some(4 * gib)), 10);
}

/// The preload by fan-out equals the serial decode bit for bit, and the
/// hot-pixel pre-build measures each distinct dark once, pushing its
/// warnings in job order.
#[test]
fn parallel_master_preload_matches_the_serial_decode() {
    use crate::fits_writer::write_fits_f32;
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (16usize, 12usize);
    let write = |name: &str, base: f32| -> PathBuf {
        let data: Vec<f32> = (0..w * h).map(|i| base + (i % 7) as f32 * 0.25).collect();
        let p = dir.path().join(name);
        write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
        p
    };
    let dark_a = write("dark_a.fits", 10.0);
    let dark_b = write("dark_b.fits", 20.0);
    let flat = write("flat.fits", 1.0);
    let opts: CalibratedLightOptions = serde_json::from_str("{}").unwrap();
    assert!(opts.hot_pixel_correction);
    let job = |id: i64, dark: &Path| CalibrateJob {
        frame_id: id,
        old_calibrated: None,
        old_mosaic: None,
        hash: String::new(),
        spec: GenerationSpec {
            inputs: crate::calibration_library::light_cal::LightCalInputs {
                light_path: dir.path().join(format!("light{id}.fits")),
                dark_path: Some(dark.to_path_buf()),
                bias_path: None,
                flat_path: Some(flat.clone()),
                flat_norm: true,
                flat_norm_mode: Default::default(),
                cfa_geometry: None,
                params: Default::default(),
                scale_divisor: 1.0,
                output_path: dir.path().join(format!("out{id}.fits")),
                cards: vec![],
                scratch_dir: dir.path().to_path_buf(),
            },
            cards: vec![],
            cfa_geometry: None,
            dark_path: Some(dark.to_path_buf()),
            debayer: false,
            format: Default::default(),
        },
        out: dir.path().join(format!("o{id}.fits")),
        mosaic_out: None,
        opts: opts.clone(),
        _pins: Vec::new(),
    };
    // Two lights share dark A, one uses dark B.
    let jobs = vec![job(1, &dark_a), job(2, &dark_a), job(3, &dark_b)];

    let db = tempfile::tempdir().unwrap();
    let mut ctx = ServiceContext::new_for_tests(db.path().join("t.db"));
    ctx.image_pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let working = tempfile::tempdir().unwrap();
    let mut rc = test_context(
        Arc::new(ctx),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        1,
        "L1",
        StackingConfig::default(),
        Vec::new(),
        WorkingLayout::new(working.path(), &set_slug("L1")),
        working.path().to_path_buf(),
        HashMap::new(),
    );
    let scratch = dir.path();

    let preloaded =
        build_preloaded_masters(&rc, &jobs, scratch, "g", (w as u64, h as u64), 0).unwrap();
    assert_eq!(preloaded.planes.len(), 3);
    let never = AtomicBool::new(false);
    for p in [&dark_a, &dark_b, &flat] {
        let serial = preload_master_plane(p, scratch, &never).unwrap();
        let got = &preloaded.planes[p];
        assert_eq!((got.width, got.height), (serial.width, serial.height));
        assert_eq!(
            got.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            serial.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "{}",
            p.display()
        );
    }

    prebuild_hot_maps(&mut rc, &jobs, scratch, "g", (w as u64, h as u64), 0);
    assert_eq!(rc.hot_maps.len(), 2, "one map per distinct dark");
    assert!(rc.hot_maps.contains_key(&dark_a) && rc.hot_maps.contains_key(&dark_b));
    let refused: Vec<&String> = rc.warnings.iter().filter(|m| m.contains("Hot-pixel")).collect();
    for (i, d) in [&dark_a, &dark_b].iter().enumerate() {
        if let HotPixelMapOutcome::Refused(_) = &*rc.hot_maps[*d] {
            assert!(refused.iter().any(|m| m.contains(&d.display().to_string())), "{i}");
        }
    }
    // Warnings follow job order: dark A's (if any) before dark B's.
    let pos = |d: &PathBuf| refused.iter().position(|m| m.contains(&d.display().to_string()));
    if let (Some(a), Some(b)) = (pos(&dark_a), pos(&dark_b)) {
        assert!(a < b);
    }
    // A second call is a no-op (cache hit): nothing new is pushed.
    let n = rc.warnings.len();
    prebuild_hot_maps(&mut rc, &jobs, scratch, "g", (w as u64, h as u64), 0);
    assert_eq!(rc.warnings.len(), n);
}

// ── stage_calibrate ──────────────────────────────────────────────────

/// Perf tier 1 Task 4: the fan-out must produce the same calibrated
/// PIXELS, the same header CARDS and the same warning ORDER as the
/// sequential stage did. `seed_ready`'s master dark is a constant-value
/// plane (MAD = 0, see `hot_pixel_map_from_dark`'s "zero mad" branch),
/// so `prebuild_hot_maps` refuses its hot-pixel map and pushes exactly
/// one "Hot-pixel correction skipped for …" warning — the one thing a
/// reordering bug in this task would show up as.
///
/// NOT a raw-byte comparison, despite the brief's own framing (a
/// discovery made while writing this pin, unrelated to the fan-out):
/// `light_resolve::source_cards_for_file` rebuilds a light's header
/// cards from `parse_stored_header_keys`'s `HashMap<String, String>`
/// (`fits_parser/stored_header.rs`), so the card ORDER in a calibrated
/// output already varies from one generation to the next — proven by
/// running this fixture's admission-1 path twice and diffing the raw
/// bytes, which disagree starting at the SAME byte offset (560, right
/// after `SIMPLE`/`BITPIX`/`NAXIS*`) whether or not a fan-out is
/// involved. That is a pre-existing property of the header-rebuild path
/// (harmless: nothing downstream keys freshness on byte-identical
/// headers), not a regression this task introduces, and fixing it is
/// out of this task's scope. So this pin compares what "the same
/// calibrated bytes" actually needs to mean here: the header's CARD SET
/// (sorted, so ordering cannot fail it) and the PIXEL DATA (which the
/// header-order shuffle cannot touch — same card COUNT on both sides
/// keeps the header block count, and therefore the data offset,
/// identical) read back bit-for-bit via `PlaneReader`.
///
/// Since Task 13c D1 `source_cards_for_file` returns its cards keyword-sorted,
/// so the order is now deterministic; the sorted comparison below stays.
///
/// Controller ruling (2026-09-19): no env var or static forces
/// admission — two SEPARATE `ServiceContext`s give the two runs two
/// different `image_pool` widths instead. `ServiceContext::new_for_tests`'s
/// stock 1-thread pool clamps `admission` to 1 (sequential, one job at a
/// time) whatever the working-set math says; a second context built
/// here with a 4-thread pool clamps `admission` up to 4 instead —
/// `admission`'s RAM-budget term is astronomically larger than this
/// fixture's `CALIBRATE_PLANES_RESIDENT_MONO * 64 * 48 * 4` (~37 KB) working
/// set on any machine with a few GB of RAM, so the pool WIDTH is the
/// clamp that actually binds. Two separate working folders + run ids;
/// the parallel run also sets `rerun_from = Some(Stage::Calibrate)` (the
/// SAME escape hatch `calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones`'s
/// own rc4 uses below) because both runs share ONE on-disk catalog —
/// without it, the parallel run's freshness check would find the
/// sequential run's OWN `stacking_artifacts` rows fresh (identical
/// config hash) and reuse them instead of regenerating, which would
/// make this pin compare a file against itself.
#[test]
fn calibrate_fan_out_matches_the_sequential_stage_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let (fixture, _light_ids, _working, _output) = seed_ready(&db_path, SET_NAME);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    // Fix round 1: `admission_with_shared` (like `admission`) treats an
    // unknown RAM total as an exhausted budget — admission 1 always,
    // whatever `shared_bytes`/`working_set_bytes` say. On a machine (or
    // sandbox) with no RAM probe the parallel run below could never
    // actually admit more than one worker, so this pin would compare
    // sequential against sequential for a reason that has nothing to do
    // with the fan-out under test. Skip it rather than fail it.
    if total_ram_bytes().is_none() {
        return;
    }

    // Sequential reference: the stock 1-thread `image_pool` clamps
    // `admission` to 1.
    let ctx_seq = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let working_seq = tempfile::tempdir().unwrap();
    let output_seq = tempfile::tempdir().unwrap();
    let layout_seq = WorkingLayout::new(working_seq.path(), &set_slug(SET_NAME));
    let mut rc_seq = test_context(
        ctx_seq,
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout_seq.clone(),
        output_seq.path().to_path_buf(),
        HashMap::new(),
    );
    stage_calibrate(&mut rc_seq).unwrap();

    let seq_dir = layout_seq.calibrated_dir();
    let mut seq_files: Vec<PathBuf> = std::fs::read_dir(&seq_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    seq_files.sort();
    assert_eq!(seq_files.len(), 3, "{seq_files:?}");
    let seq_warnings = rc_seq.warnings.clone();

    // Parallel: a second `ServiceContext` whose `image_pool` is 4 wide —
    // `admission` clamps to the pool width, not to 1. A fresh working
    // folder and `rerun_from` so this run always regenerates through
    // the fan-out rather than reusing the sequential run's own rows.
    let mut ctx2 = ServiceContext::new_for_tests(db_path.clone());
    ctx2.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap(),
    );
    let ctx_par = Arc::new(ctx2);
    // Task 4 fix round: assert this fixture's working set still admits
    // more than one worker on the 4-thread pool before running, so a
    // future admission-math change cannot silently degrade this pin to
    // comparing sequential against sequential. Fix round 1 (Important
    // finding 2): `admission_with_shared` with `shared_bytes` for the
    // fixture's own two preloaded planes (its dark and flat — see the
    // doc comment above), `calibrate_working_set_bytes` for the
    // per-worker term.
    assert!(
        admission_with_shared(
            2 * 64 * 48 * 4,
            calibrate_working_set_bytes(64, 48, ColorMode::Mono, false),
            4,
            total_ram_bytes().map(|t| t / 4),
        ) > 1,
        "fixture working set must admit more than one worker for this pin to test the fan-out"
    );
    let working_par = tempfile::tempdir().unwrap();
    let output_par = tempfile::tempdir().unwrap();
    let layout_par = WorkingLayout::new(working_par.path(), &set_slug(SET_NAME));
    let mut rc_par = test_context(
        ctx_par,
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        2,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout_par.clone(),
        output_par.path().to_path_buf(),
        HashMap::new(),
    );
    rc_par.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc_par).unwrap();

    let par_dir = layout_par.calibrated_dir();
    let mut par_files: Vec<PathBuf> = std::fs::read_dir(&par_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    par_files.sort();
    assert_eq!(par_files.len(), 3, "{par_files:?}");

    // Sorted header card lines: order-independent (see the doc comment
    // above), still catches a genuinely different card set.
    fn header_lines_sorted(path: &Path) -> Vec<String> {
        let mut lines: Vec<String> = FitsHeader::from_path(path)
            .unwrap()
            .to_header_text()
            .lines()
            .map(|l| l.to_string())
            .collect();
        lines.sort();
        lines
    }
    // Every plane's pixel data, read back through the SAME reader the
    // pipeline itself uses — the header shuffle cannot touch this: equal
    // card COUNTS (both sides carry the same cards) keep the header's
    // 2880-byte block count, and therefore the data offset, identical.
    fn planes(path: &Path) -> Vec<Vec<f32>> {
        let reader = PlaneReader::open(path).unwrap();
        (0..reader.channels())
            .map(|p| reader.read_plane(p).unwrap())
            .collect()
    }

    for (i, (sp, pp)) in seq_files.iter().zip(par_files.iter()).enumerate() {
        assert_eq!(
            header_lines_sorted(sp),
            header_lines_sorted(pp),
            "file {i}: header card SET must match the sequential stage (order is not \
                 pinned — see the doc comment above)"
        );
        assert_eq!(
            planes(sp),
            planes(pp),
            "file {i}: calibrated pixel data must match the sequential stage bit for bit"
        );
    }
    assert_eq!(
        seq_warnings, rc_par.warnings,
        "warning order (the one hot-pixel refusal) must match the sequential stage"
    );
    assert_eq!(
        seq_warnings.len(),
        1,
        "the fixture's dark has MAD = 0: exactly one refusal warning is expected: {seq_warnings:?}"
    );
}

/// Zip plan Task 9: the non-attested Calibrate stage reads lights AND masters out of zips. The same
/// frame set calibrated once from disk and once with every light and both masters inside a zip gives
/// byte-identical `calibrated` artifacts, no frame is excluded, and the stage-1 hash (which only names
/// the masters) resolved for every frame.
#[test]
fn calibrate_stage_reads_zip_resident_lights_and_masters_like_disk() {
    use crate::zip_store::test_support::{zip_files, Shape};
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let cfg = StackingConfig::default();
    let calibrate = |working_dir: &Path| {
        let layout = WorkingLayout::new(working_dir, &set_slug(SET_NAME));
        let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
        assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");
        let mut rc = test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            1,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups,
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        );
        stage_calibrate(&mut rc).unwrap();
        assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
        let dir = layout.calibrated_dir();
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
            .collect();
        files.sort();
        files
    };
    let from_disk = calibrate(working.path());
    assert_eq!(from_disk.len(), 3);

    // Fresh stage-1 state, then every light and both masters go into one zip.
    fixture.conn.execute("DELETE FROM stacking_artifacts", []).unwrap();
    let mut file_ids: Vec<i64> = light_ids
        .iter()
        .map(|id| fixture.conn.query_row("SELECT file_id FROM frames WHERE id = ?1", [id], |r| r.get(0)).unwrap())
        .collect();
    let mut stmt = fixture
        .conn
        .prepare(
            "SELECT DISTINCT fr.file_id FROM calibration_set cs
                 JOIN calibration_set_frames csf ON csf.set_id = cs.id
                 JOIN frames fr ON fr.id = csf.frame_id WHERE cs.is_master_library = 1",
        )
        .unwrap();
    let masters: Vec<i64> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    drop(stmt);
    assert_eq!(masters.len(), 2, "a master dark and a master flat");
    file_ids.extend(masters);
    let zipped = zip_files(&fixture.conn, &file_ids, &tmp.path().join("night.zip"), Shape::Foreign);
    assert!(zipped.iter().all(|p| !Path::new(p).exists()));

    let before = ctx.zip_cache.extraction_count();
    let working2 = tempfile::tempdir().unwrap();
    let from_zip = calibrate(working2.path());
    // The pixels are the contract. The headers differ in exactly one place by design: the provenance
    // cards name the master by its catalog path, which for a zip master is `<zip>/<entry>`.
    let data_of = |bytes: &[u8]| -> Vec<u8> {
        let end = bytes
            .chunks(80)
            .position(|card| card.starts_with(b"END     "))
            .expect("an END card");
        let header_len = ((end + 1) * 80).div_ceil(2880) * 2880;
        bytes[header_len..].to_vec()
    };
    assert_eq!(from_zip.len(), from_disk.len());
    for ((name_z, bytes_z), (name_d, bytes_d)) in from_zip.iter().zip(&from_disk) {
        assert_eq!(name_z, name_d);
        assert!(
            data_of(bytes_z) == data_of(bytes_d),
            "{name_z}: a zip-resident night calibrates to the same pixels"
        );
    }
    assert!(ctx.zip_cache.extraction_count() - before >= 5, "three lights and two masters were read out of the zip");
}


#[test]
fn calibrate_stage_reuses_fresh_artifacts_and_regenerates_stale_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    let build = |run_id: i64, groups: Vec<IntegrationGroup>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            groups,
            layout.clone(),
            output_dir.clone(),
            HashMap::new(),
        )
    };

    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    // First pass: 3 calibrated files, 3 artifact rows.
    let mut rc1 = build(1, plan_groups.clone());
    stage_calibrate(&mut rc1).unwrap();

    let calibrated_dir = layout.calibrated_dir();
    let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
    assert_eq!(files_on_disk, 3);

    let artifacts_after_1 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts_after_1.len(), 3);

    let snapshot = |artifacts: &[crate::db::stacking::StackingArtifactRow]| -> HashMap<i64, (std::time::SystemTime, String)> {
        artifacts
            .iter()
            .map(|a| {
                let path = a.path.clone().unwrap();
                let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
                (a.frame_id.unwrap(), (mtime, a.config_hash.clone()))
            })
            .collect()
    };
    let before = snapshot(&artifacts_after_1);

    // Second pass: 0 files rewritten (mtimes unchanged), rows unchanged.
    let mut rc2 = build(2, plan_groups.clone());
    stage_calibrate(&mut rc2).unwrap();
    let artifacts_after_2 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts_after_2.len(), 3);
    let after_reuse = snapshot(&artifacts_after_2);
    assert_eq!(
        before, after_reuse,
        "a fresh artifact must not be rewritten"
    );

    // Touch one source file's size in `files` — only that frame regenerates.
    fixture
        .conn
        .execute(
            "UPDATE files SET size = size + 1000 WHERE id = \
                 (SELECT file_id FROM frames WHERE id = ?1)",
            rusqlite::params![light_ids[0]],
        )
        .unwrap();
    let plan_groups_touched =
        group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let mut rc3 = build(3, plan_groups_touched);
    stage_calibrate(&mut rc3).unwrap();
    let artifacts_after_3 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    let after_touch = snapshot(&artifacts_after_3);
    assert_ne!(
        after_touch[&light_ids[0]], before[&light_ids[0]],
        "the touched frame must regenerate"
    );
    assert_eq!(
        after_touch[&light_ids[1]], before[&light_ids[1]],
        "an untouched frame must not regenerate"
    );
    assert_eq!(
        after_touch[&light_ids[2]], before[&light_ids[2]],
        "an untouched frame must not regenerate"
    );

    // rerun_from Calibrate: every artifact is stale, all three regenerate.
    let plan_groups_rerun = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let mut rc4 = build(4, plan_groups_rerun);
    rc4.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc4).unwrap();
    let artifacts_after_4 = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    let after_rerun = snapshot(&artifacts_after_4);
    for &id in &light_ids {
        assert_ne!(
            after_rerun[&id], after_touch[&id],
            "frame {id} must regenerate under rerun_from Calibrate"
        );
    }
}

/// Frame-level cache spec §5.1: a regroup (here "one group per exposure" -> "no split") finds the calibrated
/// rows again; nothing is recalibrated and no file is rewritten.
#[test]
fn calibrated_artifact_is_reused_across_a_regroup() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, _light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));

    let mut cfg_seconds = StackingConfig::default();
    cfg_seconds.grouping.exposure_mode = crate::stacking::config::ExposureMode::Seconds;
    let mut cfg_all = StackingConfig::default();
    cfg_all.grouping.exposure_mode = crate::stacking::config::ExposureMode::NoSplit;
    let groups_of = |cfg: &StackingConfig| {
        group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap()
    };
    let (groups_seconds, groups_all) = (groups_of(&cfg_seconds), groups_of(&cfg_all));
    assert_ne!(
        groups_seconds[0].key, groups_all[0].key,
        "the two groupings must name the group differently"
    );
    let build = |run_id: i64, cfg: &StackingConfig, groups: Vec<IntegrationGroup>| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            groups,
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        )
    };
    let rows = || {
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated"),
        )
        .unwrap()
    };

    let mut rc1 = build(1, &cfg_seconds, groups_seconds);
    let (res, text1) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc1));
    res.unwrap();
    assert_eq!(fan_out_items(&text1, "calibrate", "calibrate"), 3, "{text1}");
    let first = rows();
    assert_eq!(first.len(), 3);
    assert!(first.iter().all(|r| r.group_key.is_empty()), "{first:?}");
    let mtimes = |rows: &[crate::db::stacking::StackingArtifactRow]| -> Vec<std::time::SystemTime> {
        rows.iter()
            .map(|r| std::fs::metadata(r.path.as_ref().unwrap()).unwrap().modified().unwrap())
            .collect()
    };
    let first_mtimes = mtimes(&first);

    let mut rc2 = build(2, &cfg_all, groups_all);
    let (res, text2) = crate::stacking::telemetry::capture_logs(|| stage_calibrate(&mut rc2));
    res.unwrap();
    assert_eq!(
        fan_out_items(&text2, "calibrate", "calibrate"),
        0,
        "a regroup recalibrated: {text2}"
    );
    let second = rows();
    assert_eq!(second.len(), 3, "a regroup added rows");
    for (a, b) in first.iter().zip(&second) {
        assert_eq!((a.id, &a.created_at, &a.path), (b.id, &b.created_at, &b.path));
        assert_eq!(b.group_key, "");
    }
    assert_eq!(mtimes(&second), first_mtimes, "a regroup rewrote a calibrated file");
}

/// Frame-level cache spec §5.1/§5.7 (review focus 3): a frame re-written under a new stem or folder takes its
/// old file with it, unless another row still points to it.
#[test]
fn rewriting_a_frame_removes_its_old_file_once_unreferenced() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let (fixture, light_ids, working, output) = seed_ready(&db_path, SET_NAME);
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let cfg = StackingConfig::default();
    let subject = StackSubject::FrameSet(fixture.set_id);
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let build = |run_id: i64| {
        test_context(
            ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output.path().to_path_buf(),
            HashMap::new(),
        )
    };
    let mut rc1 = build(1);
    stage_calibrate(&mut rc1).unwrap();

    // Rows as an older version left them: the file in `calibrated/<group_key>/` under the old stem.
    let old_dir = layout.calibrated_dir().join(&plan_groups[0].key);
    std::fs::create_dir_all(&old_dir).unwrap();
    let mut old_paths = Vec::new();
    for &id in &light_ids[..2] {
        let row = crate::db::stacking::find_artifact(&fixture.conn, &subject, "", "calibrated", Some(id))
            .unwrap()
            .unwrap();
        let old = old_dir.join(format!("c_old_{id}.fits"));
        std::fs::rename(row.path.as_ref().unwrap(), &old).unwrap();
        fixture
            .conn
            .execute(
                "UPDATE stacking_artifacts SET path = ?1 WHERE id = ?2",
                rusqlite::params![old.to_str().unwrap(), row.id],
            )
            .unwrap();
        old_paths.push(old);
    }
    // Another row (of another kind) still points to the second frame's old file.
    upsert_artifact(
        &fixture.conn,
        &NewArtifact {
            subject: &subject,
            frame_id: Some(light_ids[1]),
            group_key: "",
            kind: "solve",
            path: old_paths[1].to_str(),
            config_hash: "h",
            size: None,
            modified_at: None,
            payload_json: None,
        },
    )
    .unwrap();

    let mut rc2 = build(2);
    rc2.rerun_from = Some(Stage::Calibrate);
    stage_calibrate(&mut rc2).unwrap();

    for (n, &id) in light_ids[..2].iter().enumerate() {
        let row = crate::db::stacking::find_artifact(&fixture.conn, &subject, "", "calibrated", Some(id))
            .unwrap()
            .unwrap();
        let new = PathBuf::from(row.path.unwrap());
        assert_eq!(new.parent().unwrap(), layout.calibrated_dir(), "{new:?}");
        let name = new.file_name().unwrap().to_str().unwrap().to_string();
        assert!(name.ends_with(&format!("_f{id}.fits")), "{name}");
        assert!(new.exists());
        assert_eq!(
            old_paths[n].exists(),
            n == 1,
            "frame {id}: the old file {} be gone",
            if n == 0 { "must" } else { "must not" }
        );
    }
}

/// Frame-level cache spec §5.1 (review focus 4): a collab project frame (`StackSubject::Project`) takes the
/// same frame-level key and the frame-only stem.
#[test]
fn a_project_frame_uses_the_frame_level_key() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let conn = project_conn(&db_path);
    crate::db::stacking::insert_test_project(&conn, "p1");
    let landed = tmp.path().join("collab");
    for (u, file) in [
        ("m1", test_fixtures::ProjectFile::Mono32),
        ("x1", test_fixtures::ProjectFile::Xisf32),
    ] {
        let mut spec = test_fixtures::project_frame_spec(u);
        spec.file = file;
        test_fixtures::add_project_frame(&conn, &landed, "p1", &spec);
    }
    let mut rc = project_rc(
        &ctx,
        &conn,
        "p1",
        StackingConfig::default(),
        &tmp.path().join("work"),
        &tmp.path().join("out"),
    );
    stage_calibrate(&mut rc).unwrap();
    assert!(rc.runtime_exclusions.is_empty(), "{:?}", rc.runtime_exclusions);
    let rows = crate::db::stacking::list_artifacts(&conn, &rc.subject, Some("calibrated")).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(row.group_key, "", "{row:?}");
        assert!(matches!(row.subject, StackSubject::Project(_)), "{row:?}");
        let handle = row.frame_id.expect("a project frame handle");
        let found = crate::db::stacking::find_artifact(&conn, &rc.subject, "", "calibrated", Some(handle))
            .unwrap();
        assert_eq!(found.map(|r| r.id), Some(row.id));
    }
    // The converted file is named by the frame alone: `p_<stem>_f<handle>.fits`, in the flat folder.
    let converted = rows
        .iter()
        .map(|r| PathBuf::from(r.path.as_ref().unwrap()))
        .find(|p| p.starts_with(rc.layout.prepared_dir()))
        .expect("the XISF frame is converted under prepared/");
    let handle = rows
        .iter()
        .find(|r| PathBuf::from(r.path.as_ref().unwrap()) == converted)
        .unwrap()
        .frame_id
        .unwrap();
    assert_eq!(converted.parent().unwrap(), rc.layout.prepared_dir());
    assert!(
        converted
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with(&format!("_f{handle}.fits")),
        "{converted:?}"
    );
}

/// Fix round 1, Critical: two frames of one group sharing a source
/// basename (a capture counter restarted on another night) must not
/// collide on one calibrated output. Since frame-level cache spec §5.1
/// EVERY frame's stem carries `_f<frame_id>`, the unique-named frame
/// included, so a merge never renames a file.
#[test]
fn calibrate_outputs_disambiguate_same_basename_within_a_group() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    let fixture_conn = rusqlite::Connection::open(&db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);

    // Two lights sharing the SAME source basename ("dup.fits") — a
    // capture counter that restarted on a different night, so the two
    // physical files live in DIFFERENT directories (`files.path` is
    // UNIQUE, so they can't share a path even though they share a
    // name). `add_light` gives the first one a real file; the second
    // is a manual copy into a sibling directory with the same
    // filename, inserted the same way `add_light`'s own `insert_light_row`
    // would (this test's file scope is `run/` only — the fixture
    // builder itself is untouched).
    let (dup_a, path_a) =
        test_fixtures::add_light(&fixture, &light_spec("dup", THREE_TIMES[0]));

    let night2_dir = fixture.dir.path().join("night2");
    std::fs::create_dir_all(&night2_dir).unwrap();
    let path_b = night2_dir.join("dup.fits");
    std::fs::copy(&path_a, &path_b).unwrap();
    let meta_b = std::fs::metadata(&path_b).unwrap();
    let modified_b =
        chrono::DateTime::<chrono::Utc>::from(meta_b.modified().unwrap()).to_rfc3339();
    fixture
        .conn
        .execute(
            "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, 'dup.fits', ?2, ?3, 'FITS')",
            rusqlite::params![path_b.to_string_lossy(), meta_b.len() as i64, modified_b],
        )
        .unwrap();
    let file_id_b = fixture.conn.last_insert_rowid();
    fixture
        .conn
        .execute(
            "INSERT INTO frames (file_id, instrume, filter, xbinning, naxis1, naxis2, \
                 exptime, date_obs, imagetyp) VALUES (?1, 'cam', NULL, 1, 64, 48, 60.0, ?2, 'Light')",
            rusqlite::params![file_id_b, THREE_TIMES[1]],
        )
        .unwrap();
    let dup_b = fixture.conn.last_insert_rowid();
    fixture
        .conn
        .execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            rusqlite::params![fixture.session_id, dup_b],
        )
        .unwrap();

    let (uniq, _) = test_fixtures::add_light(&fixture, &light_spec("uniq", THREE_TIMES[2]));
    let light_ids = vec![dup_a, dup_b, uniq];
    test_fixtures::add_master_dark_and_flat(&fixture, &light_ids, 64, 48);

    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();

    let cfg = StackingConfig::default();
    let layout = WorkingLayout::new(working.path(), &set_slug(SET_NAME));
    let output_dir = output.path().to_path_buf();

    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 1, "one group expected for this fixture");

    let mut rc = test_context(
        ctx.clone(),
        Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
        1,
        fixture.set_id,
        SET_NAME,
        cfg,
        plan_groups,
        layout.clone(),
        output_dir,
        HashMap::new(),
    );
    stage_calibrate(&mut rc).unwrap();

    let artifacts = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated"),
    )
    .unwrap();
    assert_eq!(artifacts.len(), 3, "{artifacts:?}");

    let path_of = |frame_id: i64| -> String {
        artifacts
            .iter()
            .find(|a| a.frame_id == Some(frame_id))
            .unwrap_or_else(|| panic!("no artifact row for frame {frame_id}"))
            .path
            .clone()
            .unwrap()
    };
    let path_a = path_of(dup_a);
    let path_b = path_of(dup_b);
    let path_uniq = path_of(uniq);

    assert_ne!(
        path_a, path_b,
        "colliding frames must get two DISTINCT calibrated files"
    );
    assert!(
        path_a.contains(&format!("_f{dup_a}")),
        "colliding frame {dup_a} must carry the disambiguating suffix: {path_a}"
    );
    assert!(
        path_b.contains(&format!("_f{dup_b}")),
        "colliding frame {dup_b} must carry the disambiguating suffix: {path_b}"
    );
    // Spec §5.1: the stem is a function of the frame alone, so a frame with a unique name carries its id too
    // (a merge that brings a namesake in must not rename it).
    assert!(
        path_uniq.contains(&format!("_f{uniq}")),
        "a frame with a unique source name carries its own id as well: {path_uniq}"
    );

    let calibrated_dir = layout.calibrated_dir();
    let files_on_disk = std::fs::read_dir(&calibrated_dir).unwrap().count();
    assert_eq!(
        files_on_disk, 3,
        "three distinct files on disk — neither collision overwrote the other"
    );
}

/// Every plane of every calibrated frame of the rig, as bit patterns, by frame id.
fn calibrated_plane_bits(rig: &Pass1Rig) -> Vec<(i64, Vec<Vec<u32>>)> {
    rig.sorted_ids()
        .into_iter()
        .map(|id| (id, file_plane_bits(&rig.calibrated_path(id))))
        .collect()
}

/// The precondition of keeping a first pass across a recalibration, on a rig: a frame calibrated, its file and
/// row deleted and calibrated again in a NEW run context has the same planes to the bit. `planes` is the plane
/// count every calibrated file must have (1 mono, 3 debayered).
fn assert_recalibration_is_bit_identical(rig: &Pass1Rig, planes: usize, parallel: bool) {
    rig.run("run 1", None);
    let before = calibrated_plane_bits(rig);
    assert_eq!(before.len(), 4);
    assert!(before.iter().all(|(_, p)| p.len() == planes), "{planes} planes each");
    let calibrated_files: Vec<PathBuf> =
        before.iter().map(|(id, _)| rig.calibrated_path(*id)).collect();
    assert!(
        calibrated_files.iter().all(|f| f.starts_with(rig.layout.calibrated_root())),
        "written under calibrated/: {calibrated_files:?}"
    );
    rig.delete_rows(&["calibrated"]);
    for file in &calibrated_files {
        std::fs::remove_file(file).unwrap();
    }
    let (_, logs) = crate::stacking::telemetry::capture_logs(|| rig.run("run 2", None));
    if parallel && total_ram_bytes().is_some() {
        assert!(widest_admission(&logs) > 1, "run 2 recalibrated in parallel: {logs}");
    }
    for file in &calibrated_files {
        assert!(file.is_file(), "run 2 recalibrated {}", file.display());
    }
    let after = calibrated_plane_bits(rig);
    assert_eq!(before, after, "a recalibration changed a pixel");
}

/// Frame-level cache spec §5.3 / §8 Q1 (Task 8, step 1): the precondition of keeping a first pass across a
/// recalibration. A frame calibrated, its file and row deleted and calibrated again in a NEW run context has the
/// same planes to the bit. Without this pin the mtime stamp must stay. Ruling R12 (b): the mono case on the stock
/// one-thread pool and on a four-thread one, and the debayered OSC case (three-plane `_d` files) on four threads.
#[test]
fn recalibration_reproduces_the_planes_bit_for_bit() {
    assert_recalibration_is_bit_identical(&Pass1Rig::new(), 1, false);
    assert_recalibration_is_bit_identical(&Pass1Rig::build(4, false), 1, true);
    assert_recalibration_is_bit_identical(&Pass1Rig::build(4, true), 3, true);
}

/// Task 13c L4a: stage 1 over several groups through ONE queue gives the files, warnings, exclusions and
/// cache flags the per-group queues give (same bytes, same commit order).
#[test]
fn calibrate_one_queue_across_groups_matches_per_group_queues() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap(),
    );
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    assert_eq!(plan_groups.len(), 3, "{plan_groups:?}");
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    type Rows = Vec<(String, Option<i64>, String, Option<i64>)>;
    type Snapshot = (
        Vec<(String, Vec<u8>)>,
        Vec<String>,
        Vec<(i64, String)>,
        Vec<(i64, bool)>,
        Rows,
        Vec<(u64, u64)>,
    );
    // `mode`: 0 = per-group queues, 1 = one queue over every group, 2 = the pre-L4 baseline — the stage run
    // once per group, in group order, on one run context.
    let run = |mode: u8| -> Snapshot {
        let w = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
        // The shared catalog keeps the other shape's rows otherwise: compare rows written by THIS shape.
        fixture
            .conn
            .execute("DELETE FROM stacking_artifacts", [])
            .unwrap();
        let rec = Arc::new(Recording::new());
        let mut rc = test_context(
            ctx.clone(),
            rec.clone() as Arc<dyn ProgressEmitter>,
            run_id,
            fixture.set_id,
            SET_NAME,
            cfg.clone(),
            plan_groups.clone(),
            layout.clone(),
            output.path().to_path_buf(),
            group_ids.clone(),
        );
        // Both runs share one catalog: regenerate rather than reuse the other run's rows.
        rc.rerun_from = Some(Stage::Calibrate);
        if mode == 2 {
            for g in &plan_groups {
                rc.plan_groups = vec![g.clone()];
                stage_calibrate_with(&mut rc, Some(false)).unwrap();
            }
            rc.plan_groups = plan_groups.clone();
        } else {
            stage_calibrate_with(&mut rc, Some(mode == 1)).unwrap();
        }
        let rows: Rows = crate::db::stacking::list_artifacts(&fixture.conn, &rc.subject, Some("calibrated"))
            .unwrap()
            .into_iter()
            .map(|r| (r.group_key, r.frame_id, r.config_hash, r.size))
            .collect();
        let progress: Vec<(u64, u64)> = rec
            .events(STACKING_PROGRESS_EVENT)
            .iter()
            .filter(|e| e["stage"] == Stage::Calibrate.as_str())
            .map(|e| (e["current"].as_u64().unwrap(), e["total"].as_u64().unwrap()))
            .collect();
        let mut files = Vec::new();
        for g in &plan_groups {
            let dir = layout.calibrated_dir();
            let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            names.sort();
            for n in names {
                files.push((
                    format!("{}/{}", g.key, n.file_name().unwrap().to_string_lossy()),
                    std::fs::read(&n).unwrap(),
                ));
            }
        }
        let mut cached: Vec<(i64, bool)> = rc.cached_calibrated.iter().map(|(k, v)| (*k, *v)).collect();
        cached.sort();
        (
            files,
            rc.warnings.clone(),
            rc.runtime_exclusions.clone(),
            cached,
            rows,
            progress,
        )
    };
    let per_group = run(0);
    let merged = run(1);
    let baseline = run(2);
    assert!(!per_group.0.is_empty(), "the fixture calibrates something");
    assert_eq!(per_group.0.len(), merged.0.len());
    assert_eq!(per_group.1, merged.1, "warnings in the same order");
    assert_eq!(per_group.2, merged.2, "exclusions");
    assert_eq!(per_group.3, merged.3, "cache flags");
    // The artifact rows, in commit order (ORDER BY id): group, then frame, same hash and size.
    assert!(!per_group.4.is_empty(), "the fixture writes calibrated rows");
    assert_eq!(per_group.4, merged.4, "calibrated rows and their commit order");
    // The pre-L4 order — one stage run per group — for the lists and the rows.
    assert_eq!(baseline.4, merged.4, "rows against the pre-L4 group-by-group order");
    assert_eq!(baseline.1, merged.1, "warnings against the pre-L4 interleaving");
    assert_eq!(baseline.2, merged.2, "exclusions against the pre-L4 interleaving");
    assert_eq!(baseline.0.len(), merged.0.len());
    // Review I1: the stage-wide progress never steps back and lands on its total, in both shapes.
    for (name, shot) in [("per-group", &per_group), ("merged", &merged)] {
        assert!(!shot.5.is_empty(), "{name}: no progress events");
        assert!(
            shot.5.windows(2).all(|w| w[0].0 <= w[1].0),
            "{name}: calibrate progress went backwards: {:?}",
            shot.5
        );
        let (last, total) = *shot.5.last().unwrap();
        assert_eq!(last, total, "{name}: the last event lands on total: {:?}", shot.5);
    }
    for (a, b) in per_group.0.iter().zip(&merged.0) {
        assert_eq!(a.0, b.0);
        assert!(a.1 == b.1, "{} differs between the two queue shapes", a.0);
    }
}

/// Cancels the run on the first Calibrate tick that carries a frame (a worker finishing inside the queue).
struct CancelOnCalibrateTick {
    inner: Recording,
    cancel: Arc<AtomicBool>,
}

impl ProgressEmitter for CancelOnCalibrateTick {
    fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
        if event_name == STACKING_PROGRESS_EVENT
            && payload["stage"] == Stage::Calibrate.as_str()
            && payload["current"].as_u64().unwrap_or(0) > 0
        {
            self.cancel.store(true, Ordering::SeqCst);
        }
        self.inner.emit_json(event_name, payload);
    }
}

/// Task 13c final review A, M1: a cancel during the merged Calibrate queue still replays every stashed phase-1
/// list (group 0's warnings and exclusions reach `rc.warnings` / `rc.runtime_exclusions`).
#[test]
fn calibrate_cancel_in_merged_queue_replays_phase1_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) = seed_run_and_groups(
        &fixture.conn,
        fixture.set_id,
        &plan_groups,
        working.path(),
        output.path(),
    );
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let cancel = Arc::new(AtomicBool::new(false));
    let emitter = Arc::new(CancelOnCalibrateTick {
        inner: Recording::new(),
        cancel: cancel.clone(),
    });
    let mut rc = test_context(
        ctx.clone(),
        emitter as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    rc.cancel = cancel;
    rc.rerun_from = Some(Stage::Calibrate);
    // The fixture's per-group hot-pixel warnings are stashed per group before the merged queue runs.
    let r = stage_calibrate_with(&mut rc, Some(true));
    assert!(matches!(r, Err(RunError::Cancelled)), "{r:?}");
    let w2 = tempfile::tempdir().unwrap();
    let mut uncancelled = test_context(
        ctx.clone(),
        Arc::new(Recording::new()) as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        WorkingLayout::new(w2.path(), &set_slug(SET_NAME)),
        output.path().to_path_buf(),
        seed_run_and_groups(
            &fixture.conn,
            fixture.set_id,
            &plan_groups,
            working.path(),
            output.path(),
        )
        .1,
    );
    uncancelled.rerun_from = Some(Stage::Calibrate);
    stage_calibrate_with(&mut uncancelled, Some(true)).unwrap();
    // Every warning the full merged run raised is present after the cancel (phase-1 ones replayed, commit-time
    // ones are a subset of the full run's).
    for w in &rc.warnings {
        assert!(uncancelled.warnings.contains(w), "{w}");
    }
    assert!(
        !rc.warnings.is_empty(),
        "the stashed lists were dropped by the cancel (the fixture raises a hot-pixel warning per group)"
    );
    assert_eq!(rc.warnings, uncancelled.warnings, "every stashed warning, in group order");
}

/// Task 13c L4 review I2: the memory budget is read right before EACH fan-out, so a settings change between
/// two groups' fan-outs is seen by the second group (Calibrate, per-group queues).
#[test]
fn calibrate_reads_the_budget_before_each_groups_fan_out() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let mut ctx_raw = ServiceContext::new_for_tests(db_path.clone());
    ctx_raw.image_pool = Arc::new(rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap());
    let ctx = Arc::new(ctx_raw);
    let (fixture, _fine, _coarse, _tiny, working, output) = seed_two_geometry_groups(&db_path);
    let cfg = StackingConfig::default();
    let plan_groups = group_frames(&fixture.conn, fixture.set_id, &cfg.grouping).unwrap();
    let (run_id, group_ids) =
        seed_run_and_groups(&fixture.conn, fixture.set_id, &plan_groups, working.path(), output.path());
    let set_budget = |mb: u64| {
        let conn = ctx.db.get().unwrap().conn();
        ctx.settings
            .persist_setting(&conn, crate::settings::keys::COMPUTE_MEMORY_BUDGET_MB, &mb.to_string())
            .unwrap();
    };
    set_budget(2048);
    let w = tempfile::tempdir().unwrap();
    let layout = WorkingLayout::new(w.path(), &set_slug(SET_NAME));
    let switch = Arc::new(BudgetSwitch {
        ctx: ctx.clone(),
        stage: Stage::Calibrate,
        group_key: plan_groups[0].key.clone(),
        to_mb: 1024,
        done: AtomicBool::new(false),
    });
    let mut rc = test_context(
        ctx.clone(),
        switch.clone() as Arc<dyn ProgressEmitter>,
        run_id,
        fixture.set_id,
        SET_NAME,
        cfg.clone(),
        plan_groups.clone(),
        layout,
        output.path().to_path_buf(),
        group_ids,
    );
    rc.rerun_from = Some(Stage::Calibrate);
    let (res, text) =
        crate::stacking::telemetry::capture_logs(|| stage_calibrate_with(&mut rc, Some(false)));
    res.unwrap();
    assert!(switch.done.load(Ordering::SeqCst), "the budget was switched mid-stage");
    let first = admitted_budgets(&text, Stage::Calibrate, &plan_groups[0].key);
    assert_eq!(first, vec![2048u64 << 20], "the first group ran on the old budget: {text}");
    for g in &plan_groups[1..] {
        let later = admitted_budgets(&text, Stage::Calibrate, &g.key);
        assert!(!later.is_empty(), "no admission line for {}: {text}", g.key);
        assert!(
            later.iter().all(|&b| b == 1024u64 << 20),
            "{} did not see the new budget: {later:?}",
            g.key
        );
    }
}

/// Step 3's cache rule (ruling R-M4d-1): a frame calibrated by an
/// earlier run that wanted no mosaic is NOT reused as-is once
/// `drizzle.bayer` goes on — the run regenerates the pair in one
/// generation. And it costs nothing the other way round: the calibrated
/// artifact's own hash is unchanged by the toggle, so a set does not
/// recalibrate when the mosaic is already there.
#[test]
fn turning_bayer_on_regenerates_the_mosaic_of_an_already_calibrated_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_osc_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let run_with_bayer = |bayer: bool| {
        let mut cfg = StackingConfig::default();
        cfg.drizzle.enabled = true;
        cfg.drizzle.scale = 1;
        cfg.drizzle.bayer = bayer;
        cfg.output.cleanup = CleanupPolicy::KeepAll;
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
    };
    let hashes = || -> Vec<(Option<i64>, String)> {
        let mut rows = crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated"),
        )
        .unwrap()
        .into_iter()
        .map(|a| (a.frame_id, a.config_hash))
        .collect::<Vec<_>>();
        rows.sort();
        rows
    };

    run_with_bayer(false);
    let before = hashes();
    assert_eq!(before.len(), light_ids.len(), "{before:?}");
    assert!(
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated_mosaic")
        )
        .unwrap()
        .is_empty(),
        "no mosaic is kept while bayer is off"
    );

    run_with_bayer(true);
    let mosaics = crate::db::stacking::list_artifacts(
        &fixture.conn,
        &StackSubject::FrameSet(fixture.set_id),
        Some("calibrated_mosaic"),
    )
    .unwrap();
    assert_eq!(
        mosaics.len(),
        light_ids.len(),
        "every already-calibrated frame regenerates its mosaic: {mosaics:?}"
    );
    for m in &mosaics {
        assert!(Path::new(&m.path.clone().unwrap()).exists());
    }
    assert_eq!(
        hashes(),
        before,
        "drizzle.bayer must not change a calibrated frame's own hash"
    );
}

/// Fix round 1 (I1): `bayer` on with the OSC debayer OFF must not want a
/// mosaic no generation can produce. Before the fix, `want_mosaic` was
/// true, the generator wrote nothing, the artifact never appeared and the
/// group recalibrated from scratch on EVERY run, with only a `debug!`.
/// Now the run says so once, keeps no mosaic, and the second run reuses
/// every calibrated frame.
#[test]
fn bayer_with_the_debayer_off_says_so_once_and_stays_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_osc_star_group(&db_path, SET_NAME, &shifts, &noise);
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let cfg = || {
        let mut c = StackingConfig::default();
        c.drizzle.enabled = true;
        c.drizzle.scale = 1;
        c.drizzle.bayer = true;
        c.calibration.debayer_osc = false;
        c.output.cleanup = CleanupPolicy::KeepAll;
        c
    };
    let run_once = || -> RunSummary {
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg()),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        serde_json::from_str(row.summary_json.as_deref().unwrap()).unwrap()
    };

    let first = run_once();
    let expected = "Bayer drizzle is ignored: the OSC debayer is off for this run, so no \
                        calibrated CFA mosaic is kept and drizzle deposits each frame's \
                        calibrated planes as they are.";
    assert!(
        first.warnings.iter().any(|w| w == expected),
        "the contradiction must be reported once: {:?}",
        first.warnings
    );
    assert_eq!(
        first
            .warnings
            .iter()
            .filter(|w| w.starts_with("Bayer drizzle is ignored"))
            .count(),
        1,
        "once per run, not once per group or frame: {:?}",
        first.warnings
    );
    assert!(
        crate::db::stacking::list_artifacts(
            &fixture.conn,
            &StackSubject::FrameSet(fixture.set_id),
            Some("calibrated_mosaic")
        )
        .unwrap()
        .is_empty(),
        "no mosaic can exist while the debayer is off"
    );

    let second = run_once();
    let frames: Vec<&SummaryFrame> =
        second.groups.iter().flat_map(|g| g.frames.iter()).collect();
    assert_eq!(frames.len(), light_ids.len(), "{frames:?}");
    assert!(
        frames.iter().all(|f| f.cached_calibrated),
        "the second run must reuse every calibrated frame: {:?}",
        frames
            .iter()
            .map(|f| (f.frame_id, f.cached_calibrated))
            .collect::<Vec<_>>()
    );
}

/// Fix round 1 (I2): the plan gate and the run must agree about what the
/// first `bayer` run will do. A fully calibrated set used to report
/// Calibrate fresh and `calibratedCached = N` while the run was about to
/// regenerate all N pairs — and only for the OSC group, which is what
/// makes the mono half of this set the control.
#[test]
fn the_plan_reports_the_mosaics_a_bayer_run_still_owes() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));

    // One OSC group and one mono group in the same set (the bucket key's
    // own colour axis splits them), four frames each.
    let fixture_conn = rusqlite::Connection::open(&db_path).unwrap();
    let fixture = test_fixtures::frame_set_with_conn(fixture_conn, SET_NAME);
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let mut osc_ids = Vec::new();
    let mut mono_ids = Vec::new();
    for (i, &(dx, dy)) in shifts.iter().enumerate() {
        for (osc, ids) in [(true, &mut osc_ids), (false, &mut mono_ids)] {
            let date_obs = date_obs_at(i);
            let stem = format!("{}{i}", if osc { "osc" } else { "mono" });
            let spec = test_fixtures::LightSpec {
                bayerpat: if osc { Some("RGGB") } else { None },
                ..star_light_spec(&stem, &date_obs)
            };
            let (id, _path) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &shifted_stars(dx, dy),
                600.0,
                5.0,
                100 + i as u64,
            );
            ids.push(id);
        }
    }
    let working = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_WORKING_DIR,
        working.path().to_str().unwrap(),
    )
    .unwrap();
    crate::db::set_setting(
        &fixture.conn,
        crate::settings::keys::STACKING_OUTPUT_DIR,
        output.path().to_str().unwrap(),
    )
    .unwrap();
    let all_ids: Vec<i64> = osc_ids.iter().chain(mono_ids.iter()).copied().collect();
    test_fixtures::add_master_dark_and_flat(
        &fixture,
        &all_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let cfg = |bayer: bool| {
        let mut c = StackingConfig::default();
        c.drizzle.enabled = true;
        c.drizzle.scale = 1;
        c.drizzle.bayer = bayer;
        c.output.cleanup = CleanupPolicy::KeepAll;
        c
    };
    let run_with = |bayer: bool| {
        let started = start_stacking(
            ctx.clone(),
            Arc::new(Recording::new()),
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(fixture.set_id),
            Some(cfg(bayer)),
            None,
        )
        .expect("start should succeed");
        wait_for_run(&ctx, started.run_id);
        let row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "done", "{row:?}");
    };
    let plan_with = |bayer: bool| {
        build_plan(
            &fixture.conn,
            &ctx.settings,
            &PathPolicy::AllowAll,
            &StackSubject::FrameSet(fixture.set_id),
            Some(cfg(bayer)),
        )
        .expect("the plan should build")
    };
    let group_of = |plan: &crate::stacking::plan::StackingPlan, osc: bool| {
        plan.groups
            .iter()
            .find(|g| g.color_mode == if osc { ColorMode::Osc } else { ColorMode::Mono })
            .expect("both groups planned")
            .clone()
    };

    // Calibrated by a run that wanted no mosaic.
    run_with(false);
    let plan_off = plan_with(false);
    assert!(
        !plan_off.stale_stages.contains(&Stage::Calibrate),
        "a non-bayer plan over a calibrated set is fresh: {:?}",
        plan_off.stale_stages
    );
    assert_eq!(group_of(&plan_off, true).calibrated_cached, 4);
    assert_eq!(group_of(&plan_off, false).calibrated_cached, 4);

    // The same set, planned with Bayer drizzle on: the OSC group owes
    // four mosaics, the mono group owes nothing.
    let plan_on = plan_with(true);
    assert!(
        plan_on.stale_stages.contains(&Stage::Calibrate),
        "the OSC group's missing mosaics make Calibrate stale: {:?}",
        plan_on.stale_stages
    );
    assert_eq!(
        group_of(&plan_on, true).calibrated_cached,
        0,
        "every OSC frame is regenerated as a pair"
    );
    assert_eq!(
        group_of(&plan_on, false).calibrated_cached,
        4,
        "the mono group ignores drizzle.bayer entirely"
    );

    // And after the bayer run, fresh again.
    run_with(true);
    let plan_after = plan_with(true);
    assert!(
        !plan_after.stale_stages.contains(&Stage::Calibrate),
        "{:?}",
        plan_after.stale_stages
    );
    assert_eq!(group_of(&plan_after, true).calibrated_cached, 4);
    assert_eq!(group_of(&plan_after, false).calibrated_cached, 4);
}
