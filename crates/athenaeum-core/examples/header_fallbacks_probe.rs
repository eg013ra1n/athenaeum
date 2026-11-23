//! Acceptance probe of the FITS keyword fallbacks (issue #21, spec 9.2-9.3), run on a COPY of a
//! catalog, never the live one.
//!
//! ```text
//! cargo run --release -p athenaeum-core --example header_fallbacks_probe -- \
//!     --db <copy> [--rules <json file>] [--samples N] (preview | apply | both <field> <KW1> <KW2> | snapshot <out.tsv>)
//! ```
//!
//! - `preview`: the counts of `preview(..)`, then per class (format, field, old value, keyword that
//!   gives the new value) the number of changes and up to `--samples` (5) samples
//!   `frame_id  file name  folder  old -> new`; then the number of FITS rows whose stored header
//!   has a line that is not an 80-column card.
//! - `apply`: runs `apply(..)` with a `NullEmitter` and prints the counts.
//! - `both <field> <KW1> <KW2>`: the frames whose stored header resolves both keywords to different
//!   values.
//! - `snapshot <out.tsv>`: the typed values (`frames.override = 1`) and the light memberships, as
//!   two TSV sections, for a before/after `diff`.

use athenaeum_core::db::Database;
use athenaeum_core::events::NullEmitter;
use athenaeum_core::fits_parser::header_keys::stored_header_key_map;
use athenaeum_core::header_fallbacks::apply::{apply, plan_frame, preview, read_rows, ScanRoots, PAGE_SIZE};
use athenaeum_core::header_fallbacks::{
    resolve_keyword, validate, FieldValue, HeaderField, HeaderFallbackConfig,
};
use athenaeum_core::models::FileFormat;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let db_path = take_flag(&mut args, "--db").ok_or_else(|| anyhow::anyhow!("--db <copy> is required"))?;
    let rules_path = take_flag(&mut args, "--rules");
    let samples: usize = take_flag(&mut args, "--samples").map(|s| s.parse()).transpose()?.unwrap_or(5);
    let rules = match rules_path {
        Some(p) => validate(&serde_json::from_str::<HeaderFallbackConfig>(&std::fs::read_to_string(p)?)?)
            .map_err(|e| anyhow::anyhow!("rules refused: {e:?}"))?,
        None => HeaderFallbackConfig::default(),
    };
    let db = Database::new(PathBuf::from(&db_path))?;
    let conn = db.conn();
    match args.first().map(String::as_str) {
        Some("preview") => run_preview(&conn, &rules, samples),
        Some("apply") => {
            let started = Instant::now();
            let gap_hours = 8.0;
            let report = apply(&conn, &rules, gap_hours, &NullEmitter)?;
            println!("apply took {} ms", started.elapsed().as_millis());
            println!("{:?}", report.counts);
            println!("frame_sets_touched={} failed_night_sets={:?}", report.frame_sets_touched.len(), report.failed_night_sets);
            Ok(())
        }
        Some("both") => {
            let field = field_by_id(args.get(1).ok_or_else(|| anyhow::anyhow!("both <field> <KW1> <KW2>"))?)?;
            let (k1, k2) = (args.get(2).ok_or_else(|| anyhow::anyhow!("KW1"))?, args.get(3).ok_or_else(|| anyhow::anyhow!("KW2"))?);
            run_both(&conn, field, k1, k2)
        }
        Some("snapshot") => run_snapshot(&conn, Path::new(args.get(1).ok_or_else(|| anyhow::anyhow!("snapshot <out.tsv>"))?)),
        _ => anyhow::bail!("expected preview | apply | both <field> <KW1> <KW2> | snapshot <out.tsv>"),
    }
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = args.iter().position(|a| a == flag)?;
    if i + 1 >= args.len() {
        return None;
    }
    let v = args.remove(i + 1);
    args.remove(i);
    Some(v)
}

fn field_by_id(id: &str) -> anyhow::Result<HeaderField> {
    HeaderField::ALL
        .iter()
        .copied()
        .find(|f| f.id() == id)
        .ok_or_else(|| anyhow::anyhow!("unknown field id {id}"))
}

