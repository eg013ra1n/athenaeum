use super::*;

/// What every step of one publish pass reads: the service context, the
/// project, the scope and what it derives, the run handle and the split hook.
#[derive(Clone, Copy)]
struct PublishPass<'a> {
    ctx: &'a ServiceContext,
    project_id: &'a str,
    scope: RunScope<'a>,
    force: bool,
    only: Option<&'a [i64]>,
    run: &'a crate::api::collab_publish_run::RunHandle,
    after_split: AfterSplit<'a>,
}

/// The hub link of one publish pass: credentials, client, node, and this
/// device's own account in the project.
struct PublishLink {
    token: String,
    client: CollabClient,
    node: Arc<crate::sharing::iroh::node::SharedIrohNode>,
    account_id: String,
    display: String,
}

/// What the selection step hands on: the live project, the publishing device,
/// the candidates, the held-back frames and the prepared rows to drop or consume.
struct Selection {
    project: CollabProjectRow,
    binding: Option<crate::db::collab::PublishingDevice>,
    held_back: Vec<HeldBackFrame>,
    candidates: Vec<PublishCandidate>,
    stale_ids: Vec<i64>,
    withheld_prepared: Vec<i64>,
    adopt_prepared: HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    from_prepared: HashMap<i64, crate::db::collab_prepare::PreparedRow>,
}

/// What the split hands on: the planned frames, the manifest meta of the
/// prepared ones, and the counters the split moved.
struct Split {
    plans: Vec<PlannedFrame>,
    prepared_cal: HashMap<i64, serde_json::Value>,
    unchanged: usize,
    refused_new: usize,
    stale: usize,
}

/// What section 7 records: the frames announced, adopted at the hub, bound
/// at identical bytes, versioned, and found already versioned.
struct ToRecord {
    announced: Vec<(SeededFrame, String, i32)>,
    hub_adopted: Vec<(SeededFrame, i32)>,
    bound: Vec<SeededFrame>,
    versioned: Vec<SeededFrame>,
    already_versioned: Vec<SeededFrame>,
}

/// What the seed-by-reference step hands on: the new frames seeded for the
/// announce, the updates staged for their versions, and the frames bound at
/// identical bytes.
struct Seeded {
    new_frames: Vec<SeededFrame>,
    updates: Vec<SeededFrame>,
    bound: Vec<SeededFrame>,
}

/// What the announce step hands on: the hub's state after the last accepted
/// batch, the frames announced, the frames adopted at the hub, and whether the
/// hub refused an outdated collab api.
struct Announced {
    state: Option<String>,
    announced: Vec<(SeededFrame, String, i32)>,
    hub_adopted: Vec<(SeededFrame, i32)>,
    outdated: bool,
}

/// What the versions step hands on: the frames the hub versioned, the frames
/// it already held at their bytes, and the real version conflicts.
struct Versions {
    versioned: Vec<SeededFrame>,
    already_versioned: Vec<SeededFrame>,
    conflicts: Vec<(String, i32)>,
}

pub(super) async fn run_publish(
    ctx: &ServiceContext,
    project_id: &str,
    _emitter: Option<Arc<dyn ProgressEmitter>>,
    scope: RunScope<'_>,
    run: &crate::api::collab_publish_run::RunHandle,
    after_split: AfterSplit<'_>,
) -> Result<PublishResult, ApiError> {
    let (force, only) = match scope {
        RunScope::Republish { only } => (true, only),
        RunScope::Publish { only, .. } | RunScope::Calibrate { only } => (false, only),
    };
    let pass = PublishPass {
        ctx,
        project_id,
        scope,
        force,
        only,
        run,
        after_split,
    };
    use crate::account::AccountClientError as E;

    // ── 1. Collaboration root, then the gate ─────────────────────────────────
    let collab_root = require_collaboration_root(ctx)?;
    let Selection {
        project,
        binding,
        mut held_back,
        candidates,
        stale_ids,
        withheld_prepared,
        adopt_prepared,
        from_prepared,
    } = publish_select(&pass)?;
    if candidates.is_empty() && stale_ids.is_empty() && withheld_prepared.is_empty() {
        tracing::info!(
            project_id,
            held_back = held_back.len(),
            "publish: nothing publishable"
        );
        return Ok(PublishResult {
            announced: 0,
            updated: 0,
            state: None,
            held_back,
            unchanged: 0,
            ..Default::default()
        });
    }

    // ── 2. Hub, node, and my own identity in the project ─────────────────────
    let PublishLink {
        token,
        client,
        node,
        account_id,
        display,
    } = publish_link(&pass, &project).await?;

    let bound_elsewhere = publish_bound_elsewhere(ctx, project_id, binding)?;

    let mut stale = publish_drop_prepared(
        ctx,
        project_id,
        &stale_ids,
        &withheld_prepared,
        &bound_elsewhere,
    )?;
    if candidates.is_empty() {
        tracing::info!(
            project_id,
            stale,
            held_back = held_back.len(),
            "publish: nothing publishable"
        );
        return Ok(PublishResult {
            stale,
            held_back,
            ..Default::default()
        });
    }

    // ── 3. The split (ruling R9: before any compute permit) ─────────────────
    // Hoisted out of the split's block (below) so the seed-by-reference
    // section (F5/C1/I1) can also tell an own-dir landing apart from an
    // attested original.
    let own_dir = {
        let db = db(ctx)?;
        let conn = db.conn();
        own_folder(&conn, &collab_root, &project, &account_id, &display).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: own folder failed");
            internal(e)
        })?
    };
    let Split {
        plans,
        mut prepared_cal,
        mut unchanged,
        refused_new,
        stale: split_stale,
    } = publish_split(
        &pass,
        &project,
        &account_id,
        &own_dir,
        &bound_elsewhere,
        candidates,
        &from_prepared,
        adopt_prepared,
        &mut held_back,
    )?;
    stale += split_stale;

    // ── 4. Generation: one compute permit, only when there is work ───────────
    // Review focus 3: a calibrate interrupted mid-write left a writer temp.
    // Swept only now, past every refusal of the split: a refused run touches
    // no disk. N40's space check (in the generation) comes after the sweep
    // on purpose — the leftovers it removes are space the check may count.
    if matches!(scope, RunScope::Calibrate { .. }) {
        crate::api::collab_prepare::sweep_writer_temps(&own_dir);
    }
    // Spec §4.3: a prepared New frame is never regenerated — its reviewed
    // file (or, attested, its original) is what is seeded and announced.
    let (from_review, plans): (Vec<PlannedFrame>, Vec<PlannedFrame>) =
        plans.into_iter().partition(|p| {
            matches!(p.kind, PublishKind::New) && from_prepared.contains_key(&p.cand.frame_id)
        });
    let mut outcome = publish_generate(&pass, &collab_root, &project, &own_dir, plans).await?;
    held_back.extend(outcome.held_back);
    unchanged += outcome.unchanged;

    // Calibrate stops here: its frames are prepared, waiting for review. A
    // cancel keeps the frames it wrote prepared (the tracker reports it).
    if matches!(scope, RunScope::Calibrate { .. }) {
        tracing::info!(
            project_id,
            count = outcome.written.len(),
            held_back = held_back.len(),
            "calibrate: frames prepared for review"
        );
        return Ok(PublishResult {
            calibrated: outcome.written.len(),
            unchanged,
            held_back,
            ..Default::default()
        });
    }

    publish_merge_reviewed(
        project_id,
        from_review,
        &from_prepared,
        &mut prepared_cal,
        &mut outcome.written,
    );

    // W4: a cancel before seeding starts ends the run here — also one that
    // landed after the last regenerated frame. Nothing is seeded or staged
    // yet: regenerated update temps are removed (the hub keeps its version,
    // the frame is stale again next time); prepared frames stay prepared.
    use crate::api::collab_publish_run::PublishStage;
    run.stage(PublishStage::Seeding, outcome.written.len());
    if outcome.cancelled || run.cancelled() {
        for w in &outcome.written {
            if w.staged != w.target {
                remove_temp(project_id, &w.staged);
            }
        }
        tracing::info!(
            project_id,
            outcome = "cancelled",
            "publish: cancelled before seeding"
        );
        return Ok(PublishResult {
            unchanged,
            held_back,
            stale,
            ..Default::default()
        });
    }

    // ── Seed by reference ────────────────────────────────────────────────────
    // An update or adoption replaces the landed file, so it runs under the
    // project's disk lock per frame (final review I2): the rename, the seed
    // and the local own-row write are one step disk truth never sees half
    // done. The row it writes describes the NEW file; the hub-confirmed
    // columns follow after `…/version` (`set_own_version`), or are put back
    // when the hub refuses it (`unstage_updates`).
    let disk_lock = crate::api::collab_exchange::project_disk_lock(ctx, project_id)?;
    let Seeded {
        new_frames,
        updates,
        bound,
    } = publish_seed(
        &pass,
        &node,
        &disk_lock,
        &own_dir,
        &from_prepared,
        outcome.written,
        &mut held_back,
        &mut stale,
        &mut unchanged,
    )
    .await?;

    // ── 5. Announce new frames, ≤ 500 per batch ──────────────────────────────
    run.stage(PublishStage::Announcing, new_frames.len());
    // Final fix D-18: every frame this run announces or versions is "in
    // flight" until step 7 recorded its implicit claim (the guard lives to
    // the end of the run) — a full holder report never ends its claim
    // explicitly meanwhile, although the hub may hold it before the local
    // set does.
    let _claim_calls = crate::api::collab_live::holdings::ClaimCalls::begin(
        ctx,
        project_id,
        new_frames
            .iter()
            .chain(updates.iter())
            .map(|f| f.written.uuid.clone()),
    );
    let mut first_err: Option<ApiError> = (refused_new > 0).then(|| {
        ApiError::Conflict(crate::account::client::publishing_device_msg(
            bound_elsewhere.clone().flatten().as_deref(),
        ))
    });
    let Announced {
        state,
        announced,
        hub_adopted,
        mut outdated,
    } = publish_announce(
        &pass,
        &token,
        &client,
        &node,
        &disk_lock,
        project.thresholds_version.unwrap_or(0),
        new_frames,
        &updates,
        &from_prepared,
        &mut first_err,
        &mut held_back,
    )
    .await?;

    // ── 6. New content versions ──────────────────────────────────────────────
    run.stage(PublishStage::Versions, updates.len());
    let Versions {
        versioned,
        already_versioned,
        conflicts,
    } = publish_versions(
        &pass,
        &token,
        &client,
        &node,
        &disk_lock,
        updates,
        &mut outdated,
        &mut first_err,
        &mut held_back,
    )
    .await?;

    // ── 7. Own rows and their claims, per frame (never aborts the run) ───────
    let (announced_n, updated_n) = publish_record_own_rows(
        &pass,
        &account_id,
        &display,
        ToRecord {
            announced,
            hub_adopted,
            bound,
            versioned,
            already_versioned,
        },
        &mut held_back,
        &mut unchanged,
    )?;
    publish_after_conflicts(ctx, project_id, &conflicts).await?;

    // ── 8. Log (the run tracker emits the one finished event) ─────────────────
    tracing::info!(
        project_id,
        count = announced_n,
        updated = updated_n,
        held_back = held_back.len(),
        unchanged,
        "frames published"
    );
    if outdated {
        return Err(client_err(E::CollabApiOutdated));
    }
    if announced_n == 0 && updated_n == 0 {
        if let Some(err) = first_err {
            return Err(err);
        }
    }
    Ok(PublishResult {
        announced: announced_n,
        updated: updated_n,
        state,
        held_back,
        unchanged,
        stale,
        ..Default::default()
    })
}

