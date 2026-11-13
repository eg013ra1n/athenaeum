//! Stage 1 for lights that are not calibrated here (`stage_prepare`): a project's frames and attested lights are
//! prepared instead.

use super::*;

/// One frame's Prepare RESOLVE outcome (plan 6b-2 C12), the twin of
/// [`CalibrateResolved`].
enum PrepareResolved {
    Reused { bytes: u64 },
    Excluded { reason: String },
    Job(Box<PrepareRunJob>),
}

/// One frame's resolved Prepare work: the pure job plus what the commit
/// writes back.
struct PrepareRunJob {
    frame_id: i64,
    group_key: String,
    /// The frame's `calibrated` / `calibrated_mosaic` rows as the resolve step found them, so the commit can remove a
    /// file the rewrite replaced ([`replace_frame_level_file`]).
    old_calibrated: Option<crate::db::stacking::StackingArtifactRow>,
    old_mosaic: Option<crate::db::stacking::StackingArtifactRow>,
    hash: String,
    /// Bayer drizzle wants this frame's pre-debayer mosaic kept.
    keep_mosaic: bool,
    frame_uuid: Option<String>,
    job: crate::stacking::prepare::PrepareJob,
    /// Keeps the zip cache file `job.src` names from eviction until the job (and its commit) ends. `None` for
    /// a frame on disk.
    _pin: Option<crate::zip_store::cache::Pinned>,
    /// `job.src` is a zip-cache file and the action would hand that very file on as the stage-1 artifact (a
    /// float FITS used in place, or a mosaic kept as `calibrated_mosaic`): the cache may evict it, so the
    /// worker copies it to `copy_to` first and reads that copy instead.
    detach: bool,
}

/// Extracts one zip-resident frame for Prepare on the run's cancel flag. `Ok(Err(excluded))`: the frame
/// cannot be read (it is excluded, not a failed run); `Err(Cancelled)` when the run was cancelled meanwhile.
fn extract_for_prepare(
    rc: &RunContext,
    frame_id: i64,
    catalog_path: &Path,
) -> Result<
    Result<(PathBuf, Option<crate::zip_store::cache::Pinned>), PrepareResolved>,
    RunError,
> {
    match crate::zip_store::materialize::materialize_one_cancellable(
        &rc.ctx,
        &catalog_path.to_string_lossy(),
        &rc.cancel,
    ) {
        Ok(found) => Ok(Ok(found)),
        Err(e) => {
            rc.check_cancel()?;
            tracing::error!(
                run_id = rc.run_id,
                frame_id,
                path = %catalog_path.display(),
                error = %format!("{e:#}"),
                "frame could not be read from its zip"
            );
            Ok(Err(PrepareResolved::Excluded {
                reason: format!("cannot read the file: {e:#}"),
            }))
        }
    }
}