fn show(v: &Option<FieldValue>) -> String {
    match v {
        None => "NULL".to_string(),
        Some(FieldValue::Number(n)) => format!("{n}"),
        Some(FieldValue::Text(t)) => format!("{t:?}"),
    }
}

fn split_path(p: &str) -> (String, String) {
    let cut = p.rfind(['/', '\\']);
    match cut {
        Some(i) => (p[i + 1..].to_string(), p[..i].to_string()),
        None => (p.to_string(), String::new()),
    }
}

#[derive(Default)]
struct Class {
    count: usize,
    samples: Vec<String>,
}

fn run_preview(conn: &rusqlite::Connection, rules: &HeaderFallbackConfig, samples: usize) -> anyhow::Result<()> {
    let started = Instant::now();
    let counts = preview(conn, rules)?;
    let preview_ms = started.elapsed().as_millis();
    println!("preview took {preview_ms} ms");
    println!("{counts:?}");

    let roots = ScanRoots::load(conn)?;
    let mut classes: BTreeMap<String, Class> = BTreeMap::new();
    let mut fits_rows = 0usize;
    let mut fits_non80 = 0usize;
    let mut fits_no_card = 0usize;
    let mut after = 0i64;
    loop {
        let rows = read_rows(conn, after, PAGE_SIZE)?;
        let Some(last) = rows.last() else { break };
        after = last.frame_id;
        let n = rows.len();
        for row in &rows {
            if row.format == "FITS" {
                if let Some(h) = row.header.as_deref().filter(|h| !h.trim().is_empty()) {
                    fits_rows += 1;
                    let lines: Vec<&str> = h
                        .lines()
                        .map(|l| l.strip_suffix('\r').unwrap_or(l))
                        .filter(|l| !l.is_empty())
                        .collect();
                    if lines.iter().any(|l| l.len() != 80) {
                        fits_non80 += 1;
                    }
                    if !lines.is_empty() && lines.iter().all(|l| l.len() != 80) {
                        fits_no_card += 1;
                    }
                }
            }
            let plan = plan_frame(rules, row, &roots);
            if plan.no_header || plan.changes.is_empty() {
                continue;
            }
            let format = if row.format == "XISF" { FileFormat::XISF } else { FileFormat::FITS };
            let keys = stored_header_key_map(format, row.header.as_deref().unwrap_or(""));
            let (name, folder) = split_path(&row.file_path);
            for (field, new) in &plan.changes {
                let old = row.values.get(*field);
                let via = match new {
                    None => "(no keyword: cleared)".to_string(),
                    Some(_) => rules
                        .order(*field)
                        .iter()
                        .find(|k| resolve_keyword(*field, k, &keys).is_some())
                        .cloned()
                        .unwrap_or_else(|| "(scan form)".to_string()),
                };
                let old_state = match (&old, new) {
                    (None, _) => "old NULL",
                    (Some(FieldValue::Text(o)), Some(FieldValue::Text(n))) if o.trim() == n || o.trim().is_empty() => "old padded/blank",
                    (Some(FieldValue::Text(_)), _) => "old text differs",
                    (Some(FieldValue::Number(_)), _) => "old number differs",
                };
                let key = format!("{} {} {} via {}", row.format, field.id(), old_state, via);
                let class = classes.entry(key).or_default();
                class.count += 1;
                if class.samples.len() < samples {
                    class.samples.push(format!("{}  {}  {}  {} -> {}", row.frame_id, name, folder, show(&old), show(new)));
                }
            }
        }
        if n < PAGE_SIZE {
            break;
        }
    }
    println!("classes: {}", classes.len());
    for (key, class) in &classes {
        println!("[{}] {}", class.count, key);
        for s in &class.samples {
            println!("    {s}");
        }
    }
    println!(
        "FITS rows with a stored header: {fits_rows}; with a line that is not an 80-column card: {fits_non80}; with no 80-column card at all (hand dump): {fits_no_card}"
    );
    Ok(())
}

