//! The coordinator's filter remap (collab v3 wave 6, spec §11; plan P3, P8,
//! P9). Buckets = publisher × camera × raw filter over the local manifest
//! mirror; a remap PATCHes each frame's `filterCanonical` on the hub, paced
//! (4 in flight, ≤ 10 per second), cancellable, one per project at a time.
//! No local row is written: the manifest edit comes back through the feed.
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::account::client::AccountClientError;
use crate::api::{db, ApiError};
use crate::collab::filters::DictionaryEntry;
use crate::collab::hub_client::CollabClient;
use crate::events::{emit_event, ProgressEmitter};
use crate::services::ServiceContext;

pub const COLLAB_REMAP_PROGRESS_EVENT: &str = "collab-remap-progress";
pub const COLLAB_REMAP_FINISHED_EVENT: &str = "collab-remap-finished";
pub const REMAP_BUSY_MSG: &str = "A filter remap is already running for this project.";
const IN_FLIGHT: usize = 4;
const MIN_SPACING: Duration = Duration::from_millis(100);
const RATE_LIMIT_WAIT: Duration = Duration::from_secs(5);
const RATE_LIMIT_RETRIES: u32 = 3;
const THROTTLE: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabRemapProgress {
    pub project_id: String,
    pub canonical: String,
    pub done: u32,
    pub failed: u32,
    pub total: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabRemapFinished {
    pub project_id: String,
    pub canonical: String,
    pub done: u32,
    pub failed: u32,
    pub total: u32,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterBucket {
    pub publisher_account_id: String,
    pub publisher: String,
    pub instrume: String,
    pub filter_raw: String,
    /// The bucket's canonical filter, `None` when its frames disagree.
    pub canonical: Option<String>,
    pub frame_uuids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FilterBucketSheet {
    pub project_id: String,
    pub dictionary: Vec<DictionaryEntry>,
    pub buckets: Vec<FilterBucket>,
    pub running: Option<CollabRemapProgress>,
}

struct RemapState {
    cancel: AtomicBool,
    progress: Mutex<CollabRemapProgress>,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<RemapState>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<RemapState>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn key(ctx: &ServiceContext, project_id: &str) -> Result<String, ApiError> {
    Ok(format!("{}|{project_id}", db(ctx)?.path().display()))
}

pub fn get_collab_filter_buckets(
    ctx: &ServiceContext,
    project_id: &str,
) -> Result<FilterBucketSheet, ApiError> {
    let database = db(ctx)?;
    let conn = database.conn();
    let project = crate::api::collab_exchange::live_project(&conn, project_id)?;
    let dictionary = crate::api::collab::project_dictionary(&project)?;
    let rows = crate::db::collab_frames::list_for_project(&conn, project_id)?;
    let mut by_key: BTreeMap<(String, String, String), FilterBucket> = BTreeMap::new();
    for row in rows {
        let manifest: serde_json::Value = serde_json::from_str(&row.manifest_json).unwrap_or_else(|e| {
            tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %e, "remap buckets: manifest row is not valid json");
            serde_json::Value::Null
        });
        let filter_raw = manifest
            .get("filterRaw")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let instrume = manifest
            .pointer("/meta/instrume")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let k = (
            row.publisher_account_id.clone(),
            instrume.clone(),
            filter_raw.clone(),
        );
        let b = by_key.entry(k).or_insert_with(|| FilterBucket {
            publisher_account_id: row.publisher_account_id.clone(),
            publisher: row.publisher_display.clone(),
            instrume,
            filter_raw,
            canonical: Some(row.filter_canonical.clone()),
            frame_uuids: Vec::new(),
        });
        if b.canonical.as_deref() != Some(row.filter_canonical.as_str()) {
            b.canonical = None;
        }
        b.frame_uuids.push(row.frame_uuid);
    }
    let running = registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key(ctx, project_id)?)
        .map(|s| s.progress.lock().unwrap_or_else(|p| p.into_inner()).clone());
    Ok(FilterBucketSheet {
        project_id: project_id.to_string(),
        dictionary,
        buckets: by_key.into_values().collect(),
        running,
    })
}

enum Stop {
    Forbidden,
    Refused(String),
}

/// One PATCH, a 429 waited out (`wait`, up to RATE_LIMIT_RETRIES times).
async fn patch_with_retry(
    client: &CollabClient,
    token: &str,
    pid: &str,
    uuid: &str,
    canonical: &str,
    wait: Duration,
) -> Result<(), AccountClientError> {
    let mut tries = 0;
    loop {
        match client.patch_frame_filter(token, pid, uuid, canonical).await {
            Err(AccountClientError::RateLimited) if tries < RATE_LIMIT_RETRIES => {
                tries += 1;
                tracing::debug!(
                    project_id = pid,
                    frame_uuid = uuid,
                    tries,
                    "remap: rate limited, waiting"
                );
                tokio::time::sleep(wait).await;
            }
            other => return other,
        }
    }
}

pub async fn remap_collab_filter(
    ctx: Arc<ServiceContext>,
    project_id: String,
    frame_uuids: Vec<String>,
    canonical: String,
    emitter: Option<Arc<dyn ProgressEmitter>>,
) -> Result<(), ApiError> {
    if frame_uuids.is_empty() {
        return Err(ApiError::Invalid("no frames to remap".into()));
    }
    {
        let database = db(&ctx)?;
        let conn = database.conn();
        let project = crate::api::collab_exchange::live_project(&conn, &project_id)?;
        let dictionary = crate::api::collab::project_dictionary(&project)?;
        if !dictionary.iter().any(|d| d.canonical == canonical) {
            return Err(ApiError::Invalid(format!(
                "{canonical:?} is not in the project's filter dictionary"
            )));
        }
    }
    let Some((hub_url, token)) = crate::api::account::hub_credentials(&ctx)? else {
        return Err(ApiError::SignedOut("Sign in to remap filters.".into()));
    };
    let client =
        Arc::new(CollabClient::new(&hub_url).map_err(crate::api::collab_exchange::client_err)?);
    let k = key(&ctx, &project_id)?;
    let total = frame_uuids.len() as u32;
    let state = Arc::new(RemapState {
        cancel: AtomicBool::new(false),
        progress: Mutex::new(CollabRemapProgress {
            project_id: project_id.clone(),
            canonical: canonical.clone(),
            done: 0,
            failed: 0,
            total,
        }),
    });
    {
        let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        if reg.contains_key(&k) {
            tracing::warn!(project_id, "remap refused: one is already running");
            return Err(ApiError::Conflict(REMAP_BUSY_MSG.into()));
        }
        reg.insert(k.clone(), state.clone());
    }
    tracing::info!(project_id, canonical, count = total, "filter remap started");
    if let Some(em) = emitter.as_deref() {
        emit_event(
            em,
            COLLAB_REMAP_PROGRESS_EVENT,
            &*state.progress.lock().unwrap_or_else(|p| p.into_inner()),
        );
    }
    tokio::spawn(async move {
        let started = Instant::now();
        let mut stop: Option<Stop> = None;
        let mut last_emit = Instant::now();
        let mut queue = frame_uuids.into_iter();
        let mut inflight = tokio::task::JoinSet::new();
        let mut next_send = Instant::now();
        loop {
            while stop.is_none()
                && !state.cancel.load(Ordering::SeqCst)
                && inflight.len() < IN_FLIGHT
            {
                let Some(uuid) = queue.next() else { break };
                tokio::time::sleep_until(next_send.into()).await;
                next_send = Instant::now() + MIN_SPACING;
                let (c, t, p, can) = (
                    client.clone(),
                    token.clone(),
                    project_id.clone(),
                    canonical.clone(),
                );
                inflight.spawn(async move {
                    let r = patch_with_retry(&c, &t, &p, &uuid, &can, RATE_LIMIT_WAIT).await;
                    (uuid, r)
                });
            }
            let Some(joined) = inflight.join_next().await else {
                break;
            };
            let (uuid, res) = match joined {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(project_id, error = %e, "remap: a request task panicked");
                    state
                        .progress
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .failed += 1;
                    continue;
                }
            };
            {
                let mut p = state.progress.lock().unwrap_or_else(|p| p.into_inner());
                match res {
                    Ok(()) => p.done += 1,
                    Err(AccountClientError::Forbidden) => {
                        p.failed += 1;
                        stop.get_or_insert(Stop::Forbidden);
                    }
                    Err(AccountClientError::Http {
                        status: 400,
                        message,
                    }) => {
                        p.failed += 1;
                        stop.get_or_insert(Stop::Refused(message));
                    }
                    Err(e) => {
                        p.failed += 1;
                        tracing::warn!(project_id, frame_uuid = %uuid, error = %e, "remap: frame failed");
                    }
                }
            }
            if let Some(em) = emitter.as_deref() {
                if last_emit.elapsed() >= THROTTLE {
                    last_emit = Instant::now();
                    emit_event(
                        em,
                        COLLAB_REMAP_PROGRESS_EVENT,
                        &*state.progress.lock().unwrap_or_else(|p| p.into_inner()),
                    );
                }
            }
        }
        let p = state
            .progress
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let cancelled = state.cancel.load(Ordering::SeqCst) && stop.is_none();
        let error = match stop {
            Some(Stop::Forbidden) => {
                Some("This account lacks thresholds.edit on the project.".to_string())
            }
            Some(Stop::Refused(m)) => Some(format!("The hub refused the remap: {m}")),
            None => None,
        };
        registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&k);
        let fin = CollabRemapFinished {
            project_id: p.project_id.clone(),
            canonical: p.canonical.clone(),
            done: p.done,
            failed: p.failed,
            total: p.total,
            cancelled,
            error: error.clone(),
        };
        match &error {
            Some(e) => {
                tracing::error!(project_id = %p.project_id, done = p.done, failed = p.failed, error = %e, "filter remap stopped")
            }
            None => {
                tracing::info!(project_id = %p.project_id, done = p.done, failed = p.failed, cancelled, duration_ms = started.elapsed().as_millis() as u64, "filter remap finished")
            }
        }
        if let Some(em) = emitter.as_deref() {
            emit_event(em, COLLAB_REMAP_FINISHED_EVENT, &fin);
        }
    });
    Ok(())
}

