//! Mirrors of the survey presets (issue #18, phase 3).
//!
//! A preset keeps its master URL; every other endpoint is found, not written
//! down (docs/skychart/nsns-permission.md, the author's ruling):
//! - the CDS aggregate of HiPS lists, queried by the survey's id;
//! - the author's own `hipslist`;
//! - the VO registry through RegTAP, for servers that publish no `hipslist`
//!   (the GAVO copy of NSNS).
//!
//! A candidate becomes a mirror only after its own `properties`, fetched from
//! the candidate URL itself, name the preset's `creator_did`, pass the
//! preset's checks (frame, width, format, order), carry the reference release
//! date and a `hips_status` that allows a copy. The URL a record names in
//! `hips_service_url` is never trusted as an address: GAVO's `rgb8` record
//! names a path that answers 404.
//!
//! The result is kept in `<app_data>/hips/mirrors.json` and refreshed at most
//! once a day; offline, the stored list is used, and without one the master
//! alone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{get_bounded, HipsStore, HipsSurvey, Properties, PROPERTIES_CAP, SURVEYS};

/// The CDS aggregate of HiPS lists, one survey per query (`ID` is the
/// `creator_did` without `ivo://`).
pub const CDS_MOCSERVER: &str = "https://alasky.cds.unistra.fr/MocServer/query";
/// `hipslist` files read whole: the author's own server.
pub const HIPSLISTS: &[&str] = &["https://www.simg.de/hipslist"];
/// A RegTAP endpoint of the VO registry (synchronous TAP).
pub const REGTAP_SYNC: &str = "https://dc.g-vo.org/tap/sync";
/// A stored list younger than this is used without asking the network.
pub const REFRESH_AFTER: Duration = Duration::from_secs(24 * 3600);
const LIST_CAP: u64 = 8 * 1024 * 1024;

/// `<app_data>/hips/mirrors.json`: the verified mirrors of each preset (the
/// master is implicit) and when they were found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorFile {
    /// Seconds since the Unix epoch.
    pub fetched_at: u64,
    pub surveys: BTreeMap<String, Vec<String>>,
}

impl MirrorFile {
    pub fn is_fresh(&self, now: SystemTime) -> bool {
        let at = UNIX_EPOCH + Duration::from_secs(self.fetched_at);
        now.duration_since(at).is_ok_and(|age| age < REFRESH_AFTER)
    }
}

fn file_path(app_data: &Path) -> std::path::PathBuf {
    app_data.join("hips").join("mirrors.json")
}

/// The stored list; `None` when there is none or it cannot be read (a
/// warning: the master alone still serves).
pub fn read_file(app_data: &Path) -> Option<MirrorFile> {
    let path = file_path(app_data);
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(f) => Some(f),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "hips mirror list unreadable");
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "hips mirror list unreadable");
            None
        }
    }
}

pub fn write_file(app_data: &Path, file: &MirrorFile) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(file).context("serialise mirrors.json")?;
    super::write_atomic(&file_path(app_data), &bytes)
}

/// Whether a `hips_status` lets a private copy be made from that endpoint.
/// HiPS 1.0 defaults a missing role to `master` and a missing clonability to
/// `clonableOnce`. `clonableOnce` allows one copy, from the master; its
/// clones are `unclonable`, so a copy from them would be a clone of a clone.
pub fn allows_copy(status: Option<&str>) -> bool {
    let words: Vec<&str> = status.unwrap_or("").split_whitespace().collect();
    if words.contains(&"private") {
        return false;
    }
    let master = !words.iter().any(|w| *w == "mirror" || *w == "partial");
    if words.contains(&"unclonable") {
        false
    } else if words.contains(&"clonable") {
        true
    } else {
        master
    }
}

/// Records of a `hipslist` (blank-line separated `properties` blocks).
pub fn parse_hipslist(text: &str) -> Vec<Properties> {
    let mut out = Vec::new();
    let mut block = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !block.trim().is_empty() {
                out.push(Properties::parse(&block));
            }
            block.clear();
        } else {
            block.push_str(line);
            block.push('\n');
        }
    }
    if !block.trim().is_empty() {
        out.push(Properties::parse(&block));
    }
    out
}