fn run_both(conn: &rusqlite::Connection, field: HeaderField, k1: &str, k2: &str) -> anyhow::Result<()> {
    let mut n = 0usize;
    let (mut col_null, mut col_is_k1, mut col_is_k2, mut col_other) = (0usize, 0usize, 0usize, 0usize);
    let mut printed = 0usize;
    let mut after = 0i64;
    loop {
        let rows = read_rows(conn, after, PAGE_SIZE)?;
        let Some(last) = rows.last() else { break };
        after = last.frame_id;
        let len = rows.len();
        for row in &rows {
            let format = match row.format.as_str() {
                "FITS" => FileFormat::FITS,
                "XISF" => FileFormat::XISF,
                _ => continue,
            };
            let Some(h) = row.header.as_deref().filter(|h| !h.trim().is_empty()) else { continue };
            let keys = stored_header_key_map(format, h);
            let (a, b) = (resolve_keyword(field, k1, &keys), resolve_keyword(field, k2, &keys));
            if let (Some(a), Some(b)) = (&a, &b) {
                if !athenaeum_core::header_fallbacks::same_value(a, b) {
                    n += 1;
                    match row.values.get(field) {
                        None => col_null += 1,
                        Some(c) if athenaeum_core::header_fallbacks::same_value(&c, a) => col_is_k1 += 1,
                        Some(c) if athenaeum_core::header_fallbacks::same_value(&c, b) => col_is_k2 += 1,
                        Some(_) => col_other += 1,
                    }
                    if printed < 5 {
                        printed += 1;
                        let (name, folder) = split_path(&row.file_path);
                        println!("sample {}  {}  {}  {}={} {}={} column={}", row.frame_id, name, folder, k1, show(&Some(a.clone())), k2, show(&Some(b.clone())), show(&row.values.get(field)));
                    }
                }
            }
        }
        if len < PAGE_SIZE {
            break;
        }
    }
    println!("both {} {} {}: N = {n}", field.id(), k1, k2);
    println!("  column NULL: {col_null}; equals {k1}'s value: {col_is_k1}; equals {k2}'s value: {col_is_k2}; equals neither (typed or kept): {col_other}");
    println!("  with {k1} first, the preview should count the NULL ones plus those equal to {k2}'s value: {}", col_null + col_is_k2);
    Ok(())
}

fn run_snapshot(conn: &rusqlite::Connection, out: &Path) -> anyhow::Result<()> {
    let mut f = std::fs::File::create(out)?;
    writeln!(f, "## typed values: id, ccd_temp, focallen, object (frames.override = 1)")?;
    let mut stmt = conn.prepare("SELECT id, ccd_temp, focallen, object FROM frames WHERE override = 1 ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut typed = 0usize;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let t: Option<f64> = r.get(1)?;
        let fl: Option<f64> = r.get(2)?;
        let o: Option<String> = r.get(3)?;
        writeln!(f, "{id}\t{}\t{}\t{}", opt(t), opt(fl), o.unwrap_or_else(|| "NULL".into()))?;
        typed += 1;
    }
    writeln!(f, "## light memberships: frame_id, frames_set_id")?;
    let mut stmt = conn.prepare(
        "SELECT sm.frame_id, n.frames_set_id FROM session_members sm JOIN sessions s ON s.id = sm.session_id \
         JOIN imaging_nights n ON n.id = s.imaging_night_id ORDER BY 1, 2",
    )?;
    let mut rows = stmt.query([])?;
    let mut members = 0usize;
    while let Some(r) = rows.next()? {
        let a: i64 = r.get(0)?;
        let b: i64 = r.get(1)?;
        writeln!(f, "{a}\t{b}")?;
        members += 1;
    }
    println!("snapshot {}: typed rows {typed}, memberships {members}", out.display());
    Ok(())
}

fn opt(v: Option<f64>) -> String {
    v.map(|n| format!("{n}")).unwrap_or_else(|| "NULL".into())
}