/// The prepared New frames join the generated ones as written.
fn publish_merge_reviewed(
    project_id: &str,
    from_review: Vec<PlannedFrame>,
    from_prepared: &HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    prepared_cal: &mut HashMap<i64, serde_json::Value>,
    written: &mut Vec<WrittenFrame>,
) {
    // The prepared New frames join the generated ones, as written.
    for plan in from_review {
        let Some(p) = from_prepared.get(&plan.cand.frame_id) else {
            continue; // never: the partition keyed on `from_prepared`
        };
        let PlannedFrame {
            cand,
            kind,
            target,
            mut meta,
            external,
            ..
        } = plan;
        if let Some(m) = meta.as_mut() {
            m.meta["calibration"] = if external {
                calibration_meta(None, true)
            } else {
                prepared_cal.remove(&cand.frame_id).unwrap_or_else(|| {
                    tracing::warn!(project_id, frame_id = cand.frame_id, "publish: no calibration links read for a prepared frame; its meta names no masters");
                    calibration_meta(None, false)
                })
            };
        }
        written.push(WrittenFrame {
            frame_id: cand.frame_id,
            filename: cand.filename,
            uuid: cand.uuid,
            filter_canonical: cand.filter_canonical,
            kind,
            staged: target.clone(),
            target,
            recipe: p.recipe_hash.clone(),
            xxh3: p.xxh3.clone(),
            byte_size: u64::try_from(p.byte_size).unwrap_or_default(),
            meta,
            identical: false,
            staged_blake3: None,
        });
    }
}

/// After the version batches: a version conflict means the hub moved on under
/// this run, so pull the manifest and ask for another run once it caught up.
async fn publish_after_conflicts(
    ctx: &ServiceContext,
    project_id: &str,
    conflicts: &[(String, i32)],
) -> Result<(), ApiError> {
    use crate::db::collab_frames as frames_db;
    if !conflicts.is_empty() {
        // The hub moved on under this run (another device of this account
        // versioned the frame). Asking for another run right away would
        // regenerate and conflict again until the manifest catches up, so
        // pull the manifest delta first, and ask only once the local rows
        // actually carry the hub's newer versions.
        match crate::api::collab_exchange::sync_manifest(ctx, project_id, None, None).await {
            Ok(_) => {
                let caught_up = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    conflicts.iter().all(|(u, n)| {
                        frames_db::get(&conn, project_id, u)
                            .ok()
                            .flatten()
                            .is_some_and(|r| r.content_version >= *n)
                    })
                };
                if caught_up {
                    crate::api::collab_autopublish::request_auto_publish(Some(project_id));
                } else {
                    tracing::info!(project_id, count = conflicts.len(), "publish: the manifest does not show the hub's newer versions yet; no new run requested");
                }
            }
            Err(e) => {
                tracing::warn!(project_id, error = %e, "publish: manifest sync after a version conflict failed; no new run requested");
            }
        }
    }
    Ok(())
}

/// A seeded Update or Adopt frame whose file now sits elsewhere: move the
/// row's `landed_path` to it, and remove the superseded generated file when
/// that is safe.
fn publish_move_landed_path(
    ctx: &ServiceContext,
    project_id: &str,
    own_dir: &Path,
    w: &WrittenFrame,
    current: &crate::db::collab_frames::LocalFrameRow,
) {
    use crate::db::collab_frames as frames_db;
    // C1/I1: the landing crossed between generated and attested
    // (or its folder otherwise changed) — move `landed_path` to
    // where this run actually put the frame's CURRENT bytes,
    // before any downstream write depends on it agreeing
    // (`stage_own_file`'s `WHERE landed_path = ?` needs the row
    // to already match). The stale file is removed ONLY when
    // the move itself actually succeeded AND it was this
    // publisher's own-dir landing — an app-generated artifact,
    // never an attested original (F5/A1: that file is never
    // removed, whatever the set's attestation is now).
    //
    // Fix round 2 (finding 2): `landed_path` is `TEXT UNIQUE`
    // table-wide, and a frame set can be linked to more than one
    // project — a move CAN fail with a real collision (another
    // project's own row already holds this exact path). A
    // failed move must never delete the row's OWN still-current
    // file: that would leave the row pointing at nothing.
    let new_landed = w.target.to_string_lossy().to_string();
    if current.landed_path.as_deref() != Some(new_landed.as_str()) {
        let moved = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
            frames_db::update_landed_path(&db.conn(), project_id, &w.uuid, &new_landed)
        });
        let moved_ok = match moved {
            Ok(n) if n > 0 => {
                tracing::info!(project_id, frame_uuid = %w.uuid, from = current.landed_path.as_deref().unwrap_or(""), to = %new_landed, "publish: own frame's landed path moved");
                true
            }
            Ok(_) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, "publish: moving the own frame's landed path matched no row");
                false
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, error = %format!("{e:#}"), "publish: moving the own frame's landed path failed");
                false
            }
        };
        if moved_ok {
            if let Some(old) = current.landed_path.as_deref() {
                let old_path = Path::new(old);
                if is_own_dir_landing(old_path, &own_dir) {
                    // C1 fix round 3, part 1: `is_own_dir_landing`
                    // only tells us the path SITS in our own
                    // folder shape — not that WE put it there. A
                    // user can move (or attest) their own light
                    // straight into that folder; deleting it
                    // would be real data loss. Two independent
                    // guards, belt-and-braces with the
                    // `publisher_dir` fix above:
                    //   - a `files` row still referencing this
                    //     exact path means it is catalogued —
                    //     generated files are never catalogued,
                    //     so this check is exact, no false
                    //     positive possible.
                    //   - `was_external_recipe(&current)`: the
                    //     row we are moving OFF of was itself an
                    //     attested (external) landing.
                    let catalogued = {
                        let conn = match db(ctx) {
                            Ok(db) => Some(db.conn()),
                            Err(e) => {
                                tracing::error!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %e, "publish: checking whether the superseded path is catalogued failed; not removing it");
                                None
                            }
                        };
                        match conn {
                            Some(conn) => {
                                crate::db::file_exists(&conn, old)
                                    .unwrap_or_else(|e| {
                                        tracing::error!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %format!("{e:#}"), "publish: checking whether the superseded path is catalogued failed; not removing it");
                                        true
                                    })
                            }
                            None => true,
                        }
                    };
                    if catalogued {
                        tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: skipped removing the superseded path — a files row still references it");
                    } else if was_external_recipe(&current) {
                        tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: skipped removing the superseded path — its recipe was external");
                    } else {
                        match std::fs::remove_file(old_path) {
                            Ok(()) => {
                                tracing::info!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), "publish: removed the superseded generated file")
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => {
                                tracing::warn!(project_id, frame_uuid = %w.uuid, path = %old_path.display(), error = %e, "publish: removing the superseded generated file failed")
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Generation: one compute permit, only when there is work. The empty branch
/// never touches the compute queue.
async fn publish_generate(
    pass: &PublishPass<'_>,
    collab_root: &Path,
    project: &CollabProjectRow,
    own_dir: &Path,
    plans: Vec<PlannedFrame>,
) -> Result<GenerationOutcome, ApiError> {
    let PublishPass {
        ctx,
        project_id,
        scope,
        run,
        ..
    } = *pass;
    let outcome = if plans.is_empty() {
        GenerationOutcome {
            written: Vec::new(),
            unchanged: 0,
            held_back: Vec::new(),
            cancelled: false,
        }
    } else {
        // The generation reports its own stages (queued for the compute
        // slot, then calibrating) and one tick per frame.
        let job = GenerationJob {
            db: db(ctx)?.clone(),
            queue: ctx.compute_queue.clone(),
            pool: Arc::clone(&ctx.image_pool),
            project_id: project_id.to_string(),
            collab_root: collab_root.to_path_buf(),
            label: format!("collab publish {}", project.title),
            plans,
            cancel: run.cancel_flag(),
            run: run.clone(),
            prepare: matches!(scope, RunScope::Calibrate { .. }).then(|| PrepareSink {
                project_id: project_id.to_string(),
                own_dir: own_dir.to_string_lossy().into_owned(),
                publish_run_id: run.id(),
            }),
        };
        tokio::task::spawn_blocking(move || run_publish_generation(job))
            .await
            .map_err(|e| {
                tracing::error!(project_id, error = %e, "publish: generation task failed");
                ApiError::Internal(format!("publish generation task: {e}"))
            })??
    };
    Ok(outcome)
}

/// The hub, the node and this device's own identity in the project. Refusals
/// come in a fixed order: signed out, client, node, store, dirs, key.
async fn publish_link(
    pass: &PublishPass<'_>,
    project: &CollabProjectRow,
) -> Result<PublishLink, ApiError> {
    let PublishPass {
        ctx, project_id, ..
    } = *pass;
    let Some((hub_url, token)) = crate::api::account::hub_credentials(ctx)? else {
        tracing::warn!(project_id, "publish refused: signed out");
        return Err(ApiError::SignedOut(
            "Sign in to publish to a project.".into(),
        ));
    };
    let client = CollabClient::new(&hub_url).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: hub client failed");
        client_err(e)
    })?;
    let node = crate::api::sync::ensure_iroh_node(ctx).await.map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: iroh node unavailable");
        e
    })?;
    // I3: nothing can be seeded without the collab store — refuse before any
    // calibration, never regenerate every frame only to hold it back.
    if crate::api::collab_exchange::ensure_collab_store(ctx)
        .await
        .is_none()
    {
        tracing::warn!(
            project_id,
            outcome = "collab_store_unmounted",
            "publish refused: the collab store is not mounted"
        );
        return Err(ApiError::Conflict(COLLAB_STORE_UNMOUNTED.into()));
    }
    let dirs = crate::api::sync::sync_dirs(ctx).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: sync dirs unavailable");
        e
    })?;
    let own_node = DeviceKey::load_or_create(&device_key_path(&dirs.identity_dir))
        .map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: device key unavailable");
            ApiError::Internal(format!("device key: {e:#}"))
        })?
        .node_id();
    let (account_id, display) = {
        let members: Vec<SnapshotMember> = serde_json::from_str(&project.members_json)
            .unwrap_or_else(|e| {
                tracing::warn!(project_id, error = %e, "publish: members_json does not parse");
                Vec::new()
            });
        match members
            .iter()
            .find(|m| member_node_ids(m).iter().any(|n| n == &own_node))
        {
            Some(m) => (m.account_id.clone(), m.display_name.clone()),
            None => {
                tracing::warn!(
                    project_id,
                    "publish: this device is not in the project snapshot; publishing under \"own\""
                );
                (String::new(), String::new())
            }
        }
    };
    Ok(PublishLink {
        token,
        client,
        node,
        account_id,
        display,
    })
}