/// Every service URL a record names: `hips_service_url` and its numbered siblings.
fn service_urls(record: &Properties) -> Vec<String> {
    record
        .0
        .iter()
        .filter(|(k, _)| *k == "hips_service_url" || k.strip_prefix("hips_service_url_").is_some_and(|n| n.parse::<u32>().is_ok()))
        .map(|(_, v)| v.clone())
        .collect()
}

/// `https://host/path` without a trailing slash; a plain `http` URL is moved
/// to `https` (every server checked serves both). `None` for anything that is
/// not an http(s) URL.
pub fn normalize(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    if rest.is_empty() || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("https://{rest}"))
}

/// The host of an endpoint URL, for logs and the Settings page.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// Access URLs of the RegTAP CSV answer (first column, header skipped).
pub fn parse_regtap_csv(text: &str) -> Vec<String> {
    text.lines().skip(1).filter_map(|l| l.split(',').next()).map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
}

fn regtap_query(ivoids: &BTreeSet<&str>) -> String {
    let list: Vec<String> = ivoids.iter().map(|i| format!("'{}'", i.replace('\'', "''"))).collect();
    format!(
        "SELECT access_url FROM rr.interface NATURAL JOIN rr.capability \
         WHERE standard_id LIKE 'ivo://ivoa.net/std/hips#hips-1.%' AND ivoid IN ({})",
        list.join(", ")
    )
}

/// Where candidates come from. A trait so tests run without the network.
pub trait Sources {
    /// The CDS aggregate's records for one `creator_did`.
    fn cds_records(&self, creator_did: &str) -> Result<String>;
    /// One `hipslist` file.
    fn hipslist(&self, url: &str) -> Result<String>;
    /// RegTAP answer (CSV) to an ADQL query.
    fn regtap(&self, adql: &str) -> Result<String>;
    /// A candidate's `properties`.
    fn properties(&self, base_url: &str) -> Result<String>;
}

/// The real network, through the HiPS client.
pub struct Network;

fn get_text(url: &str, cap: u64) -> Result<String> {
    let body = get_bounded(url, cap)?.with_context(|| format!("GET {url}: 404"))?;
    String::from_utf8(body).with_context(|| format!("GET {url}: not UTF-8"))
}

impl Sources for Network {
    fn cds_records(&self, creator_did: &str) -> Result<String> {
        let id = creator_did.strip_prefix("ivo://").unwrap_or(creator_did);
        let url = reqwest::Url::parse_with_params(CDS_MOCSERVER, &[("ID", id), ("get", "record")])
            .context("build the CDS query")?;
        get_text(url.as_str(), LIST_CAP)
    }

    fn hipslist(&self, url: &str) -> Result<String> {
        get_text(url, LIST_CAP)
    }

    fn regtap(&self, adql: &str) -> Result<String> {
        let url = reqwest::Url::parse_with_params(REGTAP_SYNC, &[("LANG", "ADQL"), ("FORMAT", "csv"), ("QUERY", adql)])
            .context("build the RegTAP query")?;
        get_text(url.as_str(), LIST_CAP)
    }

    fn properties(&self, base_url: &str) -> Result<String> {
        get_text(&format!("{base_url}/properties"), PROPERTIES_CAP)
    }
}

/// Why a candidate is not a mirror of `survey`, or `None` when it is one.
/// `reference` is the release the store holds (or the master's), when known.
pub fn refusal(survey: &HipsSurvey, props: &Properties, reference: Option<&str>) -> Option<String> {
    if props.get("creator_did") != Some(survey.creator_did) {
        return Some(format!("creator_did is {:?}", props.get("creator_did")));
    }
    if let Err(e) = props.validate(survey) {
        return Some(format!("{e:#}"));
    }
    if !allows_copy(props.get("hips_status")) {
        return Some(format!("hips_status {:?} allows no copy", props.get("hips_status")));
    }
    if let Some(reference) = reference {
        if props.get("hips_release_date") != Some(reference) {
            return Some(format!("release {:?}, the store holds {reference}", props.get("hips_release_date")));
        }
    }
    None
}

/// The release a mirror must carry: the one the store already holds, else
/// the master's live one, else none (an empty store mixes nothing).
fn reference_release(app_data: &Path, survey: &HipsSurvey, sources: &dyn Sources) -> Option<String> {
    let stored = HipsStore::new(app_data, survey).read_properties().ok().flatten();
    if let Some(date) = stored.as_ref().and_then(|p| p.get("hips_release_date")) {
        return Some(date.to_string());
    }
    match sources.properties(survey.base_url) {
        Ok(text) => Properties::parse(&text).get("hips_release_date").map(str::to_string),
        Err(e) => {
            tracing::warn!(survey_id = survey.id, error = %format!("{e:#}"), "hips master properties unreachable");
            None
        }
    }
}

