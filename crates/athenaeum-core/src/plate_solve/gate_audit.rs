//! Env-gated, observe-only acceptance telemetry for plate-solve gate
//! calibration. Zero overhead and zero behaviour change when
//! `ATHENAEUM_PLATESOLVE_GATE_CSV` is unset.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateStage { Hinted, ScaleCleared, FullBlind }

impl GateStage {
    pub fn as_str(self) -> &'static str {
        match self {
            GateStage::Hinted => "hinted",
            GateStage::ScaleCleared => "scale_cleared",
            GateStage::FullBlind => "full_blind",
        }
    }
    pub fn from_params(expected_scale: Option<f64>, disable_position_gate: bool) -> Self {
        if disable_position_gate { GateStage::FullBlind }
        else if expected_scale.is_none() { GateStage::ScaleCleared }
        else { GateStage::Hinted }
    }
}

#[derive(Clone, Debug)]
pub struct GateAuditRecord {
    pub filename: String,
    pub stage: GateStage,
    pub pass_idx: usize,
    pub accepted: bool,
    pub inliers: usize,
    pub expected_in_fov: usize,
    pub detected: usize,
    pub inlier_ratio: f64,
    pub rms_px: f64,
    pub rms_arcsec: f64,
    pub recovered_scale_arcsec: f64,
    pub header_scale_arcsec: Option<f64>,
    pub solved_ra: f64,
    pub solved_dec: f64,
    pub dist_from_header_deg: Option<f64>,
    pub required: usize,
}

pub fn csv_header() -> &'static str {
    "filename,stage,pass_idx,accepted,inliers,expected_in_fov,detected,\
inlier_ratio,rms_px,rms_arcsec,recovered_scale_arcsec,header_scale_arcsec,\
solved_ra,solved_dec,dist_from_header_deg,required"
}

fn csv_quote(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.6}")).unwrap_or_default()
}

impl GateAuditRecord {
    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6},{},{:.6},{:.6},{},{}",
            csv_quote(&self.filename),
            self.stage.as_str(),
            self.pass_idx,
            self.accepted,
            self.inliers,
            self.expected_in_fov,
            self.detected,
            self.inlier_ratio,
            self.rms_px,
            self.rms_arcsec,
            self.recovered_scale_arcsec,
            opt(self.header_scale_arcsec),
            self.solved_ra,
            self.solved_dec,
            opt(self.dist_from_header_deg),
            self.required,
        )
    }
}

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var("ATHENAEUM_PLATESOLVE_GATE_CSV").ok()?;
        // Atomic create-or-append: `create_new` is O_CREAT|O_EXCL, so the
        // header is written exactly once even if another process/run created
        // the file between a check and the open (no TOCTOU duplicate header).
        let f = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut nf) => {
                let _ = writeln!(nf, "{}", csv_header());
                nf
            }
            Err(_) => OpenOptions::new().append(true).open(&path).ok()?,
        };
        Some(Mutex::new(f))
    })
    .as_ref()
}

pub fn enabled() -> bool { sink().is_some() }

pub fn record(rec: &GateAuditRecord) {
    let Some(m) = sink() else { return };
    if let Ok(mut f) = m.lock() {
        let _ = writeln!(f, "{}", rec.to_csv_row());
    }
}

/// Build and append one gate-audit row from already-resolved scalars.
/// Single field-population site so the per-pass and final-gate callers
/// cannot drift. No-op unless `enabled()` (callers still guard with
/// `if gate_audit::enabled()` to skip the pixel_to_sky/scalar prep).
#[allow(clippy::too_many_arguments)]
pub fn record_event(
    filename: &str,
    stage: GateStage,
    pass_idx: usize,
    accepted: bool,
    inliers: usize,
    expected_in_fov: usize,
    detected: usize,
    inlier_ratio: f64,
    rms_px: f64,
    rms_arcsec: f64,
    recovered_scale_arcsec: f64,
    header_scale_arcsec: Option<f64>,
    solved_ra: f64,
    solved_dec: f64,
    ra_hint: Option<f64>,
    dec_hint: Option<f64>,
    required: usize,
) {
    let dist_from_header_deg = match (ra_hint, dec_hint) {
        (Some(h_ra), Some(h_dec)) => {
            let (d1, d2) = (solved_dec.to_radians(), h_dec.to_radians());
            let dra = (h_ra - solved_ra).to_radians();
            let c = (d1.sin() * d2.sin() + d1.cos() * d2.cos() * dra.cos())
                .clamp(-1.0, 1.0);
            Some(c.acos().to_degrees())
        }
        _ => None,
    };
    record(&GateAuditRecord {
        filename: filename.to_string(),
        stage,
        pass_idx,
        accepted,
        inliers,
        expected_in_fov,
        detected,
        inlier_ratio,
        rms_px,
        rms_arcsec,
        recovered_scale_arcsec,
        header_scale_arcsec,
        solved_ra,
        solved_dec,
        dist_from_header_deg,
        required,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::Not;

    #[test]
    fn csv_row_matches_header_arity_and_values() {
        let rec = GateAuditRecord {
            filename: "a,b.fits".into(),
            stage: GateStage::FullBlind,
            pass_idx: 2,
            accepted: false,
            inliers: 21,
            expected_in_fov: 3500,
            detected: 600,
            inlier_ratio: 0.006,
            rms_px: 7.4,
            rms_arcsec: 6.5,
            recovered_scale_arcsec: 0.88,
            header_scale_arcsec: Some(0.55),
            solved_ra: 200.0,
            solved_dec: -30.0,
            dist_from_header_deg: Some(95.2),
            required: 20,
        };
        let header_cols = csv_header().split(',').count();
        let row = rec.to_csv_row();
        let row_cols = split_csv_row(&row).len();
        assert_eq!(header_cols, row_cols, "header/row column mismatch");
        assert!(row.contains("full_blind"));
        assert!(row.contains("\"a,b.fits\""));
        assert!(row.ends_with('\n').not());

        // None-valued Option<f64> fields must render as an empty column.
        let none_rec = GateAuditRecord {
            header_scale_arcsec: None,
            dist_from_header_deg: None,
            ..rec.clone()
        };
        let cols = split_csv_row(&none_rec.to_csv_row());
        assert_eq!(cols.len(), csv_header().split(',').count());
        // header_scale_arcsec is column index 11, dist_from_header_deg is 14.
        assert_eq!(cols[11], "");
        assert_eq!(cols[14], "");
    }

    // Test-only minimal splitter: handles quoted commas but NOT escaped `""`
    // inside quoted fields (not exercised by current cases).
    fn split_csv_row(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut q = false;
        for c in s.chars() {
            match c {
                '"' => q = !q,
                ',' if !q => { out.push(std::mem::take(&mut cur)); }
                _ => cur.push(c),
            }
        }
        out.push(cur);
        out
    }
}