/// A6: the name of the other device the project is bound to, when there is
/// one. The own device is read before the replaced devices.
fn publish_bound_elsewhere(
    ctx: &ServiceContext,
    project_id: &str,
    binding: Option<crate::db::collab::PublishingDevice>,
) -> Result<Option<Option<String>>, ApiError> {
    // A6: one publishing device per (project, account). When the cached
    // binding names another device (in service — a revoked one reads as
    // unbound), no new frame is generated or announced here: the hub would
    // refuse it. Versions of this device's own frames still go out.
    let me = crate::api::account::own_device_id(ctx).map_err(|e| {
        tracing::error!(project_id, error = %e, "publish: this device's key is unavailable");
        e
    })?;
    // Fix round 2 (M3): a binding naming a device THIS device replaced is
    // not "elsewhere" (that device is retired; the hub reads it unbound).
    let replaced = {
        let db = db(ctx)?;
        let conn = db.conn();
        crate::db::collab_live::replaced_devices(&conn).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the replaced devices failed");
            internal(e)
        })?
    };
    let bound_elsewhere: Option<Option<String>> = binding
        .filter(|b| b.device_id != me && !replaced.contains_key(&b.device_id))
        .map(|b| b.name);
    Ok(bound_elsewhere)
}

/// Spec §4.1 step 5 and plan W3 / P4: the stale prepared rows go back to
/// Ready, then the withheld frames' prepared rows are dropped; both only when
/// no other device holds the binding. Returns how many frames went stale.
fn publish_drop_prepared(
    ctx: &ServiceContext,
    project_id: &str,
    stale_ids: &[i64],
    withheld_prepared: &[i64],
    bound_elsewhere: &Option<Option<String>>,
) -> Result<usize, ApiError> {
    // Spec §4.1 step 5: stale prepared frames go back to Ready — only now,
    // past the account, node and store checks, so a refused run never
    // touches disk. Under another device's binding no new frame moves on
    // this device (the run is refused, or posts versions only): the rows
    // wait for a run that can act on them. Calibrate counts its stale frames
    // as calibrated again, never as stale.
    let stale = if stale_ids.is_empty() {
        0
    } else if bound_elsewhere.is_some() {
        tracing::debug!(
            project_id,
            count = stale_ids.len(),
            "publish: stale prepared frames kept while another device publishes new frames"
        );
        0
    } else {
        let db = db(ctx)?;
        let conn = db.conn();
        drop_stale_prepared(&conn, project_id, &stale_ids)?
    };
    // Plan W3 / P4: a frame withheld before the run loses its prepared row
    // and file (never an attested original) — under the same condition.
    if !withheld_prepared.is_empty() && bound_elsewhere.is_none() {
        let db = db(ctx)?;
        let conn = db.conn();
        let gone = crate::db::collab_prepare::delete_prepared(&conn, project_id, &withheld_prepared)
            .map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "withheld frames' prepared rows not dropped");
                internal(e)
            })?;
        crate::api::collab_prepare::remove_prepared_files(&gone);
        tracing::info!(
            project_id,
            count = gone.len(),
            "withheld frames' prepared rows dropped"
        );
    }
    Ok(stale)
}

/// Section 1: the live project and its gate, the selection, then the candidate
/// walk that sorts every gated frame into a candidate, a held-back frame or a
/// prepared row to drop.
fn publish_select(pass: &PublishPass<'_>) -> Result<Selection, ApiError> {
    let PublishPass {
        ctx,
        project_id,
        scope,
        only,
        ..
    } = *pass;
    use crate::db::collab_frames as frames_db;
    let (project, gated, binding) = {
        let db = db(ctx)?;
        let conn = db.conn();
        let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
        let gated = project_gate(&conn, &project)?;
        let binding =
            crate::db::collab::publishing_device(&conn, project_id).map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the publishing device failed");
                internal(e)
            })?;
        (project, gated, binding)
    };
    // Wave 2 (plan 2026-09-30 Task 1): a selection restricts the run to the
    // chosen gate rows; the rest are neither candidates nor held back.
    let gated: Vec<_> = match only {
        None => gated,
        Some(ids) => {
            let keep: HashSet<i64> = ids.iter().copied().collect();
            let kept: Vec<_> = gated
                .into_iter()
                .filter(|(_, row)| keep.contains(&row.frame_id))
                .collect();
            tracing::info!(
                project_id,
                count = kept.len(),
                "publish: restricted to a selection"
            );
            kept
        }
    };
    let mut held_back: Vec<HeldBackFrame> = Vec::new();
    let mut candidates: Vec<PublishCandidate> = Vec::new();
    // Prepared frames whose recipe or file moved. Dropped only after the
    // account, node and store checks (section 2): a refused run never
    // touches disk.
    let mut stale_ids: Vec<i64> = Vec::new();
    // Plan W3: prepared rows of frames withheld before this Publish run —
    // dropped with the stale ones (never an attested original), not counted
    // as stale.
    let mut withheld_prepared: Vec<i64> = Vec::new();
    // P6: the prepared row of an adopt-class frame. The frame is published
    // (adopted), so the row is consumed — and its file may be the very
    // landing the adoption writes to.
    let mut adopt_prepared: HashMap<i64, crate::db::collab_prepare::PreparedRow> = HashMap::new();
    // Publish scope (spec §4.3): the prepared row behind each New candidate
    // — its reviewed file is what is seeded and announced, never regenerated.
    let mut from_prepared: HashMap<i64, crate::db::collab_prepare::PreparedRow> = HashMap::new();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let ids: Vec<i64> = gated.iter().map(|(_, row)| row.frame_id).collect();
        // The section 4.1 local facts.
        let mut local = ProjectLocal::load(&conn, project_id, &ids)?;
        let own_by_src = frames_db::own_by_source_frame(&conn, project_id).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: read own frames failed");
            internal(e)
        })?;
        for (id, row) in gated {
            if !row.publishable {
                // Plan W1: a gate failure is the run's business only when
                // the member selected the frame.
                if only.is_some() {
                    held_back.push(HeldBackFrame {
                        frame_id: row.frame_id,
                        filename: row.filename,
                        reasons: row.failures,
                        publishing_device: None,
                    });
                }
                continue;
            }
            let fid = row.frame_id;
            match scope {
                RunScope::Calibrate { .. } => {
                    // Spec §4.3: only Ready frames are calibrated — never one
                    // already published, withheld, black-holed or prepared
                    // and still current.
                    if own_by_src.contains_key(&fid)
                        || local.withheld.contains(&fid)
                        || local.black_holed.contains(&fid)
                    {
                        continue;
                    }
                    if let Some(p) = local.prepared.get(&fid) {
                        if prepared_is_current(&conn, p, id.attested) {
                            continue;
                        }
                        stale_ids.push(fid);
                    }
                }
                RunScope::Publish { .. } | RunScope::Republish { .. }
                    if !own_by_src.contains_key(&fid) =>
                {
                    // P6: a frame the hub already lists as mine with no
                    // local binding (a replaced device's) is adopted as an
                    // update, never reviewed — whatever its prepared state.
                    match is_adopt_class(&conn, project_id, &id.uuid) {
                        Ok(true) => {
                            if let Some(p) = local.prepared.remove(&fid) {
                                adopt_prepared.insert(fid, p);
                            }
                        }
                        Ok(false) => {
                            let RunScope::Publish { include_new, .. } = scope else {
                                // Ruling R5 (spec P2, P4): Republish never
                                // produces a New frame — a frame with no own
                                // row reaches the hub only through Calibrate
                                // → review → Publish, and never withheld.
                                if only.is_some() {
                                    held_back.push(held(
                                        fid,
                                        &row.filename,
                                        "not published yet — calibrate and publish it first".into(),
                                    ));
                                }
                                continue;
                            };
                            // Spec §4.3: a New frame is published only from
                            // its prepared (calibrated and reviewed) file.
                            if local.withheld.contains(&fid) {
                                // P4 / plan W3: never announced; its
                                // prepared row and file go.
                                if local.prepared.contains_key(&fid) {
                                    withheld_prepared.push(fid);
                                }
                                // Plan W1: a selected frame is reported.
                                if only.is_some() {
                                    held_back.push(held(
                                        fid,
                                        &row.filename,
                                        crate::collab::contributor_state::WITHHELD_REASON.into(),
                                    ));
                                }
                                continue;
                            }
                            if local.black_holed.contains(&fid) {
                                // Spec §4.4: skipped; a prepared row stays
                                // (a restore brings the frame back to Review).
                                continue;
                            }
                            match local.prepared.remove(&fid) {
                                Some(p) if prepared_is_current(&conn, &p, id.attested) => {
                                    if !include_new {
                                        continue;
                                    }
                                    from_prepared.insert(fid, p);
                                }
                                Some(_) => {
                                    stale_ids.push(fid);
                                    continue;
                                }
                                None => {
                                    if only.is_some() {
                                        held_back.push(held(
                                            fid,
                                            &row.filename,
                                            "not calibrated yet — calibrate it first".into(),
                                        ));
                                    }
                                    continue;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: read frame row failed");
                            held_back.push(held(
                                fid,
                                &row.filename,
                                format!("cannot read the frame's project row: {e:#}"),
                            ));
                            continue;
                        }
                    }
                }
                _ => {}
            }
            candidates.push(PublishCandidate {
                frame_id: fid,
                filename: row.filename,
                uuid: id.uuid,
                // `publishable` implies a dictionary match (P3).
                filter_canonical: id.filter_canonical.unwrap_or_default(),
                attested: id.attested,
            });
        }
    }
    Ok(Selection {
        project,
        binding,
        held_back,
        candidates,
        stale_ids,
        withheld_prepared,
        adopt_prepared,
        from_prepared,
    })
}

