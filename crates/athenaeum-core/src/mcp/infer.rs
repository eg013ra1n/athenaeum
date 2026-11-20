//! Inference of missing metadata (MCP plan Task 17, §6.3): what a frame's folder, its file name and the frames beside
//! it say about a field the header left empty, and what the user said themselves.
//!
//! Pure: no catalog, no disk. A caller hands the frames that miss a field and the frames of the same folders that
//! have it set; the answer is a proposal with the evidence it rests on, or the reason there is none.
//!
//! Only a frame that misses the field is ever proposed for (the Missing Metadata view's own test, `missing_flags`;
//! the object only for lights, as that view does). The camera and the object are taken from the other frames of the
//! same folder (D9): at least three have the field set and all agree; for the object only lights count. A frame type comes from the nearest folder of
//! its path that is named for one; a date from the file name, read as UTC because the camera clock's zone is unknown.

use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog_query::{missing_flags, ScopedFrame};
use crate::models::{Frame, ImageType};

/// Frames of one folder that must have a field set before the folder speaks for it.
const NEIGHBOURS_NEEDED: usize = 3;
/// The evidence of a value the user gave.
pub const BY_INSTRUCTION: &str = "by your instruction";
/// What a master's evidence says besides the folder or the user's word.
const IS_A_MASTER: &str = "the file is a master";
/// The widest a value the user gives may be (a FITS card holds 68 characters).
const VALUE_MAX_CHARS: usize = 120;

/// What an assistant may ask to fill. Coordinates are the plate-solve plan's.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars")]
pub enum FillField {
    Object,
    Camera,
    Date,
    FrameType,
}

impl FillField {
    pub const ALL: [FillField; 4] = [FillField::Object, FillField::Camera, FillField::Date, FillField::FrameType];

    /// The wire word.
    pub fn as_str(self) -> &'static str {
        match self {
            FillField::Object => "object",
            FillField::Camera => "camera",
            FillField::Date => "date",
            FillField::FrameType => "frame_type",
        }
    }

    /// The word a person reads.
    pub fn label(self) -> &'static str {
        match self {
            FillField::Object => "Object",
            FillField::Camera => "Camera",
            FillField::Date => "Date",
            FillField::FrameType => "Frame type",
        }
    }

    /// The header keyword the value belongs to.
    pub fn header_key(self) -> &'static str {
        match self {
            FillField::Object => "OBJECT",
            FillField::Camera => "INSTRUME",
            FillField::Date => "DATE-OBS",
            FillField::FrameType => "IMAGETYP",
        }
    }

    /// Whether the frame misses this field, by the Missing Metadata view's own rule (the object: lights only).
    pub fn is_missing(self, sf: &ScopedFrame) -> bool {
        let Some(frame) = sf.file.frame.as_ref() else { return false };
        if frame.id.is_none() {
            return false;
        }
        let flags = missing_flags(frame);
        match self {
            FillField::Object => flags.object && frame.imagetyp == Some(ImageType::Light),
            FillField::Camera => flags.camera,
            FillField::Date => flags.date,
            FillField::FrameType => flags.frame_type,
        }
    }
}

/// A value for one field of one frame, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub frame_id: i64,
    pub field: FillField,
    /// The value as the plan keeps it. A date is `YYYY-MM-DDTHH:MM:SS` (UTC); a frame type is `Light`, `Dark`,
    /// `Flat`, `Bias` or `DarkFlat`, or for a frame that is a master the same with `Master` before it.
    pub value: String,
    pub evidence: String,
}

/// A frame that misses a field and gets no value, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoProposal {
    pub frame_id: i64,
    pub field: FillField,
    pub why: String,
}

/// What the user said: this field is this value, for the frames in scope.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(crate = "rmcp::schemars")]
pub struct UserStatement {
    pub field: FillField,
    /// The value, as the user said it: a target name or camera name as it should be written, a frame type
    /// (`light`, `dark`, `flat`, `bias`, `darkflat`), or a date with its time (`YYYY-MM-DD HH:MM:SS` or
    /// `YYYY-MM-DDTHH:MM:SS`, in UTC): the night a frame belongs to is decided by its time.
    pub value: String,
}

fn frame_of(sf: &ScopedFrame) -> Option<&Frame> {
    sf.file.frame.as_ref()
}

fn frame_id_of(sf: &ScopedFrame) -> Option<i64> {
    frame_of(sf).and_then(|f| f.id)
}

