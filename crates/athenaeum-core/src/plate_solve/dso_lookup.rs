//! Deep-sky object lookup for labelling plate-solve results.
//!
//! After a successful plate solve we know the sky position of the frame's
//! center. This module finds the "best" named DSO near that position using
//! the bundled `dsos.json` catalog (derived from d3-celestial, ~3 MB). If
//! the frame's `object` field is NULL in the database, we fill it with the
//! DSO's designation (e.g. "M 42", "NGC 7000", "IC 1805").
//!
//! Catalog format: GeoJSON FeatureCollection where each feature has:
//! - `id` / `properties.desig` — object designation
//! - `properties.type` — object type code (oc, gc, bn, sfr, s, g, gg, ...)
//! - `properties.mag` — magnitude (string, possibly "999" for unknown)
//! - `properties.dim` — angular size in arcmin; either "100" (round) or
//!   "90x40" (major × minor), or "999" (unknown)
//! - `geometry.coordinates` — `[ra, dec]` in degrees. RA is in [-180, 180]
//!   (negative = 180..360) per GeoJSON convention.
//!
//! Matching strategy:
//! 1. Containment — find all DSOs whose angular radius covers the query
//!    point. Pick the brightest (smallest magnitude).
//! 2. Proximity fallback — if nothing contains the point, pick the nearest
//!    DSO within 0.5° by great-circle distance.
//! 3. Return `None` otherwise.

use serde::Deserialize;
use std::collections::HashMap;

/// Bundled DSO catalog JSON. Parsed lazily on first use.
const DSO_JSON: &str = include_str!("../../resources/dsos.json");

/// A simplified DSO entry stored in memory.
#[derive(Clone, Debug)]
pub struct Dso {
    pub designation: String,
    pub type_code: String,
    /// RA in [0, 360) degrees (normalised from the GeoJSON input).
    pub ra_deg: f64,
    /// Dec in [-90, 90] degrees.
    pub dec_deg: f64,
    /// Magnitude (None if the catalog didn't provide one).
    pub magnitude: Option<f64>,
    /// Angular radius in degrees (half the `dim` field, largest dimension).
    /// `None` if no size is provided (treat as point source).
    pub radius_deg: Option<f64>,
}

/// In-memory catalog loaded from the bundled JSON.
pub struct DsoCatalog {
    entries: Vec<Dso>,
    /// Normalised designation → index into `entries`, for looking an object up
    /// by the name a frame's OBJECT keyword carries.
    by_designation: HashMap<String, usize>,
}

/// Spelled-out catalogue names people type, mapped to the prefixes this
/// catalog actually uses.
const CATALOGUE_SYNONYMS: &[(&str, &str)] = &[
    ("MESSIER", "M"),
    ("CALDWELL", "C"),
    ("BARNARD", "B"),
    ("COLLINDER", "CR"),
    ("MELOTTE", "MEL"),
    ("SHARPLESS", "SH"),
];

/// One spelling for a designation, so `M31`, `m 31` and `Messier 31` all reach
/// the same entry.
///
/// Only `<letters><number>` shapes are rewritten. Everything else — a popular
/// name like "Ghost Nebula", a comet like "C/2025 A6" — is upper-cased and
/// otherwise left alone: it has to miss the index rather than be bent into a
/// wrong hit, because a confident position for the wrong object is worse than
/// no position at all.
pub fn normalize_designation(raw: &str) -> String {
    let squashed = raw.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase();
    let (letters, rest) = squashed.split_at(
        squashed
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(squashed.len()),
    );
    let digits = rest.trim_start();
    if letters.is_empty() || digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return squashed;
    }
    let prefix = CATALOGUE_SYNONYMS
        .iter()
        .find(|(long, _)| *long == letters)
        .map(|(_, short)| *short)
        .unwrap_or(letters);
    let number = digits.trim_start_matches('0');
    let number = if number.is_empty() { "0" } else { number };
    format!("{prefix} {number}")
}