/// Section 3, the split (ruling R9: before any compute permit): what each
/// candidate is, where every update, adoption and new frame lands, and the
/// prepared rows the split consumes or sends back to Ready.
#[allow(clippy::too_many_arguments)]
fn publish_split(
    pass: &PublishPass<'_>,
    project: &CollabProjectRow,
    account_id: &str,
    own_dir: &Path,
    bound_elsewhere: &Option<Option<String>>,
    candidates: Vec<PublishCandidate>,
    from_prepared: &HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    adopt_prepared: HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    held_back: &mut Vec<HeldBackFrame>,
) -> Result<Split, ApiError> {
    let PublishPass {
        ctx,
        project_id,
        scope,
        force,
        after_split,
        ..
    } = *pass;
    use crate::db::collab_frames::{self as frames_db, FrameOrigin};
    let mut refused_new = 0usize;
    let mut stale = 0usize;
    let opts = publish_options();
    let mut unchanged = 0usize;
    let mut plans: Vec<PlannedFrame> = Vec::new();
    // F8 manifest meta of each prepared generated frame, from its links.
    let mut prepared_cal: HashMap<i64, serde_json::Value> = HashMap::new();
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let own_devices = crate::api::collab_exchange::own_devices(ctx, &conn, &project)?;
        let own = frames_db::own_by_source_frame(&conn, project_id).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: read own frames failed");
            internal(e)
        })?;
        // Every landing this run reserves: adoption and update targets
        // (pass 2; a Calibrate's adoptions already in pass 1), then the New
        // frames' names (pass 3).
        let mut claimed: HashSet<std::path::PathBuf> = HashSet::new();
        // Pass 1: what each candidate is (new / update / adopt). F5: an
        // attested candidate is branched here, BEFORE any calibration
        // resolution — an attested light is never resolved or calibrated,
        // only hashed and stat'd in place (the generation phase).
        let mut split: Vec<(
            PublishCandidate,
            PublishKind,
            bool,
            Option<(std::path::PathBuf, String)>,
        )> = Vec::new();
        for cand in candidates {
            let fid = cand.frame_id;
            let (recipe, osc, external): (String, bool, Option<(std::path::PathBuf, String)>) =
                if cand.attested {
                    // An attested light inside a zip needs no hold here: its recipe is the catalog's size and
                    // mtime, so an already-published one is `Unchanged`. The hold sits below, on the frames that
                    // would actually be sent.
                    let row: rusqlite::Result<(String, i64, String)> = conn.query_row(
                        "SELECT fi.path, fi.size, fi.modified_at FROM frames f \
                         JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
                        [fid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    );
                    match row {
                        Ok((path, size, modified)) => {
                            // M1: `osc` (`opts.debayer_osc && osc`) only ever
                            // feeds `calibrated_output_filename` — dead on
                            // this branch, which never calls it (an
                            // attested light's name is its own basename).
                            // Never queried, so there is nothing to swallow.
                            let recipe = external_recipe(size, &modified);
                            (
                                recipe.clone(),
                                false,
                                Some((std::path::PathBuf::from(path), recipe)),
                            )
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %e, "publish: reading the attested light failed");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!("cannot read the attested light: {e}"),
                            ));
                            continue;
                        }
                    }
                } else {
                    let resolved =
                        match crate::calibration_library::light_resolve::resolve_frame_inputs(
                            &conn,
                            fid,
                            opts.flat_norm,
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: cannot resolve this light");
                                // A light or master inside a zip is the better reason when one is the cause.
                                let reason = zip_inputs_hold(&conn, fid, false)
                                    .unwrap_or_else(|| format!("cannot calibrate: {e:#}"));
                                held_back.push(held(fid, &cand.filename, reason));
                                continue;
                            }
                        };
                    if from_prepared.contains_key(&fid) {
                        // F8: what the calibrate that wrote the prepared file
                        // used — its recipe (the masters) is current.
                        prepared_cal.insert(fid, calibration_meta_of_inputs(&resolved));
                    }
                    let recipe = match recipe_hash_of_inputs(&conn, &resolved) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: recipe hash failed");
                            let reason = zip_inputs_hold(&conn, fid, false).unwrap_or_else(|| {
                                format!("cannot read the calibration inputs: {e:#}")
                            });
                            held_back.push(held(fid, &cand.filename, reason));
                            continue;
                        }
                    };
                    (recipe, resolved.cfa_geometry.is_some(), None)
                };
            let kind = match own.get(&fid) {
                Some(row) if force || row.recipe_hash.as_deref() != Some(recipe.as_str()) => {
                    PublishKind::Update(row.clone())
                }
                Some(_) => {
                    unchanged += 1;
                    continue;
                }
                None => match frames_db::get(&conn, project_id, &cand.uuid) {
                    Ok(Some(row))
                        if row.origin == FrameOrigin::Own && row.source_frame_id.is_none() =>
                    {
                        PublishKind::Adopt(row)
                    }
                    Ok(_) => PublishKind::New,
                    Err(e) => {
                        tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: read frame row failed");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!("cannot read the frame's project row: {e:#}"),
                        ));
                        continue;
                    }
                },
            };
            match (scope, &kind) {
                // Adopt-class frames are updates, never reviewed (P6):
                // Publish's, and never one of Calibrate's hold-backs.
                (RunScope::Calibrate { .. }, PublishKind::Adopt(row)) => {
                    // Its landing stays reserved: a calibrate never picks
                    // the name the adoption will write to (an attested
                    // adoption lands on its original, never a picked name).
                    if external.is_none() {
                        if let Some(t) = adopt_landing(row, &own_dir) {
                            claimed.insert(t);
                        }
                    }
                    tracing::debug!(
                        project_id,
                        frame_id = fid,
                        "calibrate: adopt-class frame left to Publish"
                    );
                    continue;
                }
                // Spec §4.3: a New frame reaches the hub only from its
                // prepared file (Republish never takes one: ruling R5). The
                // candidate read saw an own row or an adoption; this re-read
                // does not (a manifest sync landed in between) — the frame
                // waits for a calibrate.
                (RunScope::Publish { .. } | RunScope::Republish { .. }, PublishKind::New)
                    if !from_prepared.contains_key(&fid) =>
                {
                    tracing::warn!(
                        project_id,
                        frame_id = fid,
                        "publish: a new frame with no prepared file is not announced"
                    );
                    continue;
                }
                _ => {}
            }
            match &kind {
                PublishKind::New => {
                    if let Some(name) = &bound_elsewhere {
                        held_back.push(held_bound_elsewhere(fid, &cand.filename, name.as_deref()));
                        refused_new += 1;
                        continue;
                    }
                }
                PublishKind::Update(row) | PublishKind::Adopt(row) => {
                    if published_by_another_device(project_id, row, &own_devices) {
                        tracing::debug!(project_id, frame_uuid = %row.frame_uuid, "publish: frame published by another device of this account; its versions are that device's");
                        continue;
                    }
                    if !force
                        && row.last_error.as_deref()
                            == Some(not_publishing_mark(row.manifest_version).as_str())
                    {
                        tracing::debug!(project_id, frame_uuid = %row.frame_uuid, manifest_version = row.manifest_version, "publish: version refused at this manifest version; not retried until the manifest changes");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            "another device of this account versions this frame now".into(),
                        ));
                        continue;
                    }
                }
            }
            // A frame that would be sent from inside a zip waits for an unpack (spec 7.9: v1 does not send
            // from a zip). Only here, on New / Update / Adopt: an `Unchanged` one sends nothing and was
            // skipped above, so a published light zipped since is not held back on every run.
            if let Some(reason) = zip_inputs_hold(&conn, fid, external.is_some()) {
                held_back.push(held(fid, &cand.filename, reason));
                continue;
            }
            split.push((cand, kind, osc, external));
        }
        let (mut taken_names, mut name_taken, targeted) = publish_split_targets(
            &conn,
            project_id,
            account_id,
            own_dir,
            split,
            &mut claimed,
            held_back,
        )?;
        // Pass 3: new frames — their manifest fields checked against the
        // hub's per-frame rules (M1: one bad frame refuses a whole atomic
        // batch), then a free landing name. F5: an external candidate never
        // picks a landing name — its target IS the frame's current catalog
        // path, and a basename collision within this publisher is held back,
        // never renamed on disk. A prepared frame lands where its calibrate
        // wrote it (spec §4.3) — its name was picked then.
        for (cand, kind, osc, target, external) in targeted {
            let fid = cand.frame_id;
            if let (PublishKind::New, Some(p)) = (&kind, from_prepared.get(&fid)) {
                if !p.external {
                    let Some(path) = p.calibrated_path.as_deref() else {
                        tracing::error!(
                            project_id,
                            frame_id = fid,
                            "publish: a generated prepared frame has no file path"
                        );
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            "internal error: the calibrated frame has no file path".into(),
                        ));
                        continue;
                    };
                    let target = std::path::PathBuf::from(path);
                    let file_name = target
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    // A6: the name was free at calibrate time; a manifest
                    // sync since may have brought a frame of this publisher
                    // under it. Announcing it would give one publisher two
                    // frames of one name: the frame is stale, and the next
                    // calibrate picks a free name.
                    if taken_names.contains(&file_name) || claimed.contains(&target) {
                        tracing::info!(project_id, frame_id = fid, file_name = %file_name, "publish: prepared frame's name is taken since its calibrate; back to ready");
                        name_taken.push(fid);
                        continue;
                    }
                    let meta = match new_frame_meta_or_hold_back(&conn, project_id, fid) {
                        Ok(m) => m,
                        Err(reason) => {
                            held_back.push(held(fid, &cand.filename, reason));
                            continue;
                        }
                    };
                    claimed.insert(target.clone());
                    taken_names.insert(file_name);
                    plans.push(PlannedFrame {
                        cand,
                        kind,
                        target,
                        meta: Some(meta),
                        external: false,
                        recipe: Some(p.recipe_hash.clone()),
                    });
                    continue;
                }
            }
            if let Some((path, recipe)) = external {
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let meta = if matches!(kind, PublishKind::New) {
                    // Controller ruling (fix round 2, finding 3):
                    // `landed_path` is `TEXT UNIQUE` table-wide, and a frame
                    // set is linked, not owned — the SAME set can be linked
                    // to more than one project. A NEW attested frame whose
                    // original already backs ANOTHER project's own row
                    // would hit that constraint only after the hub
                    // announce succeeded, orphaning it there. Caught here,
                    // before anything is planned; a read failure holds back
                    // rather than risk the same orphan (fail-closed).
                    match crate::db::collab_frames::find_by_landed_path(
                        &conn,
                        &path.to_string_lossy(),
                    ) {
                        Ok(Some(existing)) if existing.project_id != project_id => {
                            let title = crate::db::collab::get_project(&conn, &existing.project_id)
                                .ok()
                                .flatten()
                                .map(|p| p.title)
                                .unwrap_or_else(|| existing.project_id.clone());
                            tracing::warn!(project_id, frame_id = fid, other_project = %existing.project_id, path = %path.display(), "publish: attested light's original already backs another project's frame");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!(
                                    "this file already backs a frame in project \"{title}\" — a file can back one project frame"
                                ),
                            ));
                            continue;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: checking the attested light's landed path failed");
                            held_back.push(held(
                                fid,
                                &cand.filename,
                                format!("cannot verify this file is not already published elsewhere: {e:#}"),
                            ));
                            continue;
                        }
                    }
                    // Dedup (F5): a NEW frame only — an Update or Adopt
                    // reuses the SAME name it already legitimately holds
                    // (it is its own file, unchanged), which `taken_names`
                    // already carries from its own prior publish; checking
                    // it here would refuse a frame against itself.
                    if taken_names.contains(&file_name) || !claimed.insert(path.clone()) {
                        tracing::warn!(project_id, frame_id = fid, file_name = %file_name, "publish: attested basename already published by this publisher");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!(
                                "a frame named {file_name:?} is already published by you — rename the file"
                            ),
                        ));
                        continue;
                    }
                    // I2: the hub's per-file-name rule — an original's
                    // basename is not chosen by the app the way a generated
                    // `c_<stem>.fits` is, and can break it (`:`, surrounding
                    // whitespace, empty).
                    if let Some(problem) = hub_file_name_problem(&file_name) {
                        tracing::warn!(project_id, frame_id = fid, path = %path.display(), reason = problem, "publish: attested file name breaks the hub rule");
                        held_back.push(held(
                            fid,
                            &cand.filename,
                            format!("the hub refuses this file name ({problem}): {file_name}"),
                        ));
                        continue;
                    }
                    taken_names.insert(file_name);
                    match new_frame_meta_or_hold_back(&conn, project_id, fid) {
                        Ok(m) => Some(m),
                        Err(reason) => {
                            held_back.push(held(fid, &cand.filename, reason));
                            continue;
                        }
                    }
                } else {
                    claimed.insert(path.clone());
                    None
                };
                plans.push(PlannedFrame {
                    cand,
                    kind,
                    target: path,
                    meta,
                    external: true,
                    recipe: Some(recipe),
                });
                continue;
            }
            if let Some(target) = target {
                plans.push(PlannedFrame {
                    cand,
                    kind,
                    target,
                    meta: None,
                    external: false,
                    recipe: None,
                });
                continue;
            }
            // C1/I1: this fallback now also serves a generated Update/Adopt
            // whose PREVIOUS landed_path was not this publisher's own-dir
            // landing (pass 2, `is_own_dir_landing`) — an un-attested light,
            // or one whose landing folder otherwise changed. It picks a
            // fresh own-dir name exactly like a New frame, but keeps its
            // row (`kind` stays `Update`/`Adopt`) and carries no manifest
            // meta — a version never re-announces it; the hub keeps the
            // prior meta.
            let meta = if matches!(kind, PublishKind::New) {
                match new_frame_meta_or_hold_back(&conn, project_id, fid) {
                    Ok(m) => Some(m),
                    Err(reason) => {
                        held_back.push(held(fid, &cand.filename, reason));
                        continue;
                    }
                }
            } else {
                None
            };
            let name = crate::export::calibrated_output_filename(
                &cand.filename,
                opts.debayer_osc && osc,
                opts.format,
            );
            let target = match new_frame_target(&conn, &own_dir, &name, &mut claimed, &taken_names)
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(project_id, frame_id = fid, error = %format!("{e:#}"), "publish: no landing path");
                    held_back.push(held(fid, &cand.filename, format!("no landing path: {e:#}")));
                    continue;
                }
            };
            let file_name = target
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if let Some(problem) = hub_file_name_problem(&file_name) {
                tracing::warn!(project_id, frame_id = fid, path = %target.display(), reason = problem, "publish: file name breaks the hub rule");
                held_back.push(held(
                    fid,
                    &cand.filename,
                    format!("the hub refuses this file name ({problem}): {file_name}"),
                ));
                continue;
            }
            // M3: without this, a generated New's picked basename never
            // enters `taken_names` — only the attested/external branch (and
            // `file_names_of_publisher`'s seed from a PRIOR run) did — so an
            // attested New later in this SAME run whose basename happens to
            // match could pass its own `taken_names.contains` check and both
            // would announce under one fileName.
            taken_names.insert(file_name);
            plans.push(PlannedFrame {
                cand,
                kind,
                target,
                meta,
                external: false,
                recipe: None,
            });
        }
        if let Some(hook) = after_split {
            hook(&conn);
        }
        // Adopt-class frames (the hub already lists them as mine, from a
        // replaced device) are updates, never reviewed (P6): Publish's. Pass
        // 1 already left them out; this is the guard that no other kind
        // reaches a Calibrate's generation.
        if matches!(scope, RunScope::Calibrate { .. }) {
            let before = plans.len();
            plans.retain(|p| matches!(p.kind, PublishKind::New));
            if plans.len() != before {
                tracing::debug!(
                    project_id,
                    count = before - plans.len(),
                    "calibrate: adopt-class frames left to Publish"
                );
            }
        }
        if refused_new > 0 && plans.is_empty() {
            // Nothing else to do: refuse at once, before any permit or hub
            // call (auto-publish stays quiet on it until the binding moves).
            let name = bound_elsewhere.clone().flatten();
            tracing::debug!(
                project_id,
                count = refused_new,
                outcome = "publishing_device",
                "publish: another device of this account publishes into this project; nothing announced"
            );
            return Err(ApiError::Conflict(
                crate::account::client::publishing_device_msg(name.as_deref()),
            ));
        }
        // Past every refusal: the prepared rows the split could not use.
        if !name_taken.is_empty() {
            name_taken.sort_unstable();
            name_taken.dedup();
            stale += drop_stale_prepared(&conn, project_id, &name_taken)?;
        }
        // P6: an adopted frame is published — its prepared row is consumed,
        // so no row ever names a published frame's landing. The file stays
        // when it IS the adoption's landing, or when the adoption is not
        // planned this run (where it would land is not decided); otherwise
        // it goes (never an attested original).
        if !adopt_prepared.is_empty() {
            let ids: Vec<i64> = adopt_prepared.keys().copied().collect();
            crate::db::collab_prepare::delete_prepared(&conn, project_id, &ids).map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: adopted frames' prepared rows not dropped");
                internal(e)
            })?;
            let elsewhere: Vec<crate::db::collab_prepare::PreparedRow> = adopt_prepared
                .into_values()
                .filter(|p| {
                    p.calibrated_path.as_deref().is_some_and(|path| {
                        plans.iter().any(|pl| {
                            pl.cand.frame_id == p.source_frame_id
                                && matches!(pl.kind, PublishKind::Adopt(_))
                                && pl.target != Path::new(path)
                        })
                    })
                })
                .collect();
            crate::api::collab_prepare::remove_prepared_files(&elsewhere);
            tracing::info!(
                project_id,
                count = ids.len(),
                "publish: adopted frames' prepared rows consumed"
            );
        }
    }
    Ok(Split {
        plans,
        prepared_cal,
        unchanged,
        refused_new,
        stale,
    })
}