// ---- the frame type, from the folders of the path ----

/// The frame type a folder name stands for.
fn kind_of_folder(name: &str) -> Option<&'static str> {
    match name.to_ascii_lowercase().as_str() {
        "light" | "lights" => Some("Light"),
        "dark" | "darks" => Some("Dark"),
        "flat" | "flats" => Some("Flat"),
        "bias" | "biases" | "offset" | "offsets" => Some("Bias"),
        "darkflat" | "darkflats" | "flatdark" | "flatdarks" => Some("DarkFlat"),
        _ => None,
    }
}

/// The frame type a frame of that kind is when the file is a master: its own master kind, so the type keeps
/// `is_master` set (the catalog derives the flag from the type text).
fn master_of(kind: &str) -> String {
    format!("Master{kind}")
}

/// A frame type for `frame`: `kind`, or the master kind when the frame is a master.
fn kind_for(frame: &Frame, kind: &str) -> (String, bool) {
    if frame.is_master { (master_of(kind), true) } else { (kind.to_string(), false) }
}

/// The frame type of the nearest folder of `folder` (a catalog path, either separator) named for one, with that
/// folder as the path spells it.
fn frame_type_from_folder(folder: &str) -> Option<(&'static str, String)> {
    folder
        .rsplit(['/', '\\'])
        .filter(|c| !c.is_empty())
        .find_map(|c| kind_of_folder(c).map(|k| (k, c.to_string())))
}

// ---- the date, from the file name ----

fn digits(b: &[u8]) -> Option<u32> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    b.iter().try_fold(0u32, |n, d| n.checked_mul(10)?.checked_add(u32::from(d - b'0')))
}

/// `YYYY-MM-DD[_T ]HH[-_:]MM[-_:]SS` at the start of `b`.
fn dashed(b: &[u8]) -> Option<(u32, u32, u32, u32, u32, u32)> {
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'_' | b'T' | b' ') {
        return None;
    }
    if !matches!(b[13], b'-' | b'_' | b':') || !matches!(b[16], b'-' | b'_' | b':') {
        return None;
    }
    Some((digits(&b[0..4])?, digits(&b[5..7])?, digits(&b[8..10])?, digits(&b[11..13])?, digits(&b[14..16])?, digits(&b[17..19])?))
}

/// `YYYYMMDD[-_]HHMMSS` at the start of `b`.
fn compact(b: &[u8]) -> Option<(u32, u32, u32, u32, u32, u32)> {
    if b.len() < 15 || !matches!(b[8], b'-' | b'_') {
        return None;
    }
    Some((digits(&b[0..4])?, digits(&b[4..6])?, digits(&b[6..8])?, digits(&b[9..11])?, digits(&b[11..13])?, digits(&b[13..15])?))
}

fn valid(parts: (u32, u32, u32, u32, u32, u32)) -> Option<String> {
    let (y, mo, d, h, mi, s) = parts;
    if !(1990..=2100).contains(&y) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, mo, d)?;
    Some(format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}"))
}

/// The first date and time the file name holds, as `YYYY-MM-DDTHH:MM:SS`. A run of digits that goes on before or
/// after the pattern is not one.
fn date_from_file_name(name: &str) -> Option<String> {
    let b = name.as_bytes();
    for start in 0..b.len() {
        if start > 0 && b[start - 1].is_ascii_digit() {
            continue;
        }
        let rest = &b[start..];
        for (len, parts) in [(19, dashed(rest)), (15, compact(rest))] {
            let Some(parts) = parts else { continue };
            if rest.get(len).is_some_and(u8::is_ascii_digit) {
                continue;
            }
            if let Some(text) = valid(parts) {
                return Some(text);
            }
        }
    }
    None
}

// ---- camera and object, from the frames beside it ----

fn value_of(f: &Frame, field: FillField) -> Option<&str> {
    let text = match field {
        FillField::Camera => f.instrume.as_deref(),
        FillField::Object => f.object.as_deref(),
        FillField::Date | FillField::FrameType => None,
    };
    text.map(str::trim).filter(|v| !v.is_empty())
}

