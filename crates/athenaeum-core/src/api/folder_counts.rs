//! Catalog-only file counts. Each stored file counts once, including derivatives
//! and offline files; these are not deduplicated exposure/integration statistics.
use super::{db, ApiError, PathPolicy};
use crate::services::ServiceContext;
use std::{collections::BTreeMap, path::Path};

#[derive(Default, Debug, Clone, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FrameTypeCounts {
    pub total: u64,
    pub lights: u64,
    pub darks: u64,
    pub flats: u64,
    pub bias: u64,
    pub dark_flats: u64,
    pub masters: u64,
    pub unknown: u64,
}
impl FrameTypeCounts {
    fn add(&mut self, kind: &str) {
        self.total += 1;
        let normalized = kind.to_ascii_uppercase().replace([' ', '_', '-'], "");
        match normalized.as_str() {
            "LIGHT" => self.lights += 1,
            "DARK" => self.darks += 1,
            "FLAT" => self.flats += 1,
            "BIAS" => self.bias += 1,
            "DARKFLAT" | "FLATDARK" => self.dark_flats += 1,
            "MASTERLIGHT" | "MASTERDARK" | "MASTERFLAT" | "MASTERBIAS" | "MASTERDARKFLAT"
            | "MASTERFLATDARK" => self.masters += 1,
            _ => self.unknown += 1,
        }
    }
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderTypeRow {
    pub path: String,
    pub counts: FrameTypeCounts,
}
#[derive(Debug, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct FolderTypeBreakdown {
    pub direct: FrameTypeCounts,
    pub recursive: FrameTypeCounts,
    pub children: Vec<FolderTypeRow>,
}

/// Count cataloged files directly in `path`, all descendants, and each immediate
/// child subtree. Does not enumerate disk or follow symlinks; missing files remain
/// represented until the catalog is updated. Empty/noncataloged directories have
/// no child row. Path-prefix comparisons include a separator to avoid siblings.
pub fn get_folder_type_breakdown(
    ctx: &ServiceContext,
    path: String,
    policy: &PathPolicy,
) -> Result<FolderTypeBreakdown, ApiError> {
    if !Path::new(&path).is_absolute()
        || Path::new(&path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(ApiError::Invalid(
            "Choose an absolute catalog folder without parent traversal".into(),
        ));
    }
    policy.check(Path::new(&path))?;
    let database = db(ctx)?;
    Ok(counts(&database.conn(), &path)?)
}
fn counts(conn: &rusqlite::Connection, path: &str) -> anyhow::Result<FolderTypeBreakdown> {
    let prefix = format!("{}/", path.replace('\\', "/").trim_end_matches('/'));
    let mut result = FolderTypeBreakdown {
        direct: FrameTypeCounts::default(),
        recursive: FrameTypeCounts::default(),
        children: vec![],
    };
    let mut children: BTreeMap<String, FrameTypeCounts> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "
        SELECT replace(fi.path,char(92), '/' ), COALESCE((SELECT f.imagetyp FROM
        frames f WHERE f.file_id=fi.id ORDER BY f.id LIMIT 1), '' ) FROM files
        fi WHERE substr(replace(fi.path,char(92), '/' ),1,length(?1))=?1
        ",
    )?;
    let rows = stmt.query_map([&prefix], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (file, kind) = row?;
        result.recursive.add(&kind);
        let relative = &file[prefix.len()..];
        if let Some((child, _)) = relative.split_once('/') {
            children
                .entry(format!("{prefix}{child}"))
                .or_default()
                .add(&kind);
        } else {
            result.direct.add(&kind);
        }
    }
    result.children = children
        .into_iter()
        .map(|(path, counts)| FolderTypeRow { path, counts })
        .collect();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_counts_do_not_include_similar_siblings_or_duplicate_file_rows() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "
        CREATE TABLE files(id INTEGER,path TEXT); CREATE TABLE frames(id
        INTEGER,file_id INTEGER,imagetyp TEXT);
        INSERT INTO files VALUES(1, '/data/a%/one.fit' ),(2,
        '/data/a%/night/dark.fit' ),(3, '/data/a%/night/deep/bias.fit' ),(4,
        '/data/a%2/exclude.fit' ),(5, '/data/a%/unknown.fit' ),(6,
        '/data/a%/master.fit' );
        INSERT INTO frames VALUES(1,1, 'Light' ),(2,2, 'DARK' ),(3,3, 'Bias'
        ),(4,4, 'Light' ),(6,6, 'Master Flat' ),(7,1, 'Light' );
        ",
        )
        .unwrap();
        let b = counts(&c, "/data/a%/").unwrap();
        assert_eq!(b.direct.total, 3);
        assert_eq!(b.recursive.total, 5);
        assert_eq!(b.recursive.lights, 1);
        assert_eq!(b.recursive.darks, 1);
        assert_eq!(b.recursive.bias, 1);
        assert_eq!(b.recursive.unknown, 1);
        assert_eq!(b.recursive.masters, 1);
        assert_eq!(b.children.len(), 1);
        assert_eq!(b.children[0].counts.total, 2);
        assert_eq!(counts(&c, "/data/a%/night").unwrap().direct.darks, 1);
        assert_eq!(counts(&c, "/absent").unwrap().recursive.total, 0);
    }
    #[test]
    fn type_aliases_and_root_paths() {
        let mut c = FrameTypeCounts::default();
        for kind in ["Dark Flat", "dark_flat", "FlatDark", "MasterBias", "odd"] {
            c.add(kind);
        }
        assert_eq!(c.dark_flats, 3);
        assert_eq!(c.masters, 1);
        assert_eq!(c.unknown, 1);
        assert_eq!(c.total, 5);
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "
        CREATE TABLE files(id INTEGER,path TEXT); CREATE TABLE frames(id
        INTEGER,file_id INTEGER,imagetyp TEXT); INSERT INTO files VALUES(1,
        '/a.fit' ),(2, '/child/b.fit' );
        ",
        )
        .unwrap();
        let b = counts(&c, "/").unwrap();
        assert_eq!(b.direct.total, 1);
        assert_eq!(b.recursive.total, 2);
    }
}