/// A candidate after pass 1: its kind, whether the light is a one-shot colour
/// frame, and the attested original with its recipe, when it has one.
type SplitEntry = (
    PublishCandidate,
    PublishKind,
    bool,
    Option<(std::path::PathBuf, String)>,
);

/// A candidate after pass 2: as `SplitEntry`, with the landing path an update
/// or adoption already holds between the kind and the attested original.
type TargetEntry = (
    PublishCandidate,
    PublishKind,
    bool,
    Option<std::path::PathBuf>,
    Option<(std::path::PathBuf, String)>,
);

/// Pass 2 of the split (final review I1): every update and adoption target is
/// reserved before any new frame picks its name. Returns the publisher's taken
/// file names, the frames whose prepared name the run cannot use, and the
/// targeted candidates.
fn publish_split_targets(
    conn: &Connection,
    project_id: &str,
    account_id: &str,
    own_dir: &Path,
    split: Vec<SplitEntry>,
    claimed: &mut HashSet<std::path::PathBuf>,
    held_back: &mut Vec<HeldBackFrame>,
) -> Result<(HashSet<String>, Vec<i64>, Vec<TargetEntry>), ApiError> {
    use crate::db::collab_frames as frames_db;
    // Pass 2 (final review I1): every update and adoption target is
    // reserved BEFORE any new frame picks its name — an adoption without
    // a recorded path lands at `<own>/<fileName>`, which no row
    // references yet, so a new frame could otherwise pick the same file.
    // F5: an external candidate's target is always deferred to pass 3
    // (its own CURRENT catalog path, dedup-checked there) regardless of
    // kind — never the previous own row's `landed_path`, which for a
    // frame attested after an earlier generated publish would still name
    // the old calibrated file, not the original.
    let taken_names = if account_id.is_empty() {
        HashSet::new()
    } else {
        frames_db::file_names_of_publisher(&conn, project_id, &account_id).map_err(|e| {
            tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the publisher's file names failed");
            internal(e)
        })?
    };
    // Every prepared file of the project, by path: an adoption landing
    // on one would write over another frame's reviewed file and leave
    // its row naming a published frame's landing.
    let prepared_at: HashMap<std::path::PathBuf, i64> =
        crate::db::collab_prepare::list_prepared(&conn, project_id)
            .map_err(|e| {
                tracing::error!(project_id, error = %format!("{e:#}"), "publish: reading the prepared frames failed");
                internal(e)
            })?
            .into_iter()
            .filter_map(|r| {
                r.calibrated_path
                    .map(|p| (std::path::PathBuf::from(p), r.source_frame_id))
            })
            .collect();
    // Frames whose prepared name the run cannot use (stale — back to
    // Ready, the next calibrate picks a free name).
    let mut name_taken: Vec<i64> = Vec::new();
    let mut targeted: Vec<(
        PublishCandidate,
        PublishKind,
        bool,
        Option<std::path::PathBuf>,
        Option<(std::path::PathBuf, String)>,
    )> = Vec::with_capacity(split.len());
    for (cand, kind, osc, external) in split {
        let target = if external.is_some() {
            None
        } else {
            match &kind {
                PublishKind::New => None,
                PublishKind::Update(row) => match &row.landed_path {
                    // C1: a row whose landed_path is not this
                    // publisher's own-dir landing — or whose recipe is
                    // external-shaped (fix round 2 belt-and-braces) —
                    // was an attested original (or is otherwise stale):
                    // never reuse it as a generation target; defer to
                    // pass 3's fresh-name picker exactly like a New
                    // frame.
                    Some(p)
                        if is_own_dir_landing(Path::new(p), &own_dir)
                            && !was_external_recipe(row) =>
                    {
                        Some(std::path::PathBuf::from(p))
                    }
                    Some(p) => {
                        tracing::info!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, path = %p, "publish: own frame's landed path is not this publisher's landing (un-attested or moved); picking a fresh one");
                        None
                    }
                    None => {
                        tracing::error!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, "publish: own frame has no landed path");
                        held_back.push(held(
                            cand.frame_id,
                            &cand.filename,
                            "own frame has no file path".into(),
                        ));
                        continue;
                    }
                },
                PublishKind::Adopt(row) => {
                    let landing = adopt_landing(row, &own_dir);
                    if landing.is_none() {
                        tracing::info!(project_id, frame_id = cand.frame_id, frame_uuid = %row.frame_uuid, path = row.landed_path.as_deref().unwrap_or(""), "publish: own frame's landed path is not this publisher's landing (un-attested or moved); picking a fresh one");
                    }
                    landing
                }
            }
        };
        if let (PublishKind::Adopt(_), Some(t)) = (&kind, &target) {
            // Another frame's prepared file sits where this adoption
            // lands: that frame goes back to Ready (row and file) before
            // the adoption writes there.
            if let Some(&other) = prepared_at.get(t).filter(|&&o| o != cand.frame_id) {
                tracing::info!(project_id, frame_id = other, path = %t.display(), "publish: a prepared file sits on an adoption's landing; back to ready");
                name_taken.push(other);
            }
        }
        if let Some(t) = &target {
            claimed.insert(t.clone());
        }
        targeted.push((cand, kind, osc, target, external));
    }
    Ok((taken_names, name_taken, targeted))
}