/// Per folder: each value of `field` the frames there have, with how many.
fn tally<'a>(neighbours: &'a [ScopedFrame], field: FillField) -> HashMap<&'a str, BTreeMap<String, usize>> {
    let mut out: HashMap<&str, BTreeMap<String, usize>> = HashMap::new();
    for sf in neighbours {
        let Some(frame) = frame_of(sf) else { continue };
        // A flat's OBJECT names the panel or the flat field, not the target: only lights speak for an object.
        if field == FillField::Object && frame.imagetyp != Some(ImageType::Light) {
            continue;
        }
        let Some(value) = value_of(frame, field) else { continue };
        *out.entry(sf.folder.as_str()).or_default().entry(value.to_string()).or_default() += 1;
    }
    out
}

/// What a folder's tally says: the one value its frames agree on with the evidence, or why there is none.
fn from_neighbours(field: FillField, counts: Option<&BTreeMap<String, usize>>) -> Result<(String, String), String> {
    let total: usize = counts.map_or(0, |c| c.values().sum());
    let Some(counts) = counts.filter(|_| total >= NEIGHBOURS_NEEDED) else {
        return Err(format!("fewer than {NEIGHBOURS_NEEDED} frames in this folder have it"));
    };
    let mut ranked: Vec<(&String, &usize)> = counts.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    if let [(value, _)] = ranked.as_slice() {
        return Ok(((*value).clone(), format!("{total} frames in this folder have {} = '{value}'", field.header_key())));
    }
    let parts: Vec<String> = ranked.iter().map(|(v, n)| format!("{n} '{v}'")).collect();
    Err(format!("frames in this folder disagree: {}", parts.join(", ")))
}

// ---- the entry points ----

/// A value for each of `fields` of each frame in `missing` that misses it, from what the frames' folders, file names
/// and the same-folder `neighbours` (frames with the field set) say. A frame that misses a field and has no evidence
/// for it is a [`NoProposal`] with the reason. The order is by field (as `fields` is sorted), then `missing`'s.
pub fn infer(
    missing: &[ScopedFrame],
    neighbours: &[ScopedFrame],
    fields: &[FillField],
) -> (Vec<Proposal>, Vec<NoProposal>) {
    let mut wanted: Vec<FillField> = fields.to_vec();
    wanted.sort();
    wanted.dedup();
    let mut proposals = Vec::new();
    let mut none = Vec::new();
    for field in wanted {
        let tallies = matches!(field, FillField::Object | FillField::Camera).then(|| tally(neighbours, field));
        for sf in missing.iter().filter(|sf| field.is_missing(sf)) {
            let Some(frame_id) = frame_id_of(sf) else { continue };
            let found: Result<(String, String), String> = match field {
                FillField::FrameType => frame_type_from_folder(&sf.folder)
                    .map(|(kind, folder)| {
                        let (value, master) = frame_of(sf).map_or((kind.to_string(), false), |f| kind_for(f, kind));
                        let evidence = format!("folder '{folder}' in its path");
                        (value, if master { format!("{evidence}; {IS_A_MASTER}") } else { evidence })
                    })
                    .ok_or_else(|| {
                        "no folder in its path is named for a frame type (Lights, Darks, Flats, Bias, DarkFlats)".to_string()
                    }),
                FillField::Date => {
                    let name = &sf.file.file.filename;
                    date_from_file_name(name)
                        .map(|text| {
                            (
                                text,
                                format!(
                                    "from the file name '{name}'; the camera clock's time zone is unknown, taken as UTC"
                                ),
                            )
                        })
                        .ok_or_else(|| format!("the file name '{name}' holds no date and time it can read"))
                }
                FillField::Object | FillField::Camera => {
                    let counts = tallies.as_ref().and_then(|t| t.get(sf.folder.as_str()));
                    from_neighbours(field, counts)
                }
            };
            match found {
                Ok((value, evidence)) => proposals.push(Proposal { frame_id, field, value, evidence }),
                Err(why) => none.push(NoProposal { frame_id, field, why }),
            }
        }
    }
    (proposals, none)
}

/// `value` for `field` on every frame of `frames` that misses it: the user's own word, for the frames in scope only.
/// `value` is what [`normalize_statement`] returned.
pub fn by_instruction(frames: &[ScopedFrame], field: FillField, value: &str) -> Vec<Proposal> {
    frames
        .iter()
        .filter(|sf| field.is_missing(sf))
        .filter_map(|sf| {
            let frame_id = frame_id_of(sf)?;
            // A frame type said for a master is the master kind of it (see `master_of`).
            let master = field == FillField::FrameType && frame_of(sf).is_some_and(|f| f.is_master);
            let (value, evidence) = if master {
                (master_of(value), format!("{BY_INSTRUCTION}; {IS_A_MASTER}"))
            } else {
                (value.to_string(), BY_INSTRUCTION.to_string())
            };
            Some(Proposal { frame_id, field, value, evidence })
        })
        .collect()
}