pub fn cancel_collab_remap(ctx: &ServiceContext, project_id: &str) -> Result<(), ApiError> {
    match registry()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&key(ctx, project_id)?)
    {
        Some(s) => {
            s.cancel.store(true, Ordering::SeqCst);
            tracing::info!(project_id, "filter remap cancel requested");
        }
        None => tracing::debug!(project_id, "remap cancel: none running"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab_exchange::test_support::{test_ctx, wire_hub};
    use crate::collab::fake_hub::FakeHub;
    use base64::Engine;
    use std::sync::Mutex as StdMutex;

    struct Rec(StdMutex<Vec<(String, serde_json::Value)>>);
    impl ProgressEmitter for Rec {
        fn emit_json(&self, name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((name.to_string(), payload));
        }
    }

    async fn fixture(coordinator: bool) -> (tempfile::TempDir, Arc<ServiceContext>, FakeHub) {
        let hub = FakeHub::start().await;
        let (tmp, ctx) = test_ctx();
        wire_hub(&ctx, &hub.uri(), "tok");
        let b64 = base64::engine::general_purpose::STANDARD;
        hub.add_account("tok", "acc-me", "Me", &b64.encode([7u8; 32]), None);
        hub.add_account("tok-o", "acc-o", "Other", &b64.encode([8u8; 32]), None);
        hub.add_project(
            "p1",
            "m31",
            &[
                ("acc-me", "send_receive", coordinator),
                ("acc-o", "send_receive", false),
            ],
            false,
        );
        let uuids: Vec<String> = (0..12).map(|i| format!("u{i:02}")).collect();
        let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
        hub.seed_frames_with("p1", "acc-o", &refs, "published", |f| {
            f.filter_raw = "Red".into()
        });
        crate::api::collab::refresh_projects(&ctx).await.unwrap();
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None)
            .await
            .unwrap();
        (tmp, Arc::new(ctx), hub)
    }

    async fn finished(rec: &Rec) -> CollabRemapFinished {
        for _ in 0..400 {
            if let Some((_, v)) = rec
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|(n, _)| n == COLLAB_REMAP_FINISHED_EVENT)
            {
                return serde_json::from_value(v.clone()).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("no {COLLAB_REMAP_FINISHED_EVENT}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn buckets_group_by_publisher_camera_raw_filter() {
        let (_t, ctx, _hub) = fixture(true).await;
        let sheet = get_collab_filter_buckets(&ctx, "p1").unwrap();
        assert_eq!(sheet.buckets.len(), 1);
        let b = &sheet.buckets[0];
        assert_eq!((b.filter_raw.as_str(), b.frame_uuids.len()), ("Red", 12));
        assert!(!sheet.dictionary.is_empty());
        assert!(sheet.running.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remap_patches_every_frame_and_reports_once() {
        let (_t, ctx, hub) = fixture(true).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0]
            .frame_uuids
            .clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(
            ctx.clone(),
            "p1".into(),
            uuids.clone(),
            "R".into(),
            Some(rec.clone()),
        )
        .await
        .unwrap();
        let f = finished(&rec).await;
        assert_eq!((f.done, f.failed, f.total, f.cancelled), (12, 0, 12, false));
        for u in &uuids {
            assert_eq!(hub.frame("p1", u).unwrap().filter_canonical, "R");
        }
        assert_eq!(
            rec.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == COLLAB_REMAP_FINISHED_EVENT)
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_remap_is_refused_cancel_stops_the_first_and_a_third_is_admitted() {
        let (_t, ctx, _hub) = fixture(true).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0]
            .frame_uuids
            .clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(
            ctx.clone(),
            "p1".into(),
            uuids.clone(),
            "R".into(),
            Some(rec.clone()),
        )
        .await
        .unwrap();
        // Pacing (<= 10/s) keeps 12 frames running for > 1 s: the second is refused.
        let second =
            remap_collab_filter(ctx.clone(), "p1".into(), uuids.clone(), "G".into(), None).await;
        assert!(matches!(second, Err(ApiError::Conflict(_))), "{second:?}");
        assert!(get_collab_filter_buckets(&ctx, "p1")
            .unwrap()
            .running
            .is_some());
        cancel_collab_remap(&ctx, "p1").unwrap();
        let f = finished(&rec).await;
        assert!(f.cancelled, "{f:?}");
        assert!(f.done < 12, "{f:?}");
        assert!(f.error.is_none());
        // The registry slot is released just before the finished event.
        let rec2 = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(
            ctx.clone(),
            "p1".into(),
            uuids,
            "G".into(),
            Some(rec2.clone()),
        )
        .await
        .unwrap();
        cancel_collab_remap(&ctx, "p1").unwrap();
        finished(&rec2).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_display_name_two_accounts_make_two_buckets() {
        let (_t, ctx, hub) = fixture(true).await;
        hub.add_account(
            "tok-x",
            "acc-x",
            "Other",
            &base64::engine::general_purpose::STANDARD.encode([9u8; 32]),
            None,
        );
        hub.seed_frames_with("p1", "acc-x", &["x00", "x01"], "published", |f| {
            f.filter_raw = "Red".into()
        });
        crate::api::collab_exchange::sync_manifest(&ctx, "p1", None, None)
            .await
            .unwrap();
        let sheet = get_collab_filter_buckets(&ctx, "p1").unwrap();
        let mut ids: Vec<&str> = sheet
            .buckets
            .iter()
            .map(|b| b.publisher_account_id.as_str())
            .collect();
        ids.sort();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_ne!(ids[0], ids[1]);
    }

    #[tokio::test]
    async fn a_429_is_waited_out_and_retried() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = CollabClient::new(server.uri()).unwrap();
        patch_with_retry(&client, "tok", "p1", "u00", "R", Duration::from_millis(1))
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_member_without_thresholds_edit_stops_at_the_first_forbidden() {
        let (_t, ctx, _hub) = fixture(false).await;
        let uuids = get_collab_filter_buckets(&ctx, "p1").unwrap().buckets[0]
            .frame_uuids
            .clone();
        let rec = Arc::new(Rec(StdMutex::new(Vec::new())));
        remap_collab_filter(
            ctx.clone(),
            "p1".into(),
            uuids,
            "R".into(),
            Some(rec.clone()),
        )
        .await
        .unwrap();
        let f = finished(&rec).await;
        assert_eq!(f.done, 0);
        assert!(
            f.error.as_deref().unwrap_or("").contains("thresholds.edit"),
            "{f:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_canonical_outside_the_dictionary_is_refused_up_front() {
        let (_t, ctx, _hub) = fixture(true).await;
        let err = remap_collab_filter(
            ctx.clone(),
            "p1".into(),
            vec!["u00".into()],
            "Nope".into(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
    }
}