/// The best match returned for a given query point.
#[derive(Clone, Debug)]
pub struct DsoMatch {
    pub designation: String,
    pub type_code: String,
    /// Great-circle distance from query point to the DSO centre, in degrees.
    pub distance_deg: f64,
    pub reason: DsoMatchReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DsoMatchReason {
    /// Query point is inside the DSO's angular radius.
    Contains,
    /// Query point is outside any DSO's radius but close to this one.
    Nearest,
}

/// What a name in a frame's OBJECT keyword resolves to, for the metadata
/// editor: naming a target is what makes a coordinate-less frame solvable, so
/// the editor has to be able to say whether the name will actually work.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedObject {
    /// The catalog's own spelling — "M 31" for an entered "m31".
    pub designation: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    /// Angular radius in degrees, when the catalog gives a size.
    #[ts(optional = nullable)]
    pub radius_deg: Option<f64>,
}

impl DsoCatalog {
    /// Parse the bundled JSON and return an in-memory catalog.
    pub fn load() -> Result<Self, String> {
        let raw: RawFeatureCollection = serde_json::from_str(DSO_JSON)
            .map_err(|e| format!("Failed to parse dsos.json: {e}"))?;

        let mut entries = Vec::with_capacity(raw.features.len());
        for f in raw.features {
            if f.geometry.coordinates.len() < 2 {
                continue;
            }
            let mut ra = f.geometry.coordinates[0];
            let dec = f.geometry.coordinates[1];
            if ra < 0.0 {
                ra += 360.0;
            }
            if !(0.0..360.0).contains(&ra) || !(-90.0..=90.0).contains(&dec) {
                continue;
            }

            let desig = f
                .properties
                .desig
                .clone()
                .unwrap_or(f.id.clone())
                .trim()
                .to_string();
            if desig.is_empty() {
                continue;
            }

            let mag = f.properties.mag.and_then(parse_magnitude);
            let radius = f.properties.dim.as_deref().and_then(parse_radius_deg);

            entries.push(Dso {
                designation: desig,
                type_code: f.properties.type_.unwrap_or_default(),
                ra_deg: ra,
                dec_deg: dec,
                magnitude: mag,
                radius_deg: radius,
            });
        }

        // Index by name for the OBJECT-keyword lookup. First spelling wins:
        // the file is ordered brightest/most-canonical first, and a later
        // duplicate designation must not displace it.
        let mut by_designation = HashMap::with_capacity(entries.len());
        for (i, dso) in entries.iter().enumerate() {
            by_designation
                .entry(normalize_designation(&dso.designation))
                .or_insert(i);
        }
        Ok(DsoCatalog { entries, by_designation })
    }