/// The user's value in the form a proposal carries it, or the sentence that says what is wrong with it.
pub fn normalize_statement(field: FillField, value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("the statement gives no value for {}", field.as_str()));
    }
    match field {
        FillField::Object | FillField::Camera => {
            if value.chars().count() > VALUE_MAX_CHARS || value.chars().any(char::is_control) {
                return Err(format!(
                    "'{value}' is not a usable {}: at most {VALUE_MAX_CHARS} characters, no control characters",
                    field.as_str()
                ));
            }
            Ok(value.to_string())
        }
        FillField::FrameType => {
            let lower = value.to_lowercase();
            let kind = [Some(lower.as_str()), lower.strip_suffix('s')]
                .into_iter()
                .flatten()
                .filter_map(ImageType::from_str)
                .find_map(|k| match k {
                    ImageType::Light => Some("Light"),
                    ImageType::Dark => Some("Dark"),
                    ImageType::Flat => Some("Flat"),
                    ImageType::Bias => Some("Bias"),
                    ImageType::DarkFlat => Some("DarkFlat"),
                    _ => None,
                });
            kind.map(str::to_string)
                .ok_or_else(|| format!("'{value}' is not a frame type to set; use light, dark, flat, bias or darkflat"))
        }
        FillField::Date if is_date_only(value) => Err(format!(
            "'{value}' has no time: give the time too (YYYY-MM-DD HH:MM:SS): the night a frame belongs to is decided by its time"
        )),
        FillField::Date => normalize_date(value).ok_or_else(|| {
            format!("'{value}' is not a date to set; use YYYY-MM-DD HH:MM:SS (UTC), from 1990 to 2100")
        }),
    }
}

/// `YYYY-MM-DD` and nothing after it.
fn is_date_only(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && digits(&b[0..4]).is_some()
        && digits(&b[5..7]).is_some()
        && digits(&b[8..10]).is_some()
}

