//! The project's canonical grid (spec 2026-10-02 §6, plan 6b-2 C16): the
//! output geometry every processor's project masters land on. The hub's
//! `projects.canonical_grid` is the source of truth; this is its typed,
//! normalized form. Ungated: the cache (`db::collab`) and the feed read it.
//!
//! Orientation: sky-standard, north up, east left, `det(CD) < 0`
//! (`collab::grid_geometry::grid_wcs`). Every solve in the owner's catalog has
//! `det(CD) > 0`, so a grid master is the mirror image of a raw frame,
//! which is correct on the sky; no `ROWORDER` of a source frame describes it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalGrid {
    pub scale_arcsec: f64,
    pub width_px: i64,
    pub height_px: i64,
    pub crval1: f64,
    pub crval2: f64,
    pub rotation_deg: f64,
}

/// The hub's refusal text for a grid outside its bounds (athenaeum-hub
/// `put_grid`), verbatim, so a local refusal reads as the hub's would.
pub const GRID_INVALID_MSG: &str = "grid needs scaleArcsec in (0,100], widthPx/heightPx in [64,65536], crval1 in [0,360), crval2 in [-90,90], finite rotationDeg";

fn unsigned_zero(x: f64) -> f64 {
    if x == 0.0 {
        0.0
    } else {
        x
    }
}