/// Prepare's per-frame RESOLVE phase, on the run thread:
/// - the identity check (C9's Prepare boundary);
/// - the stage-1 hash through `rc.memo`;
/// - the header-only classification;
/// - the plane-count check against the group's colour (review I12);
/// - the freshness lookup.
/// A frame whose file no reader decodes, or whose planes contradict its
/// group, is excluded here.
fn resolve_prepare_job(
    rc: &mut RunContext,
    cfg: &StackingConfig,
    group: &IntegrationGroup,
    frame: &GroupFrame,
) -> Result<PrepareResolved, RunError> {
    use crate::stacking::prepare::{
        prepare_action, probe_file, probe_zip_entry, PrepareAction, PrepareJob,
    };
    if let Some(reason) = changed_during_run(rc, frame.frame_id)? {
        return Ok(PrepareResolved::Excluded { reason });
    }
    let hash = {
        let conn = db(&rc.ctx)?.conn();
        match rc.memo.calibration_hash_checked(&conn, cfg, frame) {
            Ok(hash) => hash,
            Err(e) => {
                return Ok(PrepareResolved::Excluded {
                    reason: format!("preparation failed: {e}"),
                })
            }
        }
    };
    let mut input =
        crate::stacking::origin::prepare_input(origin_of(&rc.origins, frame.frame_id)?, frame);
    // A frame inside a zip is classified from its entry's header bytes, so the plane check and the
    // freshness lookup below decide BEFORE any extraction; only a frame that needs real work is extracted
    // (the pin lives in the job until it ends). A path with no zip row passes through untouched.
    let catalog_path = input.path.clone();
    let zip_markers = {
        let conn = db(&rc.ctx)?.conn();
        crate::zip_store::presence::zip_markers(&conn, &catalog_path.to_string_lossy())
    };
    let header_shape = zip_markers.as_ref().and_then(|(zip, entry)| {
        match probe_zip_entry(Path::new(zip), entry, &catalog_path) {
            Ok(shape) => Some(shape),
            Err(e) => {
                // Not decided here: the extraction below reports the cause in the frame's exclusion.
                tracing::warn!(
                    run_id = rc.run_id,
                    frame_id = frame.frame_id,
                    path = %catalog_path.display(),
                    error = %format!("{e:#}"),
                    "zip entry header unreadable; the frame is extracted to be classified"
                );
                None
            }
        }
    });
    let mut pin: Option<crate::zip_store::cache::Pinned> = None;
    let header_classified = header_shape.is_some();
    let shape = match header_shape {
        Some(shape) => shape,
        None => {
            if zip_markers.is_some() {
                match extract_for_prepare(rc, frame.frame_id, &catalog_path)? {
                    Ok((local, extracted_pin)) => {
                        input.path = local;
                        pin = extracted_pin;
                    }
                    Err(excluded) => return Ok(excluded),
                }
            }
            probe_file(&input.path)
        }
    };
    let action = prepare_action(&shape, cfg.calibration.debayer_osc);
    if let PrepareAction::Unsupported(why) = &action {
        return Ok(PrepareResolved::Excluded {
            reason: format!("unsupported file: {why}"),
        });
    }
    // Review I12 (C13): the group's colour comes from the manifest bayerpat
    // (or, without one, the file's plane count; a frame set uses the catalog
    // BAYERPAT or the attested rule), Prepare's debayer from the file's own
    // header. A file whose
    // post-Prepare plane count contradicts its group never reaches Measure.
    let planes = match &action {
        PrepareAction::Write {
            debayer: Some(_), ..
        } => 3,
        _ => shape.planes,
    };
    if planes != expected_stage_one_planes(cfg, group) {
        return Ok(PrepareResolved::Excluded {
            reason: format!("plane count {planes} disagrees with the group's colour mode"),
        });
    }
    let keep_mosaic = wants_cfa_mosaic(cfg, group)
        && matches!(
            action,
            PrepareAction::Write {
                debayer: Some(_),
                ..
            }
        );

    // The frame's rows sit under the frame-level key (spec §5.1). Read even under `rerun_from = Calibrate` (no reuse
    // then): the commit removes the file a rewrite replaces.
    let (existing, mosaic_row) = {
        let conn = db(&rc.ctx)?.conn();
        let existing = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group.key,
            ArtifactKind::Calibrated,
            Some(frame.frame_id),
        )?;
        let mosaic_row = crate::db::stacking::find_artifact(
            &conn,
            &rc.subject,
            &group.key,
            ArtifactKind::CalibratedMosaic,
            Some(frame.frame_id),
        )?;
        (existing, mosaic_row)
    };
    // The in-place rule (file-identity §4.1): a row that names the user's own light (or a project replica), not a
    // working-folder file, is fresh only while it names THIS frame's current input. After a relocate the row can
    // name the light's old path, where another file of the same size may now sit; a miss is a Job, which re-points
    // the row. A zip frame used in place is detached into `prepared/`, an own file, so it has no expectation.
    let expected_calibrated = (zip_markers.is_none() && matches!(action, PrepareAction::InPlace))
        .then(|| catalog_path.clone());
    let expected_mosaic = (zip_markers.is_none()
        && keep_mosaic
        && matches!(
            action,
            PrepareAction::Write {
                read: crate::stacking::prepare::PrepareRead::InPlaceFits,
                ..
            }
        ))
    .then(|| catalog_path.clone());
    let names_expected = |row: &crate::db::stacking::StackingArtifactRow, expected: &Option<PathBuf>| {
        expected
            .as_deref()
            .is_none_or(|input| row.path.as_deref().is_some_and(|p| Path::new(p) == input))
    };
    if rc.rerun_from != Some(Stage::Calibrate) {
        let mosaic_fresh = !keep_mosaic
            || mosaic_row.as_ref().is_some_and(|row| {
                is_fresh(row, &hash) && names_expected(row, &expected_mosaic)
            });
        if let Some(row) = &existing {
            if is_fresh(row, &hash) && names_expected(row, &expected_calibrated) && mosaic_fresh {
                return Ok(PrepareResolved::Reused {
                    bytes: row.size.unwrap_or(0) as u64,
                });
            }
        }
    }

    // One stem rule for every frame-level file (spec §5.1): the frame's own, `<stem>_f<id>`.
    let stem = frame_file_stem(frame);
    let dir = rc.layout.prepared_dir();
    let converted_out = dir.join(format!("p_{stem}.fits"));
    let debayered_out = dir.join(format!("p_{stem}_d.fits"));
    // A zip frame that needs real work (not Reused above) is extracted only now.
    if zip_markers.is_some() && pin.is_none() && header_classified {
        match extract_for_prepare(rc, frame.frame_id, &catalog_path)? {
            Ok((local, extracted_pin)) => {
                input.path = local;
                pin = extracted_pin;
            }
            Err(excluded) => return Ok(excluded),
        }
    }
    // The replica is sacred: a write destination is never the file Prepare
    // reads. The names live under `prepared/` so this cannot happen; the
    // check is the guard if a layout change ever breaks that.
    if converted_out == input.path || debayered_out == input.path {
        tracing::error!(
            run_id = rc.run_id,
            frame_id = frame.frame_id,
            path = %input.path.display(),
            "prepare output would overwrite its source; frame excluded"
        );
        debug_assert!(false, "a prepare output equals its source");
        return Ok(PrepareResolved::Excluded {
            reason: "preparation failed: the output path equals the source".to_string(),
        });
    }
    // The artifact must outlive the cache entry: an in-place read (the file itself becomes the
    // `calibrated` artifact, or the kept mosaic) is detached into `prepared/` for a zip-extracted frame.
    let detach = pin.is_some()
        && matches!(
            action,
            PrepareAction::InPlace
                | PrepareAction::Write {
                    read: crate::stacking::prepare::PrepareRead::InPlaceFits,
                    ..
                }
        );
    // The write order (a row is gone before the file it names is replaced): a cancelled or killed run then leaves no
    // row of this hash over another config's file. `Reused` and `Excluded` returned above and delete nothing. The
    // mosaic row goes too when the run keeps a mosaic or its file is one this job overwrites.
    {
        let conn = db(&rc.ctx)?.conn();
        crate::db::stacking::delete_artifact(
            &conn,
            &rc.subject,
            &group.key,
            ArtifactKind::Calibrated,
            Some(frame.frame_id),
        )?;
        let mosaic_names_output = mosaic_row
            .as_ref()
            .and_then(|row| row.path.as_deref())
            .is_some_and(|path| {
                let path = Path::new(path);
                path == converted_out.as_path() || path == debayered_out.as_path()
            });
        if keep_mosaic || mosaic_names_output {
            crate::db::stacking::delete_artifact(
                &conn,
                &rc.subject,
                &group.key,
                ArtifactKind::CalibratedMosaic,
                Some(frame.frame_id),
            )?;
        }
        // Any other subject's row over a file this job writes goes too (two sets with one name share a root). Both
        // outputs lie under `prepared/`; the user's own file, used in place, is never one of them.
        for written in [&converted_out, &debayered_out] {
            crate::db::stacking::delete_artifacts_naming_path(&conn, &written.to_string_lossy())?;
        }
    }
    Ok(PrepareResolved::Job(Box::new(PrepareRunJob {
        frame_id: frame.frame_id,
        group_key: group.key.clone(),
        old_calibrated: existing,
        old_mosaic: mosaic_row,
        hash,
        keep_mosaic,
        frame_uuid: input.frame_uuid,
        _pin: pin,
        detach,
        job: PrepareJob {
            src: input.path,
            shape,
            action,
            converted_out,
            debayered_out,
            // Regenerable intermediates, like Calibrate's (perf tier 1 Task 6).
            durability: crate::fits_writer::Durability::Volatile,
        },
    })))
}