fn normalize_date(value: &str) -> Option<String> {
    let b = value.as_bytes();
    let parts = dashed(b)?;
    if b.len() != 19 {
        return None;
    }
    valid(parts)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::models::{File, FileFormat, FileWithFrame};
    use crate::scope::FrameState;

    /// A frame in `folder` named `name`; `set` fills the frame's fields.
    fn frame(id: i64, folder: &str, name: &str, set: impl FnOnce(&mut Frame)) -> ScopedFrame {
        let sep = if cfg!(windows) { "\\" } else { "/" };
        let root = if cfg!(windows) { "C:\\data" } else { "/data" };
        let folder = format!("{root}{sep}{}", folder.replace('/', sep));
        let path = format!("{folder}{sep}{name}");
        let mut f = Frame { id: Some(id), file_id: id, ..Default::default() };
        set(&mut f);
        ScopedFrame {
            file: FileWithFrame {
                file: File {
                    id: Some(id),
                    path,
                    filename: name.to_string(),
                    size: 1,
                    modified_at: Utc::now(),
                    format: FileFormat::FITS,
                    created_at: Utc::now(),
                    content_hash: None,
                    archived_in_operation: None,
                    archive_zip_path: None,
                    archive_path_in_zip: None,
                    uuid: None,
                    updated_at: None,
                },
                frame: Some(f),
            },
            folder,
            night: None,
            state: FrameState::Catalog,
            frame_sets: Vec::new(),
            scan_root: None,
        }
    }

    fn light(id: i64, folder: &str, name: &str) -> ScopedFrame {
        frame(id, folder, name, |f| f.imagetyp = Some(ImageType::Light))
    }

    /// `n` frames of `folder` that have `value` as their camera.
    fn with_camera(first_id: i64, n: usize, folder: &str, value: &str) -> Vec<ScopedFrame> {
        (0..n)
            .map(|k| frame(first_id + k as i64, folder, &format!("c{k}-{first_id}.fits"), |f| f.instrume = Some(value.to_string())))
            .collect()
    }

    fn with_object(first_id: i64, n: usize, folder: &str, value: &str) -> Vec<ScopedFrame> {
        (0..n)
            .map(|k| {
                frame(first_id + k as i64, folder, &format!("o{k}-{first_id}.fits"), |f| {
                    f.object = Some(value.to_string());
                    f.imagetyp = Some(ImageType::Light);
                })
            })
            .collect()
    }

    #[test]
    fn frame_type_from_folder() {
        let missing = vec![
            frame(1, "M31/Flats", "a.fits", |_| {}),
            frame(2, "M31/lights", "b.fits", |_| {}),
            frame(3, "M31/DARKS/sub", "c.fits", |_| {}),
            frame(4, "calib/Biases", "d.fits", |_| {}),
            frame(5, "calib/Offset", "e.fits", |_| {}),
            frame(6, "calib/DarkFlats", "f.fits", |_| {}),
            frame(7, "calib/FlatDark", "g.fits", |_| {}),
            // The nearest folder wins, not the first one in the path.
            frame(8, "Flats/M31/Lights", "h.fits", |_| {}),
            // A name that merely contains a word is not a folder named for it.
            frame(9, "M31/Flats-old", "i.fits", |_| {}),
            // A frame that has a type is not proposed for.
            frame(10, "M31/Flats", "j.fits", |f| f.imagetyp = Some(ImageType::Light)),
        ];
        let (got, none) = infer(&missing, &[], &[FillField::FrameType]);
        let by_id: BTreeMap<i64, &Proposal> = got.iter().map(|p| (p.frame_id, p)).collect();
        let kinds: Vec<(i64, &str)> = by_id.iter().map(|(id, p)| (*id, p.value.as_str())).collect();
        assert_eq!(
            kinds,
            vec![(1, "Flat"), (2, "Light"), (3, "Dark"), (4, "Bias"), (5, "Bias"), (6, "DarkFlat"), (7, "DarkFlat"), (8, "Light")]
        );
        assert_eq!(by_id[&1].evidence, "folder 'Flats' in its path");
        assert_eq!(by_id[&2].evidence, "folder 'lights' in its path");
        assert!(got.iter().all(|p| p.field == FillField::FrameType));
        assert_eq!(none.len(), 1, "{none:?}");
        assert_eq!(none[0].frame_id, 9);
        assert!(none[0].why.contains("no folder in its path is named for a frame type"), "{}", none[0].why);
    }

    #[test]
    fn date_from_file_name_with_evidence() {
        let name = "M31_2024-10-05_22-15-03_0001.fits";
        let missing = vec![light(1, "M31", name)];
        let (got, none) = infer(&missing, &[], &[FillField::Date]);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, "2024-10-05T22:15:03");
        assert_eq!(
            got[0].evidence,
            format!("from the file name '{name}'; the camera clock's time zone is unknown, taken as UTC")
        );
        assert!(got[0].evidence.contains("taken as UTC"));

        // Every spelling the rule names, and what it refuses.
        let reads = |n: &str| date_from_file_name(n);
        assert_eq!(reads("L_2024-10-05T22:15:03.fits").as_deref(), Some("2024-10-05T22:15:03"));
        assert_eq!(reads("L 2024-10-05 22:15:03.fits").as_deref(), Some("2024-10-05T22:15:03"));
        assert_eq!(reads("L_2024-10-05_22_15_03.fits").as_deref(), Some("2024-10-05T22:15:03"));
        assert_eq!(reads("Light_20241005-221503_300s.fits").as_deref(), Some("2024-10-05T22:15:03"));
        assert_eq!(reads("Light_20241005_221503.fits").as_deref(), Some("2024-10-05T22:15:03"));
        assert_eq!(reads("L_1989-10-05_22-15-03.fits"), None, "before 1990");
        assert_eq!(reads("L_2101-10-05_22-15-03.fits"), None, "after 2100");
        assert_eq!(reads("L_2024-02-30_22-15-03.fits"), None, "no such day");
        assert_eq!(reads("L_2024-10-05_25-15-03.fits"), None, "no such hour");
        assert_eq!(reads("L_20241005_2215033.fits"), None, "the digits go on");
        assert_eq!(reads("L_120241005_221503.fits"), None, "the digits begin earlier");
        assert_eq!(reads("M31_0001.fits"), None);
        // The first valid one wins; an invalid one before it is passed over.
        assert_eq!(reads("x_2024-13-05_22-15-03_2023-01-02_03-04-05.fits").as_deref(), Some("2023-01-02T03:04:05"));

        // A frame with no readable date is a reason, not a guess.
        let (got, none) = infer(&[light(2, "M31", "M31_0002.fits")], &[], &[FillField::Date]);
        assert!(got.is_empty());
        assert_eq!(none[0].why, "the file name 'M31_0002.fits' holds no date and time it can read");
        // A frame that has a date is not proposed for.
        let dated = frame(3, "M31", name, |f| f.date_obs = Some(Utc::now()));
        let (got, none) = infer(&[dated], &[], &[FillField::Date]);
        assert!(got.is_empty() && none.is_empty());
    }

    #[test]
    fn camera_from_agreeing_neighbours() {
        let neighbours = with_camera(100, 40, "M31/L", "ZWO ASI2600MM Pro");
        let missing = vec![frame(1, "M31/L", "m1.fits", |_| {}), frame(2, "M31/L", "m2.fits", |_| {})];
        let (got, none) = infer(&missing, &neighbours, &[FillField::Camera]);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(got.len(), 2);
        for p in &got {
            assert_eq!(p.value, "ZWO ASI2600MM Pro");
            assert_eq!(p.evidence, "40 frames in this folder have INSTRUME = 'ZWO ASI2600MM Pro'");
        }
        // Another folder's frames are not neighbours.
        let elsewhere = vec![frame(3, "M31/Other", "m3.fits", |_| {})];
        let (got, none) = infer(&elsewhere, &neighbours, &[FillField::Camera]);
        assert!(got.is_empty());
        assert_eq!(none[0].why, "fewer than 3 frames in this folder have it");
    }

    #[test]
    fn object_from_agreeing_neighbours_names_count_and_value() {
        let neighbours = with_object(100, 12, "M31/L", "M 31");
        let missing = vec![light(1, "M31/L", "m1.fits"), light(2, "M31/L", "m2.fits")];
        let (got, none) = infer(&missing, &neighbours, &[FillField::Object]);
        assert!(none.is_empty(), "{none:?}");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].value, "M 31");
        assert_eq!(got[0].evidence, "12 frames in this folder have OBJECT = 'M 31'");

        // The object is for lights only: a dark in the same folder is not proposed for.
        let dark = frame(5, "M31/L", "d.fits", |f| f.imagetyp = Some(ImageType::Dark));
        let (got, none) = infer(&[dark], &neighbours, &[FillField::Object]);
        assert!(got.is_empty() && none.is_empty(), "{got:?} {none:?}");
    }

    #[test]
    fn flats_never_name_a_lights_object() {
        // Three flats of the folder say OBJECT = 'FlatField': that is not the light's target.
        let flats: Vec<ScopedFrame> = (0..3)
            .map(|k| {
                frame(100 + k, "M31/L", &format!("f{k}.fits"), |f| {
                    f.object = Some("FlatField".to_string());
                    f.imagetyp = Some(ImageType::Flat);
                })
            })
            .collect();
        let (got, none) = infer(&[light(1, "M31/L", "m1.fits")], &flats, &[FillField::Object]);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(none[0].why, "fewer than 3 frames in this folder have it");
        // Lights of the folder still speak for it, whatever else is there.
        let mut beside = flats;
        beside.extend(with_object(200, 3, "M31/L", "M 31"));
        let (got, _) = infer(&[light(1, "M31/L", "m1.fits")], &beside, &[FillField::Object]);
        assert_eq!(got[0].value, "M 31");
        assert_eq!(got[0].evidence, "3 frames in this folder have OBJECT = 'M 31'");
    }

    #[test]
    fn a_master_keeps_its_master_type() {
        let master = |id: i64, folder: &str| frame(id, folder, "master.fits", |f| f.is_master = true);
        let missing = vec![
            master(1, "cal/Darks"),
            master(2, "cal/Flats"),
            master(3, "cal/Bias"),
            master(4, "cal/DarkFlats"),
            master(5, "cal/Lights"),
            frame(6, "cal/Darks", "raw.fits", |_| {}),
        ];
        let (got, none) = infer(&missing, &[], &[FillField::FrameType]);
        assert!(none.is_empty(), "{none:?}");
        let kinds: Vec<(i64, &str)> = got.iter().map(|p| (p.frame_id, p.value.as_str())).collect();
        assert_eq!(
            kinds,
            vec![(1, "MasterDark"), (2, "MasterFlat"), (3, "MasterBias"), (4, "MasterDarkFlat"), (5, "MasterLight"), (6, "Dark")]
        );
        assert_eq!(got[0].evidence, "folder 'Darks' in its path; the file is a master");
        assert_eq!(got[5].evidence, "folder 'Darks' in its path");
        // The user's word for a master is the master kind too.
        let said = by_instruction(&missing[..1], FillField::FrameType, "Dark");
        assert_eq!((said[0].value.as_str(), said[0].evidence.as_str()), ("MasterDark", "by your instruction; the file is a master"));
    }

    #[test]
    fn disagreeing_neighbours_make_no_proposal() {
        let mut neighbours = with_object(100, 30, "M31/L", "A");
        neighbours.extend(with_object(200, 10, "M31/L", "B"));
        let (got, none) = infer(&[light(1, "M31/L", "m1.fits")], &neighbours, &[FillField::Object]);
        assert!(got.is_empty(), "{got:?}");
        assert_eq!(none.len(), 1);
        assert_eq!(none[0].why, "frames in this folder disagree: 30 'A', 10 'B'");
        assert_eq!((none[0].frame_id, none[0].field), (1, FillField::Object));
    }

    #[test]
    fn two_neighbours_are_not_enough() {
        let neighbours = with_camera(100, 2, "M31/L", "Cam");
        let (got, none) = infer(&[frame(1, "M31/L", "m1.fits", |_| {})], &neighbours, &[FillField::Camera]);
        assert!(got.is_empty());
        assert_eq!(none[0].why, "fewer than 3 frames in this folder have it");
        // Two that disagree are two too few before they are a disagreement.
        let mut mixed = with_camera(100, 1, "M31/L", "A");
        mixed.extend(with_camera(200, 1, "M31/L", "B"));
        let (_, none) = infer(&[frame(1, "M31/L", "m1.fits", |_| {})], &mixed, &[FillField::Camera]);
        assert_eq!(none[0].why, "fewer than 3 frames in this folder have it");
        // Three are enough.
        let three = with_camera(100, 3, "M31/L", "Cam");
        let (got, _) = infer(&[frame(1, "M31/L", "m1.fits", |_| {})], &three, &[FillField::Camera]);
        assert_eq!(got[0].evidence, "3 frames in this folder have INSTRUME = 'Cam'");
    }

    #[test]
    fn statement_says_by_your_instruction() {
        let frames = vec![
            light(1, "M31/L", "a.fits"),
            light(2, "M31/L", "b.fits"),
            // Has an object already: the user's word does not overwrite it.
            frame(3, "M31/L", "c.fits", |f| {
                f.imagetyp = Some(ImageType::Light);
                f.object = Some("Other".to_string());
            }),
            // Not a light: no object to fill.
            frame(4, "M31/D", "d.fits", |f| f.imagetyp = Some(ImageType::Dark)),
        ];
        let got = by_instruction(&frames, FillField::Object, "M 31");
        assert_eq!(got.iter().map(|p| p.frame_id).collect::<Vec<_>>(), vec![1, 2]);
        for p in &got {
            assert_eq!((p.value.as_str(), p.evidence.as_str()), ("M 31", "by your instruction"));
            assert_eq!(p.field, FillField::Object);
        }

        assert_eq!(normalize_statement(FillField::FrameType, "Flats").as_deref(), Ok("Flat"));
        assert_eq!(normalize_statement(FillField::FrameType, "dark flat").as_deref(), Ok("DarkFlat"));
        assert!(normalize_statement(FillField::FrameType, "master dark").is_err());
        let no_time = normalize_statement(FillField::Date, "2024-10-05").expect_err("a date needs a time");
        assert!(no_time.contains("give the time too (YYYY-MM-DD HH:MM:SS)"), "{no_time}");
        assert_eq!(normalize_statement(FillField::Date, "2024-10-05 22:15:03").as_deref(), Ok("2024-10-05T22:15:03"));
        assert_eq!(normalize_statement(FillField::Date, "2024-10-05T22:15:03").as_deref(), Ok("2024-10-05T22:15:03"));
        assert!(normalize_statement(FillField::Date, "last night").is_err());
        assert!(normalize_statement(FillField::Date, "2024-10-05T22:15:03Z").is_err());
        assert_eq!(normalize_statement(FillField::Camera, "  ZWO ASI2600MM Pro ").as_deref(), Ok("ZWO ASI2600MM Pro"));
        assert!(normalize_statement(FillField::Object, "   ").is_err());
    }
}