impl CanonicalGrid {
    /// The hub's own bounds, verbatim.
    pub fn validate(&self) -> Result<(), &'static str> {
        let ok = self.scale_arcsec.is_finite()
            && self.scale_arcsec > 0.0
            && self.scale_arcsec <= 100.0
            && (64..=65536).contains(&self.width_px)
            && (64..=65536).contains(&self.height_px)
            && (0.0..360.0).contains(&self.crval1)
            && (-90.0..=90.0).contains(&self.crval2)
            && self.rotation_deg.is_finite();
        if ok {
            Ok(())
        } else {
            Err(GRID_INVALID_MSG)
        }
    }

    /// One spelling per grid, applied before the PUT and before hashing:
    /// `-0.0` becomes `0.0` (Postgres `numeric` loses the sign), the rotation
    /// goes into `(-180, 180]` (the hub only checks it is finite), `crval1`
    /// into `[0, 360)`.
    pub fn normalized(&self) -> CanonicalGrid {
        let mut rotation = self.rotation_deg % 360.0;
        if rotation <= -180.0 {
            rotation += 360.0;
        } else if rotation > 180.0 {
            rotation -= 360.0;
        }
        let mut crval1 = self.crval1.rem_euclid(360.0);
        if crval1 >= 360.0 {
            crval1 -= 360.0;
        }
        CanonicalGrid {
            scale_arcsec: unsigned_zero(self.scale_arcsec),
            width_px: self.width_px,
            height_px: self.height_px,
            crval1: unsigned_zero(crval1),
            crval2: unsigned_zero(self.crval2),
            rotation_deg: unsigned_zero(rotation),
        }
    }

    /// The canonical JSON: the typed struct through `serde_json::Value`, so
    /// keys come out sorted (this workspace's `serde_json` has no
    /// `preserve_order`) and numbers in their shortest round-trip form.
    pub fn canonical_json(&self) -> String {
        let value = serde_json::to_value(self).expect("a grid serializes");
        serde_json::to_string(&value).expect("a json value serializes")
    }

    /// `ATH_GRID`: XXH3-64 of [`Self::canonical_json`], 16 hex digits.
    /// Hash a normalized grid; the hub's jsonb echo is parsed, never hashed.
    pub fn hash(&self) -> String {
        format!(
            "{:016x}",
            xxhash_rust::xxh3::xxh3_64(self.canonical_json().as_bytes())
        )
    }

    /// Parse a stored or echoed grid (any key order, numbers as stored) and
    /// normalize it.
    pub fn from_value(v: &serde_json::Value) -> Result<CanonicalGrid, serde_json::Error> {
        Ok(serde_json::from_value::<CanonicalGrid>(v.clone())?.normalized())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> CanonicalGrid {
        CanonicalGrid {
            scale_arcsec: 1.87,
            width_px: 3072,
            height_px: 2048,
            crval1: 10.68,
            crval2: 41.27,
            rotation_deg: 0.0,
        }
    }

    /// Plan 6b-2 C16: the canonical form is the typed struct's JSON with
    /// sorted keys, and the hash is XXH3-64 of exactly that, 16 hex digits.
    #[test]
    fn the_canonical_json_and_the_hash_are_pinned() {
        assert_eq!(
            grid().canonical_json(),
            r#"{"crval1":10.68,"crval2":41.27,"heightPx":2048,"rotationDeg":0.0,"scaleArcsec":1.87,"widthPx":3072}"#
        );
        assert_eq!(
            grid().hash(),
            format!(
                "{:016x}",
                xxhash_rust::xxh3::xxh3_64(grid().canonical_json().as_bytes())
            )
        );
        assert_eq!(grid().hash().len(), 16);
    }

    /// The hub stores jsonb: keys come back reordered and a whole float may
    /// come back as an integer. The hash must not see either.
    #[test]
    fn a_jsonb_echo_hashes_like_the_grid_it_echoes() {
        let echo: serde_json::Value = serde_json::from_str(
            r#"{"widthPx":3072,"crval1":10.68,"heightPx":2048,"crval2":41.27,"scaleArcsec":1.87,"rotationDeg":0}"#,
        )
        .unwrap();
        assert_eq!(
            CanonicalGrid::from_value(&echo).unwrap().hash(),
            grid().hash()
        );
    }

    /// Postgres `numeric` drops the sign of −0.0; normalization drops it first.
    #[test]
    fn negative_zero_hashes_like_zero() {
        let negative = CanonicalGrid {
            rotation_deg: -0.0,
            crval2: -0.0,
            ..grid()
        };
        let zero = CanonicalGrid {
            rotation_deg: 0.0,
            crval2: 0.0,
            ..grid()
        };
        assert_eq!(negative.normalized().hash(), zero.normalized().hash());
    }

    #[test]
    fn normalization_puts_rotation_and_crval1_in_their_ranges() {
        for (rotation, expected) in [
            (180.0, 180.0),
            (-180.0, 180.0),
            (540.0, 180.0),
            (-190.0, 170.0),
            (350.0, -10.0),
            (0.0, 0.0),
        ] {
            let g = CanonicalGrid {
                rotation_deg: rotation,
                ..grid()
            }
            .normalized();
            assert_eq!(g.rotation_deg, expected, "{rotation}");
        }
        for (crval1, expected) in [(360.0, 0.0), (-10.0, 350.0), (370.5, 10.5), (0.0, 0.0)] {
            let g = CanonicalGrid { crval1, ..grid() }.normalized();
            assert_eq!(g.crval1, expected, "{crval1}");
        }
    }

    /// The hub's own bounds, verbatim (athenaeum-hub `put_grid`).
    #[test]
    fn validation_mirrors_the_hub() {
        assert_eq!(grid().validate(), Ok(()));
        for bad in [
            CanonicalGrid {
                scale_arcsec: 0.0,
                ..grid()
            },
            CanonicalGrid {
                scale_arcsec: 100.0001,
                ..grid()
            },
            CanonicalGrid {
                width_px: 63,
                ..grid()
            },
            CanonicalGrid {
                height_px: 65537,
                ..grid()
            },
            CanonicalGrid {
                crval1: 360.0,
                ..grid()
            },
            CanonicalGrid {
                crval2: 90.1,
                ..grid()
            },
            CanonicalGrid {
                rotation_deg: f64::NAN,
                ..grid()
            },
        ] {
            assert_eq!(bad.validate(), Err(GRID_INVALID_MSG), "{bad:?}");
        }
        for ok in [
            CanonicalGrid {
                scale_arcsec: 100.0,
                ..grid()
            },
            CanonicalGrid {
                width_px: 64,
                height_px: 65536,
                ..grid()
            },
            CanonicalGrid {
                crval2: -90.0,
                ..grid()
            },
        ] {
            assert_eq!(ok.validate(), Ok(()), "{ok:?}");
        }
    }
}
