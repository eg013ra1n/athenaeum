//! The crop pass that closes the output stage (`finish_crop`).

use super::*;

/// Test hook (plan 6b-3 D10 fix round 1): the next `write_crop_item` call
/// fails once, to reach the crop pass's uncropped fallback. One-shot and read
/// only by project runs: those tests hold the grid-counters lock, while
/// concurrent frame-set runs never touch it.
#[cfg(test)]
static FORCE_CROP_FAILURE: AtomicBool = AtomicBool::new(false);

/// Test hook (plan 6b-3 D10 fix round 2): the next `record_crop_item` call
/// fails once, as a database error would. One-shot, project runs only (see
/// [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
static FORCE_RECORD_FAILURE: AtomicBool = AtomicBool::new(false);

/// Test hook (final review): the next `record_crop_item` call fails once AFTER
/// `update_group` succeeded, inside its transaction. One-shot, project runs
/// only (see [`FORCE_CROP_FAILURE`]).
#[cfg(test)]
static FORCE_RECORD_FAILURE_AFTER_GROUP: AtomicBool = AtomicBool::new(false);

/// Plan 6b-3 D10: one group's outputs waiting in the scratch folder for the
/// run's crop rectangle, with everything [`finish_crop`] needs to finalize
/// them without re-reading a header.
pub(crate) struct CropItem {
    pub(super) group_key: String,
    pub(super) group_id: i64,
    /// The group's own `coverage >= K` box; `None` when no pixel reached `K`.
    pub(super) rect: Option<CropRect>,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) channels: usize,
    pub(super) frames: usize,
    pub(super) total_exposure_s: Option<f64>,
    /// The master's cards as written: the uncropped WCS.
    pub(super) cards: Vec<Card>,
    /// The record those cards' WCS block was written from.
    pub(super) wcs: Option<PlateSolveRecord>,
    /// The scratch files.
    pub(super) master: WrittenMaster,
    pub(super) drizzle: Option<CropDrizzle>,
}

pub(crate) struct CropDrizzle {
    pub(super) written: WrittenDrizzle,
    pub(super) cards: Vec<Card>,
    pub(super) scale: u32,
}

/// One group's final files after its crop pass.
struct CropOutputs {
    rect: CropRect,
    master: WrittenMaster,
    /// The drizzle family and its (scaled) rectangle.
    drizzle: Option<(WrittenDrizzle, CropRect)>,
    /// Why the drizzle family was not written (the master family still is).
    drizzle_error: Option<String>,
}