/// For a job whose source is a zip-cache file that Prepare would hand on in place, the job re-pointed at a
/// copy in `prepared/` (written beside its final name and renamed, so a killed run leaves no half file under
/// the artifact's name). `None` when the job needs no copy.
fn detach_zip_source(job: &PrepareRunJob) -> anyhow::Result<Option<crate::stacking::prepare::PrepareJob>> {
    if !job.detach {
        return Ok(None);
    }
    let dest = &job.job.converted_out;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("creating {}: {e}", parent.display()))?;
    }
    let partial = crate::zip_store::reader::partial_path(dest);
    if let Err(e) = std::fs::copy(&job.job.src, &partial) {
        // A copy that fails halfway (disk full) leaves its partial; a retry would add another.
        let _ = std::fs::remove_file(&partial);
        return Err(anyhow::anyhow!(
            "copying {} out of the zip cache: {e}",
            job.job.src.display()
        ));
    }
    std::fs::rename(&partial, dest).map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        anyhow::anyhow!("placing {}: {e}", dest.display())
    })?;
    Ok(Some(crate::stacking::prepare::PrepareJob {
        src: dest.clone(),
        ..job.job.clone()
    }))
}

/// Write one prepared frame's artifact rows (`calibrated`, plus
/// `calibrated_mosaic` when kept) in one transaction; returns the bytes the
/// frame's `calibrated` file holds.
fn commit_prepared(
    rc: &mut RunContext,
    job: &PrepareRunJob,
    files: crate::stacking::prepare::PreparedFiles,
) -> Result<u64, RunError> {
    let (size, modified_at) =
        file_identity(&files.calibrated).map_err(|e| RunError::Other(format!("{e:#}")))?;
    let mosaic = match (&files.mosaic, job.keep_mosaic) {
        (Some(path), true) => Some((
            path.clone(),
            file_identity(path).map_err(|e| RunError::Other(format!("{e:#}")))?,
        )),
        _ => None,
    };
    tracing::debug!(
        run_id = rc.run_id,
        frame_id = job.frame_id,
        frame_uuid = job.frame_uuid.as_deref(),
        group_key = %job.group_key,
        prepared = files.prepared,
        "frame prepared"
    );
    // A deferred transaction on purpose, as `commit_calibrated` does: these
    // are write-only upserts, so the `BEGIN IMMEDIATE` rule for
    // read-then-write does not apply (review M20).
    let mut conn = db(&rc.ctx)?.conn();
    let tx = conn
        .transaction()
        .map_err(|e| RunError::Other(format!("opening the artifact transaction: {e}")))?;
    upsert_artifact(
        &tx,
        &NewArtifact {
            subject: &rc.subject,
            frame_id: Some(job.frame_id),
            group_key: &job.group_key,
            kind: ArtifactKind::Calibrated,
            path: files.calibrated.to_str(),
            config_hash: &job.hash,
            size: Some(size),
            modified_at: Some(&modified_at),
            payload_json: None,
        },
    )?;
    if let Some((path, (mosaic_size, mosaic_modified))) = &mosaic {
        upsert_artifact(
            &tx,
            &NewArtifact {
                subject: &rc.subject,
                frame_id: Some(job.frame_id),
                group_key: &job.group_key,
                kind: ArtifactKind::CalibratedMosaic,
                path: path.to_str(),
                config_hash: &job.hash,
                size: Some(*mosaic_size),
                modified_at: Some(mosaic_modified),
                payload_json: None,
            },
        )?;
    }
    tx.commit()
        .map_err(|e| RunError::Other(format!("committing the artifact rows: {e}")))?;
    // The rows now point at the new files; the files they used to point at go once nothing references them. Only
    // under `prepared/`: a landed file Prepare used in place is never ours (ruling R4).
    let owned = rc.layout.prepared_root();
    replace_frame_level_file(&conn, &owned, job.old_calibrated.as_ref(), &files.calibrated);
    if let Some((path, _)) = &mosaic {
        replace_frame_level_file(&conn, &owned, job.old_mosaic.as_ref(), path);
    }
    Ok(size as u64)
}

