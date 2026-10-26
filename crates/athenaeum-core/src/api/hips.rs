//! Sky Chart background commands (issue #18, phase 1): the survey list,
//! one tile by address, and clearing a survey's private tile store.
//!
//! A tile is served from the store; a missing one is fetched from the
//! survey's master only when `skychart.hips_online` is on. Nothing here
//! downloads anything a user did not scroll to.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

use super::ApiError;
use crate::hips::{self, CachedTile, FetchedTile, HipsStore, HipsSurvey, Properties};
use crate::services::ServiceContext;
use crate::settings::{defaults, keys};

/// One survey as the Sky Chart and the Settings page show it.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HipsSurveyInfo {
    pub id: String,
    pub label: String,
    /// `image/jpeg` or `image/png`.
    pub mime: String,
    /// Deepest order the chart may request (the survey's, capped at 6).
    pub max_order: u8,
    pub tile_width: u32,
    /// The survey's `obs_copyright`, or the preset's until `properties` were read.
    pub attribution: String,
    pub attribution_url: Option<String>,
    /// `hips_license`, or the preset's.
    pub license: String,
    /// Online fetching is on (`skychart.hips_online`).
    pub online: bool,
    pub cached_tiles: u64,
    pub cached_bytes: u64,
}

/// A tile as served to the chart.
pub enum HipsTile {
    Image { bytes: Vec<u8>, mime: &'static str },
    /// No coverage at this address (the master answered 404).
    Absent,
}

fn app_data_dir(ctx: &ServiceContext) -> Result<PathBuf, ApiError> {
    let db = ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))?;
    db.path()
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| ApiError::Internal("Cannot determine the app data directory".into()))
}

fn online(ctx: &ServiceContext) -> Result<bool, ApiError> {
    let db = ctx.db.get().ok_or_else(|| ApiError::Internal("Database not initialized".into()))?;
    let conn = db.conn();
    let v = ctx.settings.get_with_precedence(&conn, keys::SKYCHART_HIPS_ONLINE, defaults::SKYCHART_HIPS_ONLINE)?;
    Ok(v != "false")
}

fn find_survey(id: &str) -> Result<&'static HipsSurvey, ApiError> {
    hips::survey(id).ok_or_else(|| ApiError::NotFound(format!("Unknown sky survey: {id}")))
}

/// Surveys whose stored `properties` passed validation in this process.
fn validated() -> &'static Mutex<HashSet<&'static str>> {
    static V: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Makes sure the survey's `properties` are on disk and drawable; fetches
/// them from the master on first use when online.
fn ensure_properties(store: &HipsStore, survey: &'static HipsSurvey, online: bool) -> Result<(), ApiError> {
    if validated().lock().unwrap().contains(survey.id) {
        return Ok(());
    }
    let props = match store.read_properties()? {
        Some(p) => p,
        None if online => {
            let text = hips::fetch_properties(survey).map_err(|e| {
                tracing::warn!(survey_id = survey.id, error = %format!("{e:#}"), "hips properties fetch failed");
                ApiError::Internal(format!("{e:#}"))
            })?;
            let p = Properties::parse(&text);
            p.validate(survey).map_err(|e| {
                tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips survey properties refused");
                ApiError::Conflict(format!("{e:#}"))
            })?;
            store.write_properties(&text)?;
            p
        }
        None => {
            return Err(ApiError::NotFound(format!(
                "{} has no local tiles yet and online fetching is off",
                survey.label
            )))
        }
    };
    props.validate(survey).map_err(|e| {
        tracing::error!(survey_id = survey.id, error = %format!("{e:#}"), "hips survey properties refused");
        ApiError::Conflict(format!("{e:#}"))
    })?;
    validated().lock().unwrap().insert(survey.id);
    Ok(())
}

pub fn get_hips_surveys(ctx: &ServiceContext) -> Result<Vec<HipsSurveyInfo>, ApiError> {
    let app_data = app_data_dir(ctx)?;
    let online = online(ctx)?;
    hips::SURVEYS
        .iter()
        .map(|s| {
            let store = HipsStore::new(&app_data, s);
            let props = store.read_properties()?.unwrap_or_default();
            let (cached_tiles, cached_bytes) = store.usage()?;
            Ok(HipsSurveyInfo {
                id: s.id.to_string(),
                label: s.label.to_string(),
                mime: s.format.mime().to_string(),
                max_order: hips::request_max_order(s),
                tile_width: hips::TILE_WIDTH,
                attribution: props.get("obs_copyright").unwrap_or(s.fallback_attribution).to_string(),
                attribution_url: props.get("obs_copyright_url").map(str::to_string),
                license: props.get("hips_license").unwrap_or(s.fallback_license).to_string(),
                online,
                cached_tiles,
                cached_bytes,
            })
        })
        .collect()
}

/// One tile: from the store, else (online) from the survey's master, which
/// is then stored. Offline and not stored is `NotFound`; a fetch failure is
/// `Internal` and is not remembered, so a later request tries again.
pub fn get_hips_tile(ctx: &ServiceContext, survey_id: &str, order: u8, npix: u64) -> Result<HipsTile, ApiError> {
    let survey = find_survey(survey_id)?;
    hips::validate_address(survey, order, npix).map_err(|e| ApiError::Invalid(format!("{e:#}")))?;
    let store = HipsStore::new(&app_data_dir(ctx)?, survey);
    let mime = survey.format.mime();

    match store.read(order, npix)? {
        CachedTile::Image(bytes) => return Ok(HipsTile::Image { bytes, mime }),
        CachedTile::Absent => return Ok(HipsTile::Absent),
        CachedTile::Missing => {}
    }
    let online = online(ctx)?;
    if !online {
        return Err(ApiError::NotFound(format!("Tile {order}/{npix} is not cached and online fetching is off")));
    }
    ensure_properties(&store, survey, online)?;
    match hips::fetch_tile(survey, order, npix) {
        Ok(FetchedTile::Image(bytes)) => {
            tracing::debug!(survey_id = survey.id, hips_order = order, npix, bytes = bytes.len(), "hips tile fetched");
            store.write(order, npix, &bytes)?;
            Ok(HipsTile::Image { bytes, mime })
        }
        Ok(FetchedTile::Absent) => {
            tracing::debug!(survey_id = survey.id, hips_order = order, npix, "hips tile absent");
            store.mark_absent(order, npix)?;
            Ok(HipsTile::Absent)
        }
        Err(e) => {
            tracing::warn!(survey_id = survey.id, hips_order = order, npix, error = %format!("{e:#}"), "hips tile fetch failed");
            Err(ApiError::Internal(format!("{e:#}")))
        }
    }
}

/// Deletes a survey's whole tile store; returns the tile bytes it held.
pub fn clear_hips_cache(ctx: &ServiceContext, survey_id: &str) -> Result<u64, ApiError> {
    let survey = find_survey(survey_id)?;
    let store = HipsStore::new(&app_data_dir(ctx)?, survey);
    let bytes = store.clear()?;
    validated().lock().unwrap().remove(survey.id);
    tracing::info!(survey_id = survey.id, bytes, "hips cache cleared");
    Ok(bytes)
}