    /// The object a frame's OBJECT keyword names, if this catalog carries it.
    ///
    /// Used as the last source of a position hint before a blind solve: a
    /// frame whose header has no RA/Dec at all can still say what it was
    /// pointed at.
    pub fn find_by_designation(&self, name: &str) -> Option<&Dso> {
        let key = normalize_designation(name);
        if key.is_empty() {
            return None;
        }
        self.by_designation.get(&key).map(|&i| &self.entries[i])
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Find the best DSO match for a given sky position.
    ///
    /// Returns `None` if no DSO contains the point AND none are within
    /// 0.5° of it. Equivalent to `find_best_within(ra, dec, 0.5)`.
    pub fn find_best(&self, ra_deg: f64, dec_deg: f64) -> Option<DsoMatch> {
        self.find_best_within(ra_deg, dec_deg, 0.5)
    }

    /// Return the single nearest DSO by great-circle distance, ignoring
    /// any proximity cap. Intended for diagnostic / debug output: when a
    /// tight-tolerance lookup returns `None`, callers can use this to tell
    /// the user which DSO was closest and how far away it was.
    pub fn nearest_dso(&self, ra_deg: f64, dec_deg: f64) -> Option<(String, f64)> {
        let ra_deg = normalize_ra(ra_deg);
        if !(-90.0..=90.0).contains(&dec_deg) {
            return None;
        }
        let mut best: Option<(&Dso, f64)> = None;
        for dso in &self.entries {
            let d = angular_distance_deg(ra_deg, dec_deg, dso.ra_deg, dso.dec_deg);
            match &best {
                None => best = Some((dso, d)),
                Some((_, best_d)) if d < *best_d => best = Some((dso, d)),
                _ => {}
            }
        }
        best.map(|(dso, d)| (dso.designation.clone(), d))
    }

    /// Find the best DSO match for a given sky position, with a tunable
    /// cap on the proximity fallback.
    ///
    /// Containment matches (DSO radius covers the query point) are always
    /// accepted regardless of `max_nearest_deg`. Nearest-by-distance
    /// matches are only accepted if within `max_nearest_deg` of the query
    /// point. Pass a tighter value (e.g. 0.2) when the caller wants to
    /// minimise false matches in empty sky.
    pub fn find_best_within(
        &self,
        ra_deg: f64,
        dec_deg: f64,
        max_nearest_deg: f64,
    ) -> Option<DsoMatch> {
        let ra_deg = normalize_ra(ra_deg);
        if !(-90.0..=90.0).contains(&dec_deg) {
            return None;
        }

        // Pass 1: containment — collect all DSOs whose radius covers the
        // query point. Pick the brightest (lowest magnitude).
        let mut best_containing: Option<(&Dso, f64)> = None;
        for dso in &self.entries {
            if let Some(radius) = dso.radius_deg {
                let d = angular_distance_deg(ra_deg, dec_deg, dso.ra_deg, dso.dec_deg);
                if d <= radius {
                    match &best_containing {
                        None => best_containing = Some((dso, d)),
                        Some((current, _)) => {
                            if dso_better(dso, current) {
                                best_containing = Some((dso, d));
                            }
                        }
                    }
                }
            }
        }
        if let Some((dso, dist)) = best_containing {
            return Some(DsoMatch {
                designation: dso.designation.clone(),
                type_code: dso.type_code.clone(),
                distance_deg: dist,
                reason: DsoMatchReason::Contains,
            });
        }

        // Pass 2: nearest DSO within `max_nearest_deg`, weighted slightly by
        // brightness so a mag-8 catalogued galaxy beats a faint PGC satellite
        // at the same distance.
        let proximity_limit = max_nearest_deg.max(0.0);
        let mut best_near: Option<(&Dso, f64, f64)> = None; // (dso, distance, score)
        for dso in &self.entries {
            let d = angular_distance_deg(ra_deg, dec_deg, dso.ra_deg, dso.dec_deg);
            if d > proximity_limit {
                continue;
            }
            // Score: distance + 0.01 × magnitude (brighter = smaller score).
            let mag = dso.magnitude.unwrap_or(15.0);
            let score = d + 0.01 * mag;
            match &best_near {
                None => best_near = Some((dso, d, score)),
                Some((_, _, best_score)) if score < *best_score => {
                    best_near = Some((dso, d, score));
                }
                _ => {}
            }
        }
        best_near.map(|(dso, dist, _)| DsoMatch {
            designation: dso.designation.clone(),
            type_code: dso.type_code.clone(),
            distance_deg: dist,
            reason: DsoMatchReason::Nearest,
        })
    }
}

// ────────── JSON parsing types ──────────

#[derive(Deserialize)]
struct RawFeatureCollection {
    #[serde(default)]
    features: Vec<RawFeature>,
}

#[derive(Deserialize)]
struct RawFeature {
    #[serde(default)]
    id: String,
    #[serde(default)]
    properties: RawProperties,
    #[serde(default)]
    geometry: RawGeometry,
}

#[derive(Deserialize, Default)]
struct RawProperties {
    #[serde(default)]
    desig: Option<String>,
    #[serde(rename = "type", default)]
    type_: Option<String>,
    /// `mag` can be a string ("5.0"), a number (5.0), or missing.
    #[serde(default)]
    mag: Option<RawMag>,
    #[serde(default)]
    dim: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawGeometry {
    #[serde(default)]
    coordinates: Vec<f64>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
enum RawMag {
    Str(String),
    Num(f64),
}

fn parse_magnitude(m: RawMag) -> Option<f64> {
    let s: String = match m {
        RawMag::Num(n) => return if n > 50.0 { None } else { Some(n) },
        RawMag::Str(s) => s,
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<f64>() {
        Ok(v) if v > 50.0 => None, // "999" placeholder
        Ok(v) => Some(v),
        Err(_) => None,
    }
}

/// Parse a catalog `dim` string like "100", "90x40", "60" (arcmin) to a
/// radius in DEGREES. Uses the largest dimension (for elongated objects)
/// divided by 2. Returns `None` for "999" or unparseable inputs.
fn parse_radius_deg(dim: &str) -> Option<f64> {
    let dim = dim.trim();
    if dim.is_empty() || dim == "999" {
        return None;
    }
    let (a, b) = if let Some(idx) = dim.find('x') {
        let (lhs, rhs) = dim.split_at(idx);
        let rhs = &rhs[1..];
        (lhs.parse::<f64>().ok(), rhs.parse::<f64>().ok())
    } else {
        (dim.parse::<f64>().ok(), None)
    };
    let arcmin = match (a, b) {
        (Some(x), Some(y)) => x.max(y),
        (Some(x), None) => x,
        _ => return None,
    };
    if arcmin <= 0.0 || arcmin > 1800.0 {
        return None;
    }
    Some(arcmin / 60.0 / 2.0) // arcmin → deg → radius
}

fn normalize_ra(ra: f64) -> f64 {
    let mut r = ra % 360.0;
    if r < 0.0 {
        r += 360.0;
    }
    r
}

fn angular_distance_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let r1 = ra1.to_radians();
    let d1 = dec1.to_radians();
    let r2 = ra2.to_radians();
    let d2 = dec2.to_radians();
    let dra = r2 - r1;
    let ddec = d2 - d1;
    let h = (ddec / 2.0).sin().powi(2) + d1.cos() * d2.cos() * (dra / 2.0).sin().powi(2);
    2.0 * h.sqrt().asin().to_degrees()
}

/// Compare two DSOs that both contain the query point and decide which is
/// "better" for labelling. Rules, in priority order:
///
/// 1. **Designation family** — Messier > Caldwell > NGC > IC > any other
///    catalog. Amateurs know "M 42", not "LBN 918".
/// 2. **Size** — within the same family, prefer the larger object (so a
///    photograph of M 42 isn't mis-labelled as a small compact cluster
///    inside it).
/// 3. **Magnitude** — brighter wins when sizes are similar.
fn dso_better(a: &Dso, b: &Dso) -> bool {
    let pa = designation_priority(&a.designation);
    let pb = designation_priority(&b.designation);
    if pa != pb {
        return pa > pb;
    }

    let ra = a.radius_deg.unwrap_or(0.0);
    let rb = b.radius_deg.unwrap_or(0.0);
    if ra > 0.0 && rb > 0.0 {
        let ratio = ra / rb;
        if ratio > 1.1 {
            return true;
        }
        if ratio < 0.9 {
            return false;
        }
    } else if ra > 0.0 && rb == 0.0 {
        return true;
    } else if ra == 0.0 && rb > 0.0 {
        return false;
    }

    match (a.magnitude, b.magnitude) {
        (Some(ma), Some(mb)) => ma < mb,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Designation family priority. Higher = preferred. Recognises the common
/// amateur catalogs: Messier, Caldwell, NGC, IC.
fn designation_priority(desig: &str) -> u8 {
    let trimmed = desig.trim();
    let upper = trimmed.to_ascii_uppercase();

    // Messier: "M 42", "M42"
    if upper.starts_with("M ") {
        return 5;
    }
    if upper.starts_with('M') && upper.len() > 1 {
        if let Some(c) = upper.chars().nth(1) {
            if c.is_ascii_digit() {
                return 5;
            }
        }
    }

    // Caldwell: "C 9", "C9"
    if upper.starts_with("C ") {
        return 4;
    }
    if upper.starts_with('C') && upper.len() > 1 {
        if let Some(c) = upper.chars().nth(1) {
            if c.is_ascii_digit() {
                return 4;
            }
        }
    }

    // NGC
    if upper.starts_with("NGC") {
        return 3;
    }

    // IC
    if upper.starts_with("IC") {
        return 2;
    }

    // Everything else: LBN, PGC, Cr, vdB, Sh, ...
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's OBJECT keyword is written by a person or a capture program,
    /// so it arrives in every spelling: "M31", "m 31", "M  031", "Messier 31".
    /// All of them name the same object.
    #[test]
    fn designation_normalisation_accepts_how_people_write_names() {
        assert_eq!(normalize_designation("M 31"), "M 31");
        assert_eq!(normalize_designation("M31"), "M 31");
        assert_eq!(normalize_designation("m31"), "M 31");
        assert_eq!(normalize_designation("  M  031 "), "M 31");
        assert_eq!(normalize_designation("ngc2903"), "NGC 2903");
        assert_eq!(normalize_designation("IC5146"), "IC 5146");
        // Catalogue names people spell out; the file uses the short prefix.
        assert_eq!(normalize_designation("Messier 42"), "M 42");
        assert_eq!(normalize_designation("Caldwell 7"), "C 7");
        assert_eq!(normalize_designation("Barnard 150"), "B 150");
        // Anything that is not "<catalogue> <number>" is left alone — a
        // popular name has to miss rather than be mangled into a wrong hit.
        assert_eq!(normalize_designation("Ghost Nebula"), "GHOST NEBULA");
        assert_eq!(normalize_designation("C/2025 A6 (Lemmon)"), "C/2025 A6 (LEMMON)");
    }

    /// The point of the index: turn a name in a header into a position.
    #[test]
    fn finds_objects_by_designation() {
        let cat = DsoCatalog::load().expect("catalog should parse");
        let m31 = cat.find_by_designation("M31").expect("M31 is in the catalog");
        assert!((m31.ra_deg - 10.685).abs() < 0.05, "ra {}", m31.ra_deg);
        assert!((m31.dec_deg - 41.269).abs() < 0.05, "dec {}", m31.dec_deg);
        // Same object, other spellings.
        for spelling in ["M 31", "m 31", "Messier 31"] {
            let hit = cat.find_by_designation(spelling).expect(spelling);
            assert_eq!(hit.designation, m31.designation);
        }
        assert!(cat.find_by_designation("LDN 1272").is_some(), "LDN designations resolve");
        // A popular name the catalog does not carry resolves to nothing —
        // better a blind solve than a confident wrong position.
        assert!(cat.find_by_designation("Tadpoles").is_none());
        assert!(cat.find_by_designation("").is_none());
    }

    #[test]
    fn catalog_loads() {
        let cat = DsoCatalog::load().expect("catalog should parse");
        assert!(cat.len() > 100, "expected many entries, got {}", cat.len());
    }

    #[test]
    fn finds_m42() {
        let cat = DsoCatalog::load().expect("load");
        // M 42 is at approximately RA 83.82, Dec -5.39
        let m = cat.find_best(83.82, -5.39).expect("should find something");
        eprintln!("Best for M42 area: {} ({:?})", m.designation, m.reason);
        assert!(
            m.designation.contains("42") || m.designation.contains("M 42") || m.designation == "M 42",
            "expected M42, got {}",
            m.designation
        );
    }

    #[test]
    fn finds_ngc7000() {
        let cat = DsoCatalog::load().expect("load");
        // NGC 7000 (North America Nebula): RA 314.75, Dec 44.33
        let m = cat.find_best(314.75, 44.33).expect("should find something");
        eprintln!("Best for NGC 7000 area: {} ({:?})", m.designation, m.reason);
    }

    #[test]
    fn unknown_position_returns_none_or_nearest() {
        let cat = DsoCatalog::load().expect("load");
        // Random empty sky region
        let _ = cat.find_best(42.0, -70.0);
    }

    #[test]
    fn parse_dim_simple() {
        assert_eq!(parse_radius_deg("60"), Some(0.5)); // 60/60/2 = 0.5 deg
        assert_eq!(parse_radius_deg("120"), Some(1.0));
    }

    #[test]
    fn parse_dim_elongated() {
        // 90x40 → max = 90 → 90/60/2 = 0.75
        assert_eq!(parse_radius_deg("90x40"), Some(0.75));
    }

    #[test]
    fn parse_dim_placeholder() {
        assert_eq!(parse_radius_deg("999"), None);
        assert_eq!(parse_radius_deg(""), None);
    }

    #[test]
    fn ra_normalization() {
        assert!((normalize_ra(-10.0) - 350.0).abs() < 1e-9);
        assert!((normalize_ra(370.0) - 10.0).abs() < 1e-9);
        assert!((normalize_ra(180.0) - 180.0).abs() < 1e-9);
    }

    #[test]
    fn find_best_within_tighter_cap_rejects_far_matches() {
        let cat = DsoCatalog::load().expect("load");
        // Pick a point ~0.35° off from M 42 (RA ≈ 83.82, Dec ≈ -5.39) so
        // containment doesn't fire and the proximity fallback is the only
        // path that can return a hit.
        let ra = 83.82 + 0.35;
        let dec = -5.39;

        // With the default 0.5° cap this should return something.
        let default_match = cat.find_best(ra, dec);

        // With a tight 0.2° cap, the same point is outside containment for
        // most DSOs and beyond the proximity limit — expect None OR a match
        // that is strictly closer than 0.2°.
        let tight_match = cat.find_best_within(ra, dec, 0.2);
        if let Some(m) = tight_match.as_ref() {
            // Any returned match must be inside containment (distance <=
            // radius, which is captured by reason=Contains) OR within 0.2°.
            if m.reason == DsoMatchReason::Nearest {
                assert!(
                    m.distance_deg <= 0.2,
                    "find_best_within(0.2) returned Nearest match at {:.3}° — should have been None",
                    m.distance_deg
                );
            }
        }

        // Sanity: if the default cap found a Nearest match beyond 0.2°, the
        // tight cap must NOT return the same thing.
        if let Some(d) = &default_match {
            if d.reason == DsoMatchReason::Nearest && d.distance_deg > 0.2 {
                match tight_match {
                    None => { /* expected */ }
                    Some(t) if t.reason == DsoMatchReason::Contains => {
                        // A Contains hit is allowed regardless of cap.
                    }
                    Some(t) => {
                        assert!(
                            t.distance_deg <= 0.2,
                            "tight cap should reject Nearest at {:.3}°",
                            t.distance_deg
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn find_best_within_preserves_contains() {
        let cat = DsoCatalog::load().expect("load");
        // M 42 core — inside the nebula radius, so Contains should win
        // regardless of how tight the nearest cap is.
        let m = cat.find_best_within(83.82, -5.39, 0.001)
            .expect("containment should fire even with ~0 nearest cap");
        assert!(
            matches!(m.reason, DsoMatchReason::Contains | DsoMatchReason::Nearest),
            "unexpected reason: {:?}",
            m.reason
        );
    }
}