/// Section 7: the own rows and their claims, one `BEGIN IMMEDIATE`
/// transaction per frame; a frame whose record fails is held back, never
/// aborts the run. Returns how many were announced and how many updated.
fn publish_record_own_rows(
    pass: &PublishPass<'_>,
    account_id: &str,
    display: &str,
    rows: ToRecord,
    held_back: &mut Vec<HeldBackFrame>,
    unchanged: &mut usize,
) -> Result<(usize, usize), ApiError> {
    let PublishPass {
        ctx, project_id, ..
    } = *pass;
    use crate::db::collab_frames::{self as frames_db, FrameOrigin};
    let ToRecord {
        announced,
        hub_adopted,
        bound,
        versioned,
        already_versioned,
    } = rows;
    // Claims (spec §6.2, hub § "Implicit claims"): an announce made the hub
    // claim `(uuid, 1)` for this device and a version `(uuid, new)` — both
    // enter the claim set WITHOUT an outbox row, in the same transaction as
    // the own row. An adoption (R8a/R8b) may have no hub claim of this
    // device behind it: `ensure_claimed` adds it through the outbox.
    let mut announced_n = 0usize;
    let mut updated_n = 0usize;
    {
        let db = db(ctx)?;
        let conn = db.conn();
        let mut record_failed = |f: &SeededFrame, what: &str, e: &anyhow::Error| {
            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the own frame failed");
            held_back.push(held(
                f.written.frame_id,
                &f.written.filename,
                format!("{what}, but recording it locally failed: {e:#}"),
            ));
        };
        let new_row = |f: &SeededFrame, frame_state: &str, gv: i32, accepted: bool| {
            let w = &f.written;
            let wire = frame_in_wire(f, gv);
            let mut manifest = serde_json::to_value(&wire).unwrap_or_else(|e| {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %e, "publish: encoding the manifest row failed");
                serde_json::json!({})
            });
            if let Some(obj) = manifest.as_object_mut() {
                obj.insert("publisherAccountId".into(), serde_json::json!(account_id));
                obj.insert("publisherDisplayName".into(), serde_json::json!(display));
                obj.insert("own".into(), serde_json::json!(true));
                obj.insert(
                    "contentVersion".into(),
                    serde_json::json!(f.content_version),
                );
                obj.insert("accepted".into(), serde_json::json!(accepted));
                obj.insert("state".into(), serde_json::json!(frame_state));
                obj.insert("manifestVersion".into(), serde_json::json!(0));
                obj.insert(
                    "createdAt".into(),
                    serde_json::json!(crate::sync::now_iso()),
                );
                obj.insert("holderCount".into(), serde_json::json!(1));
            }
            frames_db::LocalFrameRow {
                project_id: project_id.to_string(),
                frame_uuid: w.uuid.clone(),
                content_version: f.content_version,
                origin: FrameOrigin::Own,
                publisher_account_id: account_id.to_owned(),
                publisher_display: display.to_owned(),
                file_name: wire.file_name.clone(),
                filter_canonical: w.filter_canonical.clone(),
                state: frame_state.to_string(),
                accepted,
                byte_size: w.byte_size as i64,
                xxh3: w.xxh3.clone(),
                blake3: f.blake3.clone(),
                manifest_version: 0,
                manifest_json: manifest.to_string(),
                landed_path: Some(w.target.to_string_lossy().to_string()),
                size_mtime_seen: size_mtime_seen(&w.target),
                on_disk: true,
                awaiting_gc: false,
                source_frame_id: Some(w.frame_id),
                recipe_hash: Some(w.recipe.clone()),
                last_error: None,
                updated_at: String::new(),
                local_state: frames_db::LocalState::OwnHeld,
                frame_seq: None,
                own_staged: false,
            }
        };
        let adopt = |conn: &Connection, f: &SeededFrame| -> anyhow::Result<()> {
            let w = &f.written;
            let n = frames_db::adopt_own(
                conn,
                project_id,
                &w.uuid,
                w.frame_id,
                &w.target.to_string_lossy(),
                &w.recipe,
                size_mtime_seen(&w.target).as_deref(),
            )?;
            if n == 0 {
                anyhow::bail!("no own row for frame {} to bind", w.uuid);
            }
            Ok(())
        };
        // One transaction per frame: the own-row write(s) and the claim.
        // IMMEDIATE: a body may read before it writes (the "already
        // announced" bind reads the row first); a deferred read-to-write
        // upgrade under another writer fails at once with SQLITE_BUSY, never
        // waiting the busy timeout.
        let in_tx = |body: &dyn Fn(&Connection) -> anyhow::Result<()>| -> anyhow::Result<()> {
            let tx = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            body(&tx)?;
            tx.commit()?;
            Ok(())
        };

        for (f, frame_state, gv) in &announced {
            let row = new_row(f, frame_state, *gv, true);
            match in_tx(&|c| {
                frames_db::record_own(c, &row)?;
                // Spec §4.3: the prepared row is consumed; its file stays —
                // it is now the published `landed_path`.
                crate::db::collab_prepare::delete_prepared(c, project_id, &[f.written.frame_id])?;
                crate::db::collab_live::add_implicit_claim(
                    c,
                    project_id,
                    &f.written.uuid,
                    f.content_version,
                )
            }) {
                Ok(()) => announced_n += 1,
                Err(e) => record_failed(f, "announced", &e),
            }
        }
        // R8a: the hub already had these as mine. Bind an existing own row
        // (a manifest sync got there first), else record one from the local
        // plan with the state unknown — the manifest sync sets the hub truth.
        for (f, gv) in &hub_adopted {
            let result = in_tx(&|c| {
                match frames_db::get(c, project_id, &f.written.uuid)? {
                    Some(row) if row.origin == FrameOrigin::Own => adopt(c, f)?,
                    Some(_) => {
                        anyhow::bail!("frame {} is cached as another publisher's", f.written.uuid)
                    }
                    None => frames_db::record_own(c, &new_row(f, "unknown", *gv, false))?,
                }
                crate::db::collab_prepare::delete_prepared(c, project_id, &[f.written.frame_id])?;
                frames_db::ensure_claimed(c, project_id, &f.written.uuid)?;
                Ok(())
            });
            match result {
                Ok(()) => announced_n += 1,
                Err(e) => record_failed(f, "already announced", &e),
            }
        }
        // R8b, identical bytes: bound and seeded at the hub's version.
        for f in &bound {
            match in_tx(&|c| {
                adopt(c, f)?;
                frames_db::ensure_claimed(c, project_id, &f.written.uuid)?;
                Ok(())
            }) {
                Ok(()) => *unchanged += 1,
                Err(e) => record_failed(f, "already published", &e),
            }
        }
        for (f, already) in versioned
            .iter()
            .map(|f| (f, false))
            .chain(already_versioned.iter().map(|f| (f, true)))
        {
            let w = &f.written;
            let result = in_tx(&|c| {
                if matches!(w.kind, PublishKind::Adopt(_)) {
                    adopt(c, f)?;
                }
                if !already {
                    // The hub's implicit claim of the new version.
                    crate::db::collab_live::add_implicit_claim(
                        c,
                        project_id,
                        &w.uuid,
                        f.content_version,
                    )?;
                }
                frames_db::set_own_version(
                    c,
                    project_id,
                    &w.uuid,
                    f.content_version,
                    &f.blake3,
                    &w.xxh3,
                    w.byte_size as i64,
                    &w.recipe,
                    size_mtime_seen(&w.target).as_deref(),
                )?;
                if already {
                    frames_db::ensure_claimed(c, project_id, &w.uuid)?;
                }
                Ok(())
            });
            match result {
                Ok(()) => {
                    if already {
                        *unchanged += 1;
                    } else {
                        updated_n += 1;
                    }
                }
                Err(e) => record_failed(f, "versioned", &e),
            }
        }
    }
    Ok((announced_n, updated_n))
}

/// Seed by reference: every written frame is seeded under the project's disk
/// lock (an update or adoption) or without it (a new frame), and sorted into
/// the new frames, the updates and the frames bound at identical bytes.
#[allow(clippy::too_many_arguments)]
async fn publish_seed(
    pass: &PublishPass<'_>,
    node: &crate::sharing::iroh::node::SharedIrohNode,
    disk_lock: &tokio::sync::Mutex<()>,
    own_dir: &Path,
    from_prepared: &HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    written: Vec<WrittenFrame>,
    held_back: &mut Vec<HeldBackFrame>,
    stale: &mut usize,
    unchanged: &mut usize,
) -> Result<Seeded, ApiError> {
    let PublishPass {
        ctx,
        project_id,
        run,
        ..
    } = *pass;
    use crate::db::collab_frames::{self as frames_db, FrameOrigin};
    let mut new_frames: Vec<SeededFrame> = Vec::new();
    let mut updates: Vec<SeededFrame> = Vec::new();
    let mut bound: Vec<SeededFrame> = Vec::new();
    for (i, w) in written.into_iter().enumerate() {
        // One tick per frame, however its iteration ends.
        let _seeded = FrameDone {
            run,
            done: i + 1,
            file: w.filename.clone(),
        };
        if w.kind.row().is_none() {
            // A reviewed file is announced with its calibrate-time xxh3 and
            // size, so it is seeded only while it is still that file: the
            // section-1 currency check may be long past (other frames'
            // generation and seeding ran since). Changed or gone → stale:
            // back to Ready (row and non-external file), never announced.
            if let Some(p) = from_prepared.get(&w.frame_id) {
                if p.size_mtime_seen.is_none()
                    || size_mtime_seen_quiet(&w.target) != p.size_mtime_seen
                {
                    tracing::info!(project_id, frame_id = w.frame_id, path = %w.target.display(), "publish: prepared file changed since its review; back to ready");
                    let dropped = {
                        let db = db(ctx)?;
                        let conn = db.conn();
                        drop_stale_prepared(&conn, project_id, &[w.frame_id])
                    };
                    match dropped {
                        Ok(n) => *stale += n,
                        // Logged at `error` by `drop_stale_prepared`.
                        Err(e) => held_back.push(held(
                            w.frame_id,
                            &w.filename,
                            format!("the calibrated file changed since its review: {e}"),
                        )),
                    }
                    continue;
                }
            }
            match node
                .seed_project_frame(project_id, &w.uuid, 1, &w.target)
                .await
            {
                Ok(hash) => new_frames.push(SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: 1,
                    hub_prior: None,
                    written: w,
                }),
                Err(e) => {
                    tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: seeding a new frame failed; not announced");
                    held_back.push(held(
                        w.frame_id,
                        &w.filename,
                        format!("seeding failed: {e:#}"),
                    ));
                }
            }
            continue;
        }
        let _disk = disk_lock.lock().await;
        // C1c: the row as it is NOW — not as the split saw it.
        let current = {
            let db = db(ctx)?;
            let conn = db.conn();
            frames_db::get(&conn, project_id, &w.uuid)
        };
        let current = match current {
            Ok(Some(row)) if row.origin == FrameOrigin::Own => row,
            Ok(_) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, "publish: the own frame row vanished before seeding");
                // F5/A1: `staged == target` for an external frame — its
                // original is never removed.
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    "the frame's project row vanished during the run".into(),
                ));
                continue;
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: re-reading the own frame failed");
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    format!("cannot read the frame's project row: {e:#}"),
                ));
                continue;
            }
        };
        let identical = w.identical || w.staged_blake3.as_deref() == Some(current.blake3.as_str());
        if identical && !w.identical {
            // C1c: another run already versioned exactly these bytes.
            tracing::info!(project_id, frame_uuid = %w.uuid, content_version = current.content_version, "publish: the hub already has these bytes; no new version");
            if let PublishKind::Update(_) = &w.kind {
                // F5/A1: never remove an external frame's original.
                if w.staged != w.target {
                    remove_temp(project_id, &w.staged);
                }
                // I2: an attested original moved on disk (the app's own file
                // browser, or a forced Republish reading its CURRENT catalog
                // path) keeps identical bytes and size/mtime, so the recipe
                // is unchanged and this identical-bytes branch is the one
                // that runs — but `landed_path` still names the OLD location
                // unless moved here too, or the row reads "not on disk"
                // forever. Same `update_landed_path` + UNIQUE-failure
                // handling as the generated-file move above.
                let new_landed = w.target.to_string_lossy().to_string();
                if current.landed_path.as_deref() != Some(new_landed.as_str()) {
                    let moved = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                        frames_db::update_landed_path(&db.conn(), project_id, &w.uuid, &new_landed)
                    });
                    match moved {
                        Ok(n) if n > 0 => {
                            tracing::info!(project_id, frame_uuid = %w.uuid, from = current.landed_path.as_deref().unwrap_or(""), to = %new_landed, "publish: own frame's landed path moved (identical bytes, moved on disk)");
                        }
                        Ok(_) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, "publish: moving the own frame's landed path matched no row");
                        }
                        Err(e) => {
                            tracing::error!(project_id, frame_uuid = %w.uuid, path = %new_landed, error = %format!("{e:#}"), "publish: moving the own frame's landed path failed");
                        }
                    }
                }
                let written = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    frames_db::set_recipe_hash(&conn, project_id, &w.uuid, &w.recipe).and_then(
                        |n| {
                            // The hub already versioned exactly these bytes: a
                            // row staged with them is confirmed (Task 10, C11).
                            frames_db::clear_own_staged(
                                &conn,
                                project_id,
                                &w.uuid,
                                &current.blake3,
                                &w.xxh3,
                            )?;
                            Ok(n)
                        },
                    )
                };
                if let Err(e) = written {
                    tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: storing the new recipe failed");
                }
                *unchanged += 1;
                continue;
            }
        }
        let prior_version = current.content_version;
        let version = if identical {
            prior_version
        } else {
            // The old tag must not pin the old content once the landed file
            // is replaced.
            if let Err(e) = node.unseed_project_frame(project_id, &w.uuid).await {
                tracing::warn!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: unseeding the previous version failed");
            }
            prior_version + 1
        };
        // F5/A1: an external Update's `staged` IS `target` — the same
        // original path — so there is nothing to rename, and never a
        // `remove_temp` on it either.
        if w.staged != w.target {
            if let Err(e) = std::fs::rename(&w.staged, &w.target) {
                tracing::error!(project_id, frame_uuid = %w.uuid, src = %w.staged.display(), dest = %w.target.display(), error = %e, "publish: replacing the landed frame failed");
                remove_temp(project_id, &w.staged);
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    format!("cannot replace the published file: {e}"),
                ));
                continue;
            }
        }
        match node
            .seed_project_frame(project_id, &w.uuid, version, &w.target)
            .await
        {
            Ok(hash) => {
                publish_move_landed_path(ctx, project_id, &own_dir, &w, &current);
                let mut f = SeededFrame {
                    blake3: hash.to_hex().to_string(),
                    content_version: version,
                    hub_prior: None,
                    written: w,
                };
                if identical {
                    f.written.identical = true;
                    bound.push(f);
                    continue;
                }
                f.hub_prior = Some((current.xxh3.clone(), current.byte_size));
                let staged = {
                    let db = db(ctx)?;
                    let conn = db.conn();
                    frames_db::stage_own_file(
                        &conn,
                        project_id,
                        &f.written.uuid,
                        &f.written.target.to_string_lossy(),
                        &f.written.xxh3,
                        f.written.byte_size as i64,
                        size_mtime_seen(&f.written.target).as_deref(),
                    )
                };
                match staged {
                    Ok(0) => {
                        // Fix round 2 (finding 1): a silent no-match here
                        // means `landed_path` in the row does not (yet, or
                        // any longer) equal `f.written.target` — most often
                        // because the move above failed (finding 2) — and
                        // disk truth is now silently stale until the next
                        // publish notices the drift.
                        tracing::error!(project_id, frame_uuid = %f.written.uuid, path = %f.written.target.display(), "publish: recording the regenerated file matched no row");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the regenerated file failed");
                    }
                }
                updates.push(f);
            }
            Err(e) => {
                tracing::error!(project_id, frame_uuid = %w.uuid, error = %format!("{e:#}"), "publish: seeding failed");
                held_back.push(held(
                    w.frame_id,
                    &w.filename,
                    format!("seeding failed: {e:#}"),
                ));
            }
        }
    }
    Ok(Seeded {
        new_frames,
        updates,
        bound,
    })
}