/// Adds the service URLs of every record that names a preset.
fn take_records(text: &str, candidates: &mut BTreeMap<&'static str, BTreeSet<String>>) {
    for record in parse_hipslist(text) {
        let Some(survey) = SURVEYS.iter().find(|s| record.get("creator_did") == Some(s.creator_did)) else { continue };
        candidates.entry(survey.id).or_default().extend(service_urls(&record).iter().filter_map(|u| normalize(u)));
    }
}

/// Finds and checks the mirrors of every preset. A source that fails is a
/// warning; `Err` only when every source failed, so a stored list is kept.
pub fn discover(app_data: &Path, sources: &dyn Sources, now: SystemTime) -> Result<MirrorFile> {
    let mut candidates: BTreeMap<&'static str, BTreeSet<String>> = BTreeMap::new();
    let mut unknown: BTreeSet<String> = BTreeSet::new();
    let mut answered = 0usize;
    let mut last_error = None;

    for survey in SURVEYS {
        match sources.cds_records(survey.creator_did) {
            Ok(text) => {
                answered += 1;
                take_records(&text, &mut candidates);
            }
            Err(e) => {
                // One failure stands for the server: the other queries would
                // each wait out the same timeout.
                tracing::warn!(survey_id = survey.id, error = %format!("{e:#}"), "hips mirror source failed");
                last_error = Some(e);
                break;
            }
        }
    }
    for url in HIPSLISTS {
        match sources.hipslist(url) {
            Ok(text) => {
                answered += 1;
                take_records(&text, &mut candidates);
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "hips mirror source failed");
                last_error = Some(e);
            }
        }
    }
    let ivoids: BTreeSet<&str> = SURVEYS.iter().flat_map(|s| s.registry_ivoids.iter().copied()).collect();
    if !ivoids.is_empty() {
        match sources.regtap(&regtap_query(&ivoids)) {
            Ok(text) => {
                answered += 1;
                unknown.extend(parse_regtap_csv(&text).iter().filter_map(|u| normalize(u)));
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "hips mirror source failed");
                last_error = Some(e);
            }
        }
    }
    if answered == 0 {
        return Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no mirror source answered")));
    }

    // Each candidate's own `properties`, read once.
    let masters: BTreeSet<String> = SURVEYS.iter().filter_map(|s| normalize(s.base_url)).collect();
    let mut all: BTreeSet<String> = candidates.values().flatten().cloned().collect();
    all.extend(unknown.iter().cloned());
    let mut props: BTreeMap<String, Properties> = BTreeMap::new();
    for url in all.difference(&masters) {
        match sources.properties(url) {
            Ok(text) => {
                props.insert(url.clone(), Properties::parse(&text));
            }
            Err(e) => tracing::warn!(endpoint = host_of(url), error = %format!("{e:#}"), "hips mirror candidate unreachable"),
        }
    }

    let mut file = MirrorFile {
        fetched_at: now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        surveys: BTreeMap::new(),
    };
    for survey in SURVEYS {
        let named = candidates.get(survey.id).cloned().unwrap_or_default();
        let registry = props
            .iter()
            .filter(|(url, p)| unknown.contains(*url) && p.get("creator_did") == Some(survey.creator_did))
            .map(|(url, _)| url.clone());
        let mine: BTreeSet<String> = named.into_iter().chain(registry).filter(|u| !masters.contains(u)).collect();
        if mine.is_empty() {
            continue;
        }
        let reference = reference_release(app_data, survey, sources);
        let mut ok = Vec::new();
        for url in mine {
            let Some(p) = props.get(&url) else { continue };
            match refusal(survey, p, reference.as_deref()) {
                None => ok.push(url),
                Some(why) => tracing::info!(survey_id = survey.id, endpoint = host_of(&url), reason = %why, "hips mirror refused"),
            }
        }
        if !ok.is_empty() {
            file.surveys.insert(survey.id.to_string(), ok);
        }
    }
    let mirrors: usize = file.surveys.values().map(Vec::len).sum();
    tracing::info!(mirrors, "hips mirrors refreshed");
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hips::{NSNS_RGB8, NSNS_SII8};
    use std::cell::RefCell;

    #[test]
    fn copy_rules_follow_hips_status() {
        assert!(allows_copy(Some("public master clonable")));
        assert!(allows_copy(Some("public master clonableOnce")));
        assert!(allows_copy(Some("public mirror clonable")));
        assert!(allows_copy(Some("public partial clonable")));
        assert!(!allows_copy(Some("public mirror unclonable")));
        assert!(!allows_copy(Some("public mirror clonableOnce")), "a clone of a clonableOnce master is unclonable");
        assert!(!allows_copy(Some("public master unclonable")));
        assert!(!allows_copy(Some("private master clonable")));
        // HiPS 1.0 defaults: master, clonableOnce.
        assert!(allows_copy(None));
        assert!(allows_copy(Some("public")));
        assert!(!allows_copy(Some("public mirror")));
    }

    #[test]
    fn urls_normalise_to_https_without_slash() {
        assert_eq!(normalize("http://dc.g-vo.org/nsns/q/sii8/hips/").as_deref(), Some("https://dc.g-vo.org/nsns/q/sii8/hips"));
        assert_eq!(normalize("https://a.b/c").as_deref(), Some("https://a.b/c"));
        assert_eq!(normalize("ftp://a.b/c"), None);
        assert_eq!(normalize("http://"), None);
        assert_eq!(host_of("https://dc.g-vo.org/nsns/q"), "dc.g-vo.org");
    }

    #[test]
    fn hipslist_records_and_numbered_urls() {
        let text = "creator_did = ivo://a/b\nhips_service_url = http://one/x\nhips_service_url_1 = http://two/x\nhips_service_url_x = nope\n\n\n# c\ncreator_did = ivo://c/d\n";
        let recs = parse_hipslist(text);
        assert_eq!(recs.len(), 2);
        assert_eq!(service_urls(&recs[0]), vec!["http://one/x".to_string(), "http://two/x".to_string()]);
        assert_eq!(recs[1].get("creator_did"), Some("ivo://c/d"));
    }

    #[test]
    fn regtap_csv_and_query() {
        let csv = "access_url\nhttp://dc.g-vo.org/nsns/q/sii8/hips\n\"http://dc.g-vo.org/nsns/cont/rgb8/hips\"\n";
        assert_eq!(parse_regtap_csv(csv), vec!["http://dc.g-vo.org/nsns/q/sii8/hips", "http://dc.g-vo.org/nsns/cont/rgb8/hips"]);
        let q = regtap_query(&["ivo://x'y"].into_iter().collect());
        assert!(q.contains("ivoid IN ('ivo://x''y')"), "{q}");
    }

    const RGB8_MASTER: &str = "creator_did = ivo://simg.de/P/NSNS/DR0_2/rgb8\nhips_status = public master clonable\nhips_release_date = 2025-06-07T16:19Z\nhips_order = 5\nhips_frame = equatorial\nhips_tile_width = 512\nhips_tile_format = png\n";

    #[test]
    fn refusals() {
        let ok = Properties::parse(&RGB8_MASTER.replace("master", "mirror"));
        assert_eq!(refusal(&NSNS_RGB8, &ok, Some("2025-06-07T16:19Z")), None);
        assert_eq!(refusal(&NSNS_RGB8, &ok, None), None);
        assert!(refusal(&NSNS_RGB8, &ok, Some("2026-01-01T00:00Z")).unwrap().contains("release"));
        assert!(refusal(&NSNS_SII8, &ok, None).unwrap().contains("creator_did"));
        let unclonable = Properties::parse(&RGB8_MASTER.replace("master clonable", "mirror unclonable"));
        assert!(refusal(&NSNS_RGB8, &unclonable, None).unwrap().contains("hips_status"));
        let fits = Properties::parse(&RGB8_MASTER.replace("= png", "= fits"));
        assert!(refusal(&NSNS_RGB8, &fits, None).is_some());
    }

    /// Answers from fixtures; records which property files were read.
    struct Fixture {
        cds: Option<String>,
        list: Option<String>,
        regtap: Option<String>,
        props: BTreeMap<String, String>,
        asked: RefCell<Vec<String>>,
        cds_calls: RefCell<usize>,
    }

    impl Sources for Fixture {
        fn cds_records(&self, _: &str) -> Result<String> {
            *self.cds_calls.borrow_mut() += 1;
            self.cds.clone().context("cds down")
        }
        fn hipslist(&self, _: &str) -> Result<String> {
            self.list.clone().context("list down")
        }
        fn regtap(&self, _: &str) -> Result<String> {
            self.regtap.clone().context("regtap down")
        }
        fn properties(&self, base_url: &str) -> Result<String> {
            self.asked.borrow_mut().push(base_url.to_string());
            self.props.get(base_url).cloned().context("404")
        }
    }

    fn fixture() -> Fixture {
        let gavo_rgb = "https://dc.g-vo.org/nsns/cont/rgb8/hips";
        // GAVO's record names a wrong address; the registry has the right one.
        let gavo_props = RGB8_MASTER.replace("master", "mirror") + "hips_service_url = http://dc.g-vo.org/nsns/q/rgb8/hips\n";
        let mut props = BTreeMap::new();
        props.insert(gavo_rgb.to_string(), gavo_props);
        props.insert("https://www.simg.de/nebulae3/dr0_2/rgb8".to_string(), RGB8_MASTER.to_string());
        props.insert(
            "https://dc.g-vo.org/nsns/q/sii/hips".to_string(),
            "creator_did = ivo://simg.de/P/NSNS/DR0_2/sii\nhips_tile_format = fits\n".to_string(),
        );
        props.insert("https://bad.example/rgb8".to_string(), RGB8_MASTER.replace("master clonable", "mirror unclonable"));
        Fixture {
            cds: None,
            list: Some(format!("{RGB8_MASTER}hips_service_url = http://www.simg.de/nebulae3/dr0_2/rgb8\nhips_service_url_1 = http://bad.example/rgb8\n")),
            regtap: Some("access_url\nhttp://dc.g-vo.org/nsns/q/sii/hips\nhttp://dc.g-vo.org/nsns/cont/rgb8/hips\n".into()),
            props,
            asked: RefCell::new(Vec::new()),
            cds_calls: RefCell::new(0),
        }
    }

    #[test]
    fn discovery_keeps_verified_mirrors_only() {
        let dir = tempfile::tempdir().unwrap();
        let f = fixture();
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let file = discover(dir.path(), &f, now).unwrap();
        assert_eq!(file.fetched_at, 1_000_000);
        assert_eq!(
            file.surveys.get("nsns-rgb8"),
            Some(&vec!["https://dc.g-vo.org/nsns/cont/rgb8/hips".to_string()]),
            "the registry URL, not the record's; the unclonable copy and the master are not mirrors"
        );
        assert_eq!(file.surveys.len(), 1, "the FITS sii HiPS matches no preset");
        // The master's properties are read for the reference release only.
        assert_eq!(f.asked.borrow().iter().filter(|u| u.contains("simg.de")).count(), 1);
        assert_eq!(*f.cds_calls.borrow(), 1, "a CDS that fails is not asked again for the other presets");

        write_file(dir.path(), &file).unwrap();
        assert_eq!(read_file(dir.path()), Some(file.clone()));
        assert!(file.is_fresh(now + Duration::from_secs(3600)));
        assert!(!file.is_fresh(now + REFRESH_AFTER));
    }

    #[test]
    fn a_store_of_another_release_refuses_the_mirror() {
        let dir = tempfile::tempdir().unwrap();
        HipsStore::new(dir.path(), &NSNS_RGB8).write_properties(&RGB8_MASTER.replace("2025-06-07T16:19Z", "2024-01-01T00:00Z")).unwrap();
        let file = discover(dir.path(), &fixture(), SystemTime::now()).unwrap();
        assert!(file.surveys.is_empty(), "{file:?}");
    }

    #[test]
    fn every_source_down_is_an_error_one_up_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let down = Fixture {
            cds: None,
            list: None,
            regtap: None,
            props: BTreeMap::new(),
            asked: RefCell::new(Vec::new()),
            cds_calls: RefCell::new(0),
        };
        assert!(discover(dir.path(), &down, SystemTime::now()).is_err());
        let only_regtap = Fixture { regtap: Some("access_url\n".into()), ..down };
        assert!(discover(dir.path(), &only_regtap, SystemTime::now()).unwrap().surveys.is_empty());
    }

    #[test]
    fn an_unreadable_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("hips")).unwrap();
        fs::write(dir.path().join("hips/mirrors.json"), b"{nope").unwrap();
        assert_eq!(read_file(dir.path()), None);
    }
}
