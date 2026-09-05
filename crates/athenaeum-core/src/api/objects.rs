//! Resolving an object name against the bundled DSO catalog — the one place
//! both transports answer "does this name mean anything to the solver?".
//!
//! Why it exists: a frame with no RA/Dec in its header is solved blind, which
//! costs tens of seconds and often fails. If the frame says WHAT it points at,
//! `plate_solve::hints::apply_object_name_fallback` turns that name into a
//! position hint. So naming a target in the metadata editor is a real repair
//! for unsolvable frames — but only if the name is one the catalog carries.
//! This command lets the editor say so while the user types, instead of
//! letting them discover it after a failed solve.

use crate::plate_solve::dso_lookup::{DsoCatalog, ResolvedObject};
use crate::services::ServiceContext;

/// Resolve `name` to a catalog object, or `None` when nothing matches.
///
/// Accepts the spellings people actually type — `M31`, `m 31`, `Messier 31`
/// all reach `M 31` (see `dso_lookup::normalize_designation`). A popular name
/// the catalog does not carry ("Ghost Nebula") and anything that is not a
/// designation at all (a comet, a session id) resolve to `None`: the editor
/// then tells the user that plate solving will still have to search blind.
pub fn resolve_object_name(ctx: &ServiceContext, name: String) -> Option<ResolvedObject> {
    let catalog = load_catalog(ctx)?;
    catalog.find_by_designation(&name).map(|d| ResolvedObject {
        designation: d.designation.clone(),
        ra_deg: d.ra_deg,
        dec_deg: d.dec_deg,
        radius_deg: d.radius_deg,
    })
}

/// The process-wide catalog, loading it on first use.
///
/// Mirrors how the plate-solve batch obtains it: parsed once (~3 MB of JSON)
/// and shared, never per call.
#[cfg(all(feature = "render", feature = "solver"))]
fn load_catalog(ctx: &ServiceContext) -> Option<std::sync::Arc<DsoCatalog>> {
    if let Some(cat) = ctx.dso_catalog.read().ok()?.as_ref() {
        return Some(cat.clone());
    }
    let loaded = std::sync::Arc::new(DsoCatalog::load().ok()?);
    if let Ok(mut guard) = ctx.dso_catalog.write() {
        *guard = Some(loaded.clone());
    }
    Some(loaded)
}

/// Headless builds carry no shared catalog slot; parse on demand.
#[cfg(not(all(feature = "render", feature = "solver")))]
fn load_catalog(_ctx: &ServiceContext) -> Option<std::sync::Arc<DsoCatalog>> {
    DsoCatalog::load().ok().map(std::sync::Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plate_solve::dso_lookup::DsoCatalog;

    /// The editor's question, answered the way a person types it.
    #[test]
    fn resolves_the_spellings_people_type() {
        let cat = DsoCatalog::load().unwrap();
        for spelling in ["M31", "m 31", "Messier 31", "  M  031  "] {
            let hit = cat.find_by_designation(spelling).expect(spelling);
            assert_eq!(hit.designation, "M 31");
        }
    }

    /// And says no where it means no — a name the catalog lacks must not be
    /// dressed up as a hint the solver will never get.
    #[test]
    fn refuses_names_the_catalog_does_not_carry() {
        let cat = DsoCatalog::load().unwrap();
        for miss in ["Ghost Nebula", "C/2025 A6 (Lemmon)", "0884998404", ""] {
            assert!(cat.find_by_designation(miss).is_none(), "{miss}");
        }
    }
}