/// Section 5: announce the new frames, at most 500 per batch. A refused batch
/// is unseeded; most refusals hold it back and record an error, an outdated
/// api sets `outdated` instead, and a publishing-device refusal replaces the
/// error recorded so far.
#[allow(clippy::too_many_arguments)]
async fn publish_announce(
    pass: &PublishPass<'_>,
    token: &str,
    client: &CollabClient,
    node: &crate::sharing::iroh::node::SharedIrohNode,
    disk_lock: &tokio::sync::Mutex<()>,
    mut gate_version: i32,
    new_frames: Vec<SeededFrame>,
    updates: &[SeededFrame],
    from_prepared: &HashMap<i64, crate::db::collab_prepare::PreparedRow>,
    first_err: &mut Option<ApiError>,
    held_back: &mut Vec<HeldBackFrame>,
) -> Result<Announced, ApiError> {
    let PublishPass {
        ctx,
        project_id,
        run,
        ..
    } = *pass;
    use crate::account::AccountClientError as E;
    let mut state: Option<String> = None;
    let mut announced: Vec<(SeededFrame, String, i32)> = Vec::new();
    let mut hub_adopted: Vec<(SeededFrame, i32)> = Vec::new();
    let mut stale_retried = false;
    let mut outdated = false;
    let mut announced_so_far = 0usize;
    let mut batches = announce_batches(new_frames);
    'batches: while let Some(mut batch) = batches.pop_front() {
        // Plan W3 / review focus 2: a frame withheld since the split is
        // never announced — untagged, its prepared row and file removed.
        // Fail-closed: when the withheld set cannot be read, this batch is
        // not announced.
        let withheld = {
            let db = db(ctx)?;
            let conn = db.conn();
            crate::db::collab_prepare::withheld_ids(&conn, project_id)
        };
        let withheld = match withheld {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(project_id, count = batch.len(), error = %format!("{e:#}"), "publish: reading the withheld frames failed; batch not announced");
                fail_batch(
                    &node,
                    project_id,
                    &batch,
                    &format!("cannot read the withheld frames: {e:#}"),
                    held_back,
                )
                .await;
                first_err.get_or_insert(internal(e));
                continue;
            }
        };
        let (keep, dropped): (Vec<SeededFrame>, Vec<SeededFrame>) = batch
            .into_iter()
            .partition(|f| !withheld.contains(&f.written.frame_id));
        batch = keep;
        if !dropped.is_empty() {
            let refs: Vec<&SeededFrame> = dropped.iter().collect();
            unseed_all(&node, project_id, &refs).await;
            let ids: Vec<i64> = dropped.iter().map(|f| f.written.frame_id).collect();
            drop_withheld_mid_run(ctx, project_id, &ids, &from_prepared);
        }
        // W4: cancel before each New-frame announce batch. What was seeded
        // and not announced is untagged; the frames stay prepared.
        if run.cancelled() {
            let mut rest: Vec<&SeededFrame> = batch.iter().collect();
            rest.extend(batches.iter().flatten());
            unseed_all(&node, project_id, &rest).await;
            tracing::info!(
                project_id,
                count = rest.len(),
                outcome = "cancelled",
                "publish: cancelled before announcing; frames stay prepared"
            );
            break 'batches;
        }
        loop {
            if batch.is_empty() {
                break;
            }
            let wire: Vec<_> = batch
                .iter()
                .map(|f| frame_in_wire(f, gate_version))
                .collect();
            match client.announce_frames(&token, project_id, &wire).await {
                Ok(resp) => {
                    state = Some(resp.state.clone());
                    tracing::info!(project_id, count = batch.len(), state = %resp.state, "publish: frames announced");
                    announced_so_far += batch.len();
                    announced.extend(
                        batch
                            .drain(..)
                            .map(|f| (f, resp.state.clone(), gate_version)),
                    );
                    run.tick(announced_so_far, None);
                    break;
                }
                Err(E::CollabApiOutdated) => {
                    // Nothing more goes out: roll back every seed that is not
                    // yet announced; what the hub already took is recorded
                    // below so it never becomes an orphan.
                    tracing::error!(
                        project_id,
                        outcome = "collab_api_outdated",
                        "publish: hub refused an outdated collab api; rolling back unsent seeds"
                    );
                    outdated = true;
                    let mut unsent: Vec<&SeededFrame> = batch.iter().collect();
                    unsent.extend(batches.iter().flatten());
                    unsent.extend(updates.iter());
                    unseed_all(&node, project_id, &unsent).await;
                    let staged: Vec<&SeededFrame> = updates.iter().collect();
                    unstage_updates(ctx, &disk_lock, project_id, &staged).await;
                    break 'batches;
                }
                Err(E::PublishingDevice {
                    device_id,
                    device_name,
                }) => {
                    // A6: another in-service device of this account is the
                    // project's publishing device. Nothing of this batch was
                    // written; nothing more is announced this run. Recorded
                    // so the next run (auto-publish included) stops before
                    // any generation, until the binding moves.
                    tracing::info!(
                        project_id,
                        count = batch.len(),
                        bound_device_id = %device_id,
                        outcome = "publishing_device",
                        "publish refused: another device of the account publishes into this project"
                    );
                    {
                        let recorded = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                            crate::db::collab::set_publishing_device(
                                &db.conn(),
                                project_id,
                                Some(&crate::db::collab::PublishingDevice {
                                    device_id: device_id.clone(),
                                    name: device_name.clone(),
                                }),
                            )
                        });
                        if let Err(e) = recorded {
                            tracing::error!(project_id, error = %format!("{e:#}"), "publish: recording the publishing device failed");
                        }
                    }
                    let rest: Vec<SeededFrame> = batches.drain(..).flatten().collect();
                    for group in [&batch, &rest] {
                        let refs: Vec<&SeededFrame> = group.iter().collect();
                        unseed_all(&node, project_id, &refs).await;
                        held_back.extend(group.iter().map(|f| {
                            held_bound_elsewhere(
                                f.written.frame_id,
                                &f.written.filename,
                                device_name.as_deref(),
                            )
                        }));
                    }
                    *first_err = Some(ApiError::Conflict(
                        crate::account::client::publishing_device_msg(device_name.as_deref()),
                    ));
                    break 'batches;
                }
                Err(e) if !already_announced_in(&e, &batch).is_empty() => {
                    // R8a: the hub already holds these uuids as mine (an
                    // announce whose reply was lost, or whose row was never
                    // recorded). Adopt them and retry the rest — each retry
                    // drops at least one frame, so the loop is bounded by the
                    // batch size.
                    let named = already_announced_in(&e, &batch);
                    tracing::warn!(project_id, count = named.len(), error = %e, "publish: frames already announced; adopting them and retrying the rest");
                    let mut i = 0usize;
                    let mut keep = Vec::with_capacity(batch.len());
                    for f in batch.drain(..) {
                        if named.contains(&i) {
                            hub_adopted.push((f, gate_version));
                        } else {
                            keep.push(f);
                        }
                        i += 1;
                    }
                    batch = keep;
                }
                Err(e) if !stale_retried && is_stale_gate_refusal(&e) => {
                    stale_retried = true;
                    tracing::warn!(project_id, version = gate_version, error = %e, "publish: stale gate version; refreshing thresholds and retrying once");
                    let verdicts =
                        match refresh_project_thresholds(ctx, &client, &token, project_id).await {
                            Ok(refreshed) => {
                                gate_version = refreshed.thresholds_version.unwrap_or(0);
                                regate(ctx, &refreshed)
                            }
                            Err(err) => Err(err),
                        };
                    let verdicts = match verdicts {
                        Ok(v) => v,
                        Err(err) => {
                            fail_batch(
                                &node,
                                project_id,
                                &batch,
                                &format!("announce failed: {err}"),
                                held_back,
                            )
                            .await;
                            first_err.get_or_insert(err);
                            break;
                        }
                    };
                    // The refreshed verdicts apply to this batch AND every
                    // queued one: a frame that no longer passes leaves.
                    let passes = |f: &SeededFrame| {
                        verdicts
                            .get(&f.written.frame_id)
                            .is_some_and(|r| r.publishable)
                    };
                    let mut dropped: Vec<SeededFrame> = Vec::new();
                    let (keep, drop_now): (Vec<_>, Vec<_>) =
                        batch.into_iter().partition(|f| passes(f));
                    batch = keep;
                    dropped.extend(drop_now);
                    for queued in batches.iter_mut() {
                        let (keep, drop_now): (Vec<_>, Vec<_>) =
                            std::mem::take(queued).into_iter().partition(|f| passes(f));
                        *queued = keep;
                        dropped.extend(drop_now);
                    }
                    let refs: Vec<&SeededFrame> = dropped.iter().collect();
                    unseed_all(&node, project_id, &refs).await;
                    for f in &dropped {
                        let reasons = verdicts
                            .get(&f.written.frame_id)
                            .map(|r| r.failures.clone())
                            .filter(|r| !r.is_empty())
                            .unwrap_or_else(|| {
                                vec!["no longer in the project's linked sets".into()]
                            });
                        held_back.push(HeldBackFrame {
                            frame_id: f.written.frame_id,
                            filename: f.written.filename.clone(),
                            reasons,
                            publishing_device: None,
                        });
                    }
                }
                Err(e) => {
                    // F5: nothing of this batch is announced — stop seeding
                    // it. The files stay; own rows are recorded only after a
                    // successful announce, so the next run re-announces them.
                    tracing::error!(project_id, count = batch.len(), error = %e, "publish: announce failed");
                    fail_batch(
                        &node,
                        project_id,
                        &batch,
                        &format!("announce failed: {e}"),
                        held_back,
                    )
                    .await;
                    first_err.get_or_insert(client_err(e));
                    break;
                }
            }
        }
    }
    Ok(Announced {
        state,
        announced,
        hub_adopted,
        outdated,
    })
}