/// Plan 6b-3 D10 (spec §5.9): the crop pass, after the last group of a run
/// with `cropToCoverage = K > 0`. Co-registered: ONE rectangle, the union of
/// the groups' `coverage >= K` boxes (every group shares one pixel grid: the
/// run reference's, or the project's canonical grid), so the run's masters
/// stay pixel-aligned. Native: each group's own box. Each group's outputs
/// are re-read from the scratch folder, cropped and written in the
/// configured format into the output folder, and its rows, group paths and
/// summary paths move onto them. A group with no rectangle, or whose crop
/// fails, is finalized uncropped with a warning (said once the uncropped
/// write has succeeded); a group that cannot be finalized at all keeps its
/// scratch files, which its rows already name — a master is never lost. A
/// drizzle family that cannot be cropped never costs the master (ruling
/// R-M3-7): the master family is finalized and the drizzle family stays in
/// the scratch folder, named in a warning. The scratch folder goes when every
/// output is final; otherwise it stays, and no cleanup policy removes it
/// (`paths::cleanup_work`): it is the user's master data, and only a manual
/// action does. The pass deliberately ignores a cancel — stopping between
/// groups would strand the rest in the scratch folder — so a large drizzle
/// family adds cancel latency; one Output-stage tick per group reports the
/// pass (final review B2) and a cancel that arrived during it is logged once.
/// Database errors fail the run, as everywhere in
/// Output, but only after every group has been tried: a group whose rows
/// could not be updated counts as kept, so the scratch folder (which its rows
/// may still name) survives, and a warning names it.
pub(super) fn finish_crop(rc: &mut RunContext) -> Result<(), RunError> {
    let items = std::mem::take(&mut rc.crop_items);
    if items.is_empty() {
        return Ok(());
    }
    let k = rc.config.output.crop_to_coverage;
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    let run_rect = match rc.config.registration.geometry {
        RegistrationGeometry::CoRegistered => {
            Some(union_rects(items.iter().filter_map(|i| i.rect)))
        }
        RegistrationGeometry::Native => None,
    };
    let mut kept = 0usize;
    let mut record_error: Option<RunError> = None;
    let crop_total = items.len();
    for (crop_done, item) in items.into_iter().enumerate() {
        rc.crop_progress(&item.group_key, crop_done, crop_total);
        let full = CropRect::full(item.width, item.height);
        let wanted = run_rect.unwrap_or(item.rect);
        let (target, note) = match wanted {
            Some(r) if r.fits_within(item.width, item.height) => (r, None),
            Some(r) => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    crop_x0 = r.x0,
                    crop_y0 = r.y0,
                    crop_width = r.width,
                    crop_height = r.height,
                    "crop rectangle outside the group's master; outputs written uncropped"
                );
                let note = format!(
                    "group {}: the run's crop rectangle does not fit its master; \
                     its outputs are written uncropped",
                    item.group_key
                );
                (full, Some(note))
            }
            None => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    count = k,
                    "no pixel reached the crop coverage; outputs written uncropped"
                );
                let note = format!(
                    "group {}: no pixel is covered by {k} frames; \
                     its outputs are written uncropped",
                    item.group_key
                );
                (full, Some(note))
            }
        };
        let result = match write_crop_item(rc, &item, target) {
            Ok(o) => Ok((o, note)),
            Err(e) if target != full => {
                tracing::warn!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    error = %format!("{e:#}"),
                    "crop failed; writing the outputs uncropped"
                );
                match write_crop_item(rc, &item, full) {
                    Ok(o) => {
                        let note = format!(
                            "group {}: the crop failed ({e:#}); its outputs are written uncropped",
                            item.group_key
                        );
                        Ok((o, Some(note)))
                    }
                    Err(e2) => Err(anyhow::anyhow!(
                        "the crop failed ({e:#}); the uncropped write failed too ({e2:#})"
                    )),
                }
            }
            Err(e) => Err(e),
        };
        match result {
            Ok((o, note)) => {
                if let Err(e) = record_crop_item(rc, &item, &o) {
                    // Its rows may still name the scratch folder, so the
                    // folder stays, whatever the other groups do, and the
                    // pass goes on with them. The final files just written
                    // are removed again: nothing names them, and a leftover
                    // would only be an orphan beside a later successful run.
                    kept += 1;
                    remove_partial_outputs(&crop_output_paths(&o));
                    tracing::error!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = ?e,
                        "crop outputs written but their rows could not be updated; \
                         final files removed, scratch kept"
                    );
                    rc.warnings.push(format!(
                        "group {}: its rows could not be updated after the crop; \
                         its just-written final files were removed and the scratch folder {} is kept",
                        item.group_key,
                        scratch.display()
                    ));
                    record_error.get_or_insert(e);
                    continue;
                }
                if let Some(note) = note {
                    rc.warnings.push(note);
                }
                if let Some(msg) = &o.drizzle_error {
                    kept += 1;
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = %msg,
                        "drizzle outputs could not be cropped; kept in the scratch folder"
                    );
                    rc.warnings.push(format!(
                        "group {}: its drizzle outputs could not be cropped ({msg}); \
                         they stay in {}",
                        item.group_key,
                        scratch.display()
                    ));
                }
            }
            Err(e) => {
                kept += 1;
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    path = %scratch.display(),
                    error = %format!("{e:#}"),
                    "crop outputs could not be finalized; kept in the scratch folder"
                );
                rc.warnings.push(format!(
                    "group {}: its outputs could not be finalized ({e:#}); they stay in {}",
                    item.group_key,
                    scratch.display()
                ));
            }
        }
    }
    rc.crop_progress("", crop_total, crop_total);
    if rc.cancel.load(Ordering::SeqCst) {
        tracing::info!(
            run_id = rc.run_id,
            "crop pass finished after a cancel; the outputs are final"
        );
    }
    remove_scratch_unless_kept(rc, &scratch, kept);
    match record_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Every final file one crop item wrote: the master family and, when it
/// was written, the drizzle family.
fn crop_output_paths(o: &CropOutputs) -> Vec<PathBuf> {
    let mut paths = vec![o.master.master.clone()];
    paths.extend(o.master.rejection_low.clone());
    paths.extend(o.master.rejection_high.clone());
    paths.extend(o.master.coverage.clone());
    if let Some((d, _)) = &o.drizzle {
        paths.push(d.drizzle.clone());
        paths.extend(d.weight_map.clone());
        paths.extend(d.coverage.clone());
    }
    paths
}