/// Stage 1 for a project or an attested set (spec §5.1, §13; plan 6b-2 C12).
/// Calibrate's shape:
/// - per group, a resolve pass on the run thread ([`resolve_prepare_job`]);
/// - a fan-out of the pixel work across the image pool ([`execute_prepare`]:
///   nothing for a file used in place, a conversion, or a debayer);
/// - commits in frame order ([`commit_prepared`]).
/// A per-frame failure excludes that frame; a cancel unwinds as
/// [`RunError::Cancelled`]. Progress and timing report under
/// `Stage::Calibrate`, the stage-1 slot.
///
/// [`execute_prepare`]: crate::stacking::prepare::execute_prepare
pub(super) fn stage_prepare(rc: &mut RunContext) -> Result<(), RunError> {
    let stage_start = Instant::now();
    let meter = crate::stacking::telemetry::StageMeter::start(rc, Stage::Calibrate, None);
    let excluded_set: HashSet<i64> = rc.excluded.iter().copied().collect();
    let groups = rc.plan_groups.clone();
    let cfg = rc.config.clone();
    let total: usize = groups
        .iter()
        .flat_map(|g| g.frames.iter())
        .filter(|f| !excluded_set.contains(&f.frame_id))
        .count();
    let bytes_total = calibrate_bytes_total(&groups, &excluded_set);

    tracing::info!(
        run_id = rc.run_id,
        set_id = rc.subject.frame_set_id(),
        project_id = rc.subject.project_id(),
        count = total,
        groups = groups.len(),
        config_hash = %rc.hash,
        "stacking run started"
    );
    warn_bayer_drizzle_ignored(rc, &cfg, &groups);

    let mut current = 0usize;
    let mut bytes_done = 0u64;
    rc.progress(
        Stage::Calibrate,
        None,
        current,
        total,
        bytes_done,
        bytes_total,
        None,
        None,
    );

    for group in &groups {
        rc.check_cancel()?;
        let group_max_w = group
            .frames
            .iter()
            .map(|f| f.width.max(0) as u64)
            .max()
            .unwrap_or(0);
        let group_max_h = group
            .frames
            .iter()
            .map(|f| f.height.max(0) as u64)
            .max()
            .unwrap_or(0);

        let mut jobs: Vec<PrepareRunJob> = Vec::new();
        for frame in &group.frames {
            if excluded_set.contains(&frame.frame_id) {
                continue;
            }
            rc.check_cancel()?;
            match resolve_prepare_job(rc, &cfg, group, frame)? {
                PrepareResolved::Reused { bytes } => {
                    bytes_done += bytes;
                    rc.cached_calibrated.insert(frame.frame_id, true);
                    current += 1;
                    rc.progress(
                        Stage::Calibrate,
                        Some(group.key.clone()),
                        current,
                        total,
                        bytes_done,
                        bytes_total,
                        Some(frame.frame_id),
                        None,
                    );
                }
                PrepareResolved::Excluded { reason } => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = frame.frame_id,
                        reason = %reason,
                        "frame excluded"
                    );
                    rc.runtime_exclusions.push((frame.frame_id, reason));
                    current += 1;
                    rc.progress(
                        Stage::Calibrate,
                        Some(group.key.clone()),
                        current,
                        total,
                        bytes_done,
                        bytes_total,
                        Some(frame.frame_id),
                        None,
                    );
                }
                PrepareResolved::Job(job) => jobs.push(*job),
            }
        }
        if jobs.is_empty() {
            continue;
        }

        // Conservative for Prepare: an OSC group with the debayer on is charged the debayered footprint.
        let working_set_bytes = calibrate_working_set_bytes(
            group_max_w,
            group_max_h,
            group.color_mode,
            cfg.calibration.debayer_osc,
        );
        let budget = fan_budget(rc);
        let admission_n = admission(
            working_set_bytes,
            rc.ctx.image_pool.current_num_threads(),
            budget.bytes,
        );
        log_admission(
            rc,
            Stage::Calibrate,
            &group.key,
            working_set_bytes,
            0,
            admission_n,
            budget,
            "prepare",
        );
        let ticker = FanOutTicker::new(
            rc,
            Stage::Calibrate,
            Some(group.key.clone()),
            current,
            total,
            bytes_done,
            bytes_total,
        );
        let ticker_ref = &ticker;
        let cancel_ref: &AtomicBool = &rc.cancel;
        let pool_ref: &Arc<rayon::ThreadPool> = &rc.ctx.image_pool;
        let job_refs: Vec<&PrepareRunJob> = jobs.iter().collect();
        let results = fan_out(
            job_refs,
            &AdmissionGate::budgeted(rc, working_set_bytes, 0, admission_n),
            FanOutLabel::new(rc, Stage::Calibrate, Some(&group.key), "prepare"),
            cancel_ref,
            move |job: &PrepareRunJob| {
                if stage_one_job_cancelled(&job.job.converted_out, job.frame_id)
                    || stage_one_job_cancelled(&job.job.debayered_out, job.frame_id)
                {
                    ticker_ref.tick(Some(job.frame_id));
                    return Err(CANCELLED_MARKER.to_string());
                }
                let out = detach_zip_source(job)
                    .and_then(|prepared_job| {
                        crate::stacking::prepare::execute_prepare(
                            prepared_job.as_ref().unwrap_or(&job.job),
                            Some(pool_ref),
                            cancel_ref,
                        )
                    })
                    .map_err(|e| {
                        if matches!(
                            e.downcast_ref::<IntegrationError>(),
                            Some(IntegrationError::Cancelled)
                        ) {
                            CANCELLED_MARKER.to_string()
                        } else {
                            format!("preparation failed: {e:#}")
                        }
                    });
                ticker_ref.tick(Some(job.frame_id));
                out
            },
        );
        rc.check_cancel()?;
        current = ticker.done();

        for (pos, res) in results.into_iter().enumerate() {
            let job = &jobs[pos];
            match res {
                None => return Err(RunError::Cancelled),
                Some(Err(msg)) if msg == CANCELLED_MARKER => return Err(RunError::Cancelled),
                Some(Err(msg)) => {
                    tracing::warn!(
                        run_id = rc.run_id,
                        frame_id = job.frame_id,
                        error = %msg,
                        "preparation failed; frame excluded"
                    );
                    rc.runtime_exclusions.push((job.frame_id, msg));
                }
                Some(Ok(files)) => {
                    bytes_done += commit_prepared(rc, job, files)?;
                    rc.cached_calibrated.insert(job.frame_id, false);
                }
            }
        }
        rc.progress(
            Stage::Calibrate,
            Some(group.key.clone()),
            current,
            total,
            bytes_done,
            bytes_total,
            None,
            None,
        );
    }

    meter.finish();
    rc.timings.push(crate::stacking::provenance::StageTiming {
        stage: Stage::Calibrate,
        duration_ms: stage_start.elapsed().as_millis() as u64,
    });
    Ok(())
}

#[cfg(test)]
#[path = "prepare_tests.rs"]
mod tests;