/// Section 6: post the new content versions of the staged updates, at most
/// 500 per batch, after flushing the pending holdings. Sets `outdated` when
/// the hub refuses an outdated collab api.
#[allow(clippy::too_many_arguments)]
async fn publish_versions(
    pass: &PublishPass<'_>,
    token: &str,
    client: &CollabClient,
    node: &crate::sharing::iroh::node::SharedIrohNode,
    disk_lock: &tokio::sync::Mutex<()>,
    updates: Vec<SeededFrame>,
    outdated: &mut bool,
    first_err: &mut Option<ApiError>,
    held_back: &mut Vec<HeldBackFrame>,
) -> Result<Versions, ApiError> {
    let PublishPass {
        ctx, project_id, ..
    } = *pass;
    use crate::account::AccountClientError as E;
    use crate::db::collab_frames as frames_db;
    // Hub rule: any pending outbox entry for a frame is flushed BEFORE its
    // version call. A failed flush is logged and the versions still go out:
    // the hub keeps the highest `reportSeq` per frame, so a late flush is
    // harmless.
    let mut versioned: Vec<SeededFrame> = Vec::new();
    // Frames whose bytes the hub turned out to hold already (C1c): recorded
    // at the hub's version, counted unchanged, no `…/version`.
    let mut already_versioned: Vec<SeededFrame> = Vec::new();
    // Real version conflicts `(uuid, the hub's content version)`: the hub
    // has a newer version than this run based its update on — held back;
    // another run is asked for once the local manifest has caught up.
    let mut conflicts: Vec<(String, i32)> = Vec::new();
    // `conflict` answers at exactly `expectedVersion + 1`: our own version
    // whose first reply was lost — or another device of this account's.
    // Decided after the batches by the hub's BLAKE3 for that version.
    let mut maybe_ours: Vec<(SeededFrame, i32, i32)> = Vec::new();
    if !*outdated && !updates.is_empty() {
        if let Err(e) = crate::api::collab_live::holdings::flush_project_now(ctx, project_id).await
        {
            tracing::warn!(project_id, error = %e, "publish: flushing holdings before the versions failed; posting the versions anyway");
        }
    }
    if !*outdated {
        // C1c: never post a version whose bytes are the row's CURRENT
        // content — re-read right before the call; the re-read row's
        // version is what the new one supersedes (CAS `expectedVersion`).
        let mut to_post: Vec<(SeededFrame, i32)> = Vec::new();
        for mut f in updates {
            let current = {
                let db = db(ctx)?;
                let conn = db.conn();
                frames_db::get(&conn, project_id, &f.written.uuid)
            };
            let expected = match current {
                Ok(Some(row)) if row.blake3 == f.blake3 => {
                    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = row.content_version, "publish: the hub already has these bytes; no new version");
                    if row.content_version != f.content_version {
                        retag_under_hub_version(&node, project_id, &mut f, row.content_version)
                            .await;
                    }
                    already_versioned.push(f);
                    continue;
                }
                Ok(Some(row)) => row.content_version,
                Ok(None) => {
                    tracing::warn!(project_id, frame_uuid = %f.written.uuid, "publish: the own frame row vanished before its version; posting against the seeded version");
                    f.content_version - 1
                }
                Err(e) => {
                    // The hub is the arbiter: post, and let it refuse.
                    tracing::warn!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: re-reading the own frame before its version failed");
                    f.content_version - 1
                }
            };
            to_post.push((f, expected));
        }
        let mut batches = announce_batches(to_post);
        while let Some(batch) = batches.pop_front() {
            let wire: Vec<crate::collab::live::wire::VersionInWire> = batch
                .iter()
                .map(|(f, expected)| crate::collab::live::wire::VersionInWire {
                    uuid: f.written.uuid.clone(),
                    expected_version: *expected,
                    blake3: f.blake3.clone(),
                    byte_size: f.written.byte_size as i64,
                    xxh3: f.written.xxh3.clone(),
                })
                .collect();
            let reply = crate::collab::hub_client::with_retry(
                "publish versions",
                crate::collab::hub_client::RetryPolicy::Interactive,
                || client.frame_versions(&token, project_id, &wire),
            )
            .await;
            let reply = match reply {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(project_id, count = batch.len(), error = %e, "publish: new frame versions failed");
                    let frames: Vec<SeededFrame> = batch.into_iter().map(|(f, _)| f).collect();
                    let refs: Vec<&SeededFrame> = frames.iter().collect();
                    unseed_all(&node, project_id, &refs).await;
                    unstage_updates(ctx, &disk_lock, project_id, &refs).await;
                    for f in &frames {
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            format!("new version failed: {e}"),
                        ));
                    }
                    if matches!(e, E::CollabApiOutdated) {
                        *outdated = true;
                        let rest: Vec<SeededFrame> =
                            batches.drain(..).flatten().map(|(f, _)| f).collect();
                        let refs: Vec<&SeededFrame> = rest.iter().collect();
                        unseed_all(&node, project_id, &refs).await;
                        unstage_updates(ctx, &disk_lock, project_id, &refs).await;
                    }
                    first_err.get_or_insert(client_err(e));
                    if *outdated {
                        break;
                    }
                    continue;
                }
            };
            let mut results = reply.results.into_iter();
            for (f, expected) in batch {
                use crate::collab::live::wire::VersionStatus;
                let result = results.next().filter(|r| r.uuid == f.written.uuid);
                match &result {
                    Some(r) if r.status == VersionStatus::Ok => {
                        let cv = r.content_version;
                        accept_version(&node, project_id, f, cv, &mut versioned).await;
                    }
                    Some(r)
                        if r.status == VersionStatus::Conflict
                            && r.content_version == expected + 1 =>
                    {
                        maybe_ours.push((f, expected, r.content_version));
                    }
                    Some(r) if r.status == VersionStatus::Conflict => {
                        let n = r.content_version;
                        refuse_version(
                            ctx,
                            &node,
                            &disk_lock,
                            project_id,
                            f,
                            n,
                            &mut conflicts,
                            held_back,
                        )
                        .await;
                    }
                    Some(r) if r.status == VersionStatus::NotPublishingDevice => {
                        // A6: another device of this account versions this
                        // frame. Not retried until its manifest row changes.
                        tracing::info!(project_id, frame_uuid = %f.written.uuid, outcome = "not_publishing_device", "publish: new frame version refused: another device of the account versions this frame");
                        unseed_all(&node, project_id, &[&f]).await;
                        unstage_updates(ctx, &disk_lock, project_id, &[&f]).await;
                        let marked = db(ctx).map_err(|e| anyhow::anyhow!("{e}")).and_then(|db| {
                            let conn = db.conn();
                            let mv = frames_db::get(&conn, project_id, &f.written.uuid)?
                                .map_or(0, |r| r.manifest_version);
                            frames_db::set_error(
                                &conn,
                                project_id,
                                &f.written.uuid,
                                Some(&not_publishing_mark(mv)),
                            )
                        });
                        if let Err(e) = marked {
                            tracing::error!(project_id, frame_uuid = %f.written.uuid, error = %format!("{e:#}"), "publish: recording the version refusal failed");
                        }
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            "new version refused: another device of this account versions this frame now".into(),
                        ));
                    }
                    other => {
                        let reason = match other {
                            Some(r) if r.status == VersionStatus::Unknown => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused with a status this build does not know");
                                "new version failed: the hub answered with a status this version of Athenaeum does not know"
                            }
                            Some(r) if r.status == VersionStatus::NotFound => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused: the hub does not know the frame");
                                "new version failed: the hub does not know this frame"
                            }
                            Some(r) if r.status == VersionStatus::Forbidden => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: new frame version refused: not the frame's publisher");
                                "new version failed: the hub says this device's account did not publish the frame"
                            }
                            _ => {
                                tracing::error!(project_id, frame_uuid = %f.written.uuid, "publish: the versions reply has no matching result for this frame");
                                "new version failed: the hub's reply did not answer for this frame"
                            }
                        };
                        unseed_all(&node, project_id, &[&f]).await;
                        unstage_updates(ctx, &disk_lock, project_id, &[&f]).await;
                        held_back.push(held(
                            f.written.frame_id,
                            &f.written.filename,
                            reason.to_string(),
                        ));
                    }
                }
            }
        }
        // A conflict at `expected + 1` is our own lost reply ONLY when the
        // hub's BLAKE3 for that version is ours (controller ruling): another
        // device of this account may have bumped it with other bytes. One
        // manifest delta read decides; a failed read decides "not ours" (the
        // safe side: the frame is held back and regenerated next run).
        if !maybe_ours.is_empty() {
            let uuids: HashSet<String> = maybe_ours
                .iter()
                .map(|(f, _, _)| f.written.uuid.clone())
                .collect();
            let since = {
                let db = db(ctx)?;
                let conn = db.conn();
                let mut since = i64::MAX;
                for u in &uuids {
                    let mv = frames_db::get(&conn, project_id, u)
                        .ok()
                        .flatten()
                        .map_or(0, |r| r.manifest_version);
                    since = since.min(mv);
                }
                since.clamp(0, i64::MAX)
            };
            let hub = match hub_frame_versions(&client, &token, project_id, since, &uuids).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(project_id, error = %e, "publish: reading the hub's versions after a conflict failed; treating the conflicts as real");
                    HashMap::new()
                }
            };
            for (f, expected, n) in std::mem::take(&mut maybe_ours) {
                if is_own_lost_reply(expected, &f.blake3, hub.get(&f.written.uuid)) {
                    tracing::info!(project_id, frame_uuid = %f.written.uuid, content_version = n, "publish: the hub already took this version (a retried call)");
                    accept_version(&node, project_id, f, n, &mut versioned).await;
                } else {
                    refuse_version(
                        ctx,
                        &node,
                        &disk_lock,
                        project_id,
                        f,
                        n,
                        &mut conflicts,
                        held_back,
                    )
                    .await;
                }
            }
        }
    }
    Ok(Versions {
        versioned,
        already_versioned,
        conflicts,
    })
}