fn remove_scratch_unless_kept(rc: &RunContext, scratch: &Path, kept: usize) {
    if kept > 0 {
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(scratch) {
        tracing::warn!(
            run_id = rc.run_id,
            path = %scratch.display(),
            error = %error,
            "removing the crop scratch folder failed"
        );
    }
}

/// Plan 6b-3 D10 (fix round 1): the exit path of a crop run that did not
/// reach [`finish_crop`] — cancelled between groups, or failed. Every group
/// already written to the scratch folder is finalized UNCROPPED (the full
/// rectangle through the crop writer), so its master lands in the output
/// folder as a K = 0 run's would. One run warning says so. A group that
/// cannot be finalized keeps its scratch, named in a warning. Nothing here
/// fails: the run is already ending.
pub(super) fn finalize_unfinished_crop(rc: &mut RunContext) {
    let items = std::mem::take(&mut rc.crop_items);
    if items.is_empty() {
        return;
    }
    let scratch = rc.layout.uncropped_dir(rc.run_id);
    let (mut kept, mut finalized) = (0usize, 0usize);
    let crop_total = items.len();
    for (crop_done, item) in items.iter().enumerate() {
        rc.crop_progress(&item.group_key, crop_done, crop_total);
        let full = CropRect::full(item.width, item.height);
        let result = write_crop_item(rc, item, full)
            .map_err(|e| format!("{e:#}"))
            .and_then(|o| {
                record_crop_item(rc, item, &o).map_err(|e| format!("{e:?}"))?;
                Ok(o)
            });
        match result {
            Ok(o) => {
                finalized += 1;
                if let Some(msg) = o.drizzle_error {
                    kept += 1;
                    tracing::warn!(
                        run_id = rc.run_id,
                        group_key = %item.group_key,
                        path = %scratch.display(),
                        error = %msg,
                        "drizzle outputs could not be finalized; kept in the scratch folder"
                    );
                    rc.warnings.push(format!(
                        "group {}: its drizzle outputs could not be finalized ({msg}); \
                         they stay in {}",
                        item.group_key,
                        scratch.display()
                    ));
                }
            }
            Err(msg) => {
                kept += 1;
                tracing::error!(
                    run_id = rc.run_id,
                    group_key = %item.group_key,
                    path = %scratch.display(),
                    error = %msg,
                    "crop outputs could not be finalized; kept in the scratch folder"
                );
                rc.warnings.push(format!(
                    "group {}: its outputs could not be finalized ({msg}); they stay in {}",
                    item.group_key,
                    scratch.display()
                ));
            }
        }
    }
    rc.crop_progress("", crop_total, crop_total);
    if finalized > 0 {
        tracing::warn!(
            run_id = rc.run_id,
            count = finalized,
            "run did not finish; written outputs finalized uncropped"
        );
        rc.warnings.push(format!(
            "{finalized} group(s): outputs were written uncropped because the run did not finish"
        ));
    }
    remove_scratch_unless_kept(rc, &scratch, kept);
}

/// The file half of one group's crop: the master family, then the drizzle
/// family, each into the output folder. `rect` may be the full master: that
/// finalization writes exactly what the K = 0 path writes, the same writer
/// on the same pixels and cards (review I7). A drizzle family that cannot be
/// written never costs the master (ruling R-M3-7): the master family stays
/// final and the failure is returned in `drizzle_error`. The drizzle WCS is
/// `scale_plate_solve` of the master's record again (review M13): the first
/// write already scaled the same record, so it does not fail here. Names
/// come from the scratch files, so a name already taken in the output folder
/// resolves as `_2`, `_2_2` where the K = 0 path would say `_2`, `_3`.
fn write_crop_item(
    rc: &RunContext,
    item: &CropItem,
    rect: CropRect,
) -> anyhow::Result<CropOutputs> {
    #[cfg(test)]
    if rc.subject.project_id().is_some() && FORCE_CROP_FAILURE.swap(false, Ordering::SeqCst) {
        anyhow::bail!("forced crop failure");
    }
    let format = rc.config.output.format;
    let mut cards = item.cards.clone();
    if let Some(wcs) = &item.wcs {
        restate_crpix(&mut cards, &crop_plate_solve(wcs, rect.x0, rect.y0));
    }
    let _guard = OUTPUT_WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let master = write_cropped_master(&rc.output_dir, &item.master, rect, &cards, format)?;
    let mut drizzle = None;
    let mut drizzle_error = None;
    if let Some(d) = &item.drizzle {
        let drect = rect.scaled(d.scale);
        let attempt = (|| -> anyhow::Result<WrittenDrizzle> {
            let mut dcards = d.cards.clone();
            if let Some(wcs) = &item.wcs {
                let scaled = scale_plate_solve(wcs, d.scale)?;
                restate_crpix(&mut dcards, &crop_plate_solve(&scaled, drect.x0, drect.y0));
            }
            let stem = master
                .master
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("master");
            write_cropped_drizzle(
                &rc.output_dir,
                stem,
                d.scale,
                &d.written,
                drect,
                &dcards,
                format,
            )
        })();
        match attempt {
            Ok(w) => drizzle = Some((w, drect)),
            Err(e) => drizzle_error = Some(format!("{e:#}")),
        }
    }
    Ok(CropOutputs {
        rect,
        master,
        drizzle,
        drizzle_error,
    })
}

/// The record half: the group row's paths, one `master_lights` upsert per
/// final file (the key is `(run, group, kind)`, so each scratch row is
/// replaced), and the summary's paths.
fn record_crop_item(
    rc: &mut RunContext,
    item: &CropItem,
    out: &CropOutputs,
) -> Result<(), RunError> {
    #[cfg(test)]
    if rc.subject.project_id().is_some() && FORCE_RECORD_FAILURE.swap(false, Ordering::SeqCst) {
        return Err(RunError::Other("forced record failure".to_string()));
    }
    let show = |p: &Path| p.display().to_string();
    let format_str = enum_serde_name(&rc.config.output.format)?;
    let master_path = show(&out.master.master);
    let rejection_low = out.master.rejection_low.as_deref().map(show);
    let rejection_high = out.master.rejection_high.as_deref().map(show);
    let coverage = out.master.coverage.as_deref().map(show);
    let drizzle = out.drizzle.as_ref().map(|(d, r)| (show(&d.drizzle), *r));
    let weight_map = out
        .drizzle
        .as_ref()
        .and_then(|(d, _)| d.weight_map.as_deref().map(show));
    let drizzle_coverage = out
        .drizzle
        .as_ref()
        .and_then(|(d, _)| d.coverage.as_deref().map(show));
    {
        let conn = db(&rc.ctx)?.conn();
        // One IMMEDIATE transaction: the group row and every `master_lights`
        // row land together or not at all, so a failure never leaves the group
        // naming a final file the caller then removes. Dropping `tx` on an
        // early return rolls it back.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(anyhow::Error::from)?;
        update_group(
            &tx,
            item.group_id,
            &GroupUpdate {
                master_path: Some(&master_path),
                rejection_low_path: rejection_low.as_deref(),
                rejection_high_path: rejection_high.as_deref(),
                drizzle_path: drizzle.as_ref().map(|(p, _)| p.as_str()),
                ..Default::default()
            },
        )?;
        #[cfg(test)]
        if rc.subject.project_id().is_some()
            && FORCE_RECORD_FAILURE_AFTER_GROUP.swap(false, Ordering::SeqCst)
        {
            return Err(RunError::Other(
                "forced record failure after the group row".to_string(),
            ));
        }
        let row =
            |kind: MasterLightKind, path: &str, format: &str, rect: CropRect, channels: usize| {
                insert_master_light(
                    &tx,
                    &NewMasterLight {
                        subject: &rc.subject,
                        run_id: rc.run_id,
                        group_key: &item.group_key,
                        kind: kind.as_db_str(),
                        path,
                        format,
                        width: rect.width as i64,
                        height: rect.height as i64,
                        channels: channels as i64,
                        frames: item.frames as i64,
                        total_exposure_s: item.total_exposure_s,
                    },
                )
            };
        row(
            MasterLightKind::Master,
            &master_path,
            &format_str,
            out.rect,
            item.channels,
        )?;
        if let Some(path) = coverage.as_deref() {
            row(MasterLightKind::Coverage, path, "fits", out.rect, 1)?;
        }
        if let Some((path, drect)) = drizzle.as_ref() {
            row(
                MasterLightKind::Drizzle,
                path,
                &format_str,
                *drect,
                item.channels,
            )?;
            if let Some(path) = weight_map.as_deref() {
                row(
                    MasterLightKind::WeightMap,
                    path,
                    &format_str,
                    *drect,
                    item.channels,
                )?;
            }
            if let Some(path) = drizzle_coverage.as_deref() {
                row(
                    MasterLightKind::DrizzleCoverage,
                    path,
                    "fits",
                    *drect,
                    item.channels,
                )?;
            }
        }
        tx.commit().map_err(anyhow::Error::from)?;
    }
    if let Some(g) = rc
        .summary
        .groups
        .iter_mut()
        .find(|g| g.key == item.group_key)
    {
        g.master_path = Some(master_path.clone());
        g.rejection_low_path = rejection_low;
        g.rejection_high_path = rejection_high;
        g.coverage_path = coverage;
        if let Some((path, _)) = drizzle {
            g.drizzle_path = Some(path);
            g.weight_map_path = weight_map;
            g.drizzle_coverage_path = drizzle_coverage;
        }
    }
    let message = if out.rect.is_full(item.width, item.height) {
        "outputs finalized uncropped"
    } else {
        "outputs cropped"
    };
    tracing::info!(
        run_id = rc.run_id,
        group_key = %item.group_key,
        crop_x0 = out.rect.x0,
        crop_y0 = out.rect.y0,
        crop_width = out.rect.width,
        crop_height = out.rect.height,
        path = %master_path,
        "{}",
        message
    );
    Ok(())
}

#[cfg(test)]
#[path = "crop_tests.rs"]
mod tests;
