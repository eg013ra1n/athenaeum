//! Replays the hub's recorded answers against [`FakeHub`] and reports every
//! place where the fake answers differently (tests only).
//!
//! A fixture (`tests/fixtures/hub_contract/<nn>-<name>.json`, copied from the
//! hub by `scripts/sync-hub-contract.sh`, the hub commit in `HUB_COMMIT`) is
//! one recorded scenario: a `given` (accounts, devices, one project), then
//! numbered `steps`, each a request and the hub's answer. The replayer builds
//! the `given` on a fresh fake, sends every step to it over HTTP, and compares
//! status and body with what the hub answered. Nothing here is edited by hand:
//! when the fake and the hub differ, the fake is fixed to answer like the hub.
//!
//! How a recorded answer is read:
//!
//! - ids the hub minted are symbols (`{{project:P}}`, `{{account:A}}`,
//!   `{{device:A1}}`); the replayer binds ids of its own to them, in the
//!   `given`'s order, and substitutes them in paths, request bodies and
//!   answers;
//! - a string under a timestamp key is `{{ts}}`: the fake's value must be a
//!   string RFC 3339 accepts;
//! - a counter is `{{v0}}` or `{{v0+3}}`: an integer equal to the base the
//!   step-0 `capture` bound to that name, plus the offset;
//! - everything else (public keys, frame uuids, digests, `frameSeq`, text)
//!   is compared exactly; object key order never matters and numbers compare
//!   as `serde_json` compares them;
//! - `ignore` entries are removed from both answers before comparing, and
//!   `unordered` entries compare an array as a set keyed by an element's
//!   pointer;
//! - the recorded body is also rendered back to concrete JSON and parsed into
//!   the step's `wire` type (`error:<Variant>` runs it through the client's
//!   own status classifier), so a hub answer the client could not read
//!   fails here too. That failure is a defect of the client, not of the fake.
//!
//! A difference is reported per fixture, all at once, through the test's
//! panic message.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::account::AccountClientError;
use crate::collab::fake_hub::FakeHub;
use crate::collab::hub_client::{
    classify_for_contract, AnnounceFramesWire, ManifestPageWire, MyProjectWire, NewVersionWire,
    PublishingDeviceSwitchWire,
};
use crate::collab::live::wire::{
    HoldersReportReplyWire, HoldersSnapshotWire, VersionStatus, VersionsReplyWire,
};

/// The fixtures this module replays, one test each. `every_fixture_is_replayed`
/// keeps the list equal to the folder.
const FIXTURES: &[&str] = &["01-snapshot_after_announce"];

/// What a report prints for a side that has no value at a pointer.
const MISSING: &str = "<missing>";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hub_contract")
}

fn load(stem: &str) -> Value {
    let path = fixtures_dir().join(format!("{stem}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
}

/// The hub commit the fixtures were copied from (first word of `HUB_COMMIT`).
fn hub_commit() -> String {
    std::fs::read_to_string(fixtures_dir().join("HUB_COMMIT"))
        .ok()
        .and_then(|t| t.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Pointers ───────────────────────────────────────────────────────────────

/// `"/a/b"` becomes `["a", "b"]`; `""` is the root.
fn pointer_segments(pointer: &str) -> Vec<String> {
    if pointer.is_empty() {
        return Vec::new();
    }
    assert!(
        pointer.starts_with('/'),
        "JSON pointer must start with '/': {pointer}"
    );
    pointer[1..]
        .split('/')
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect()
}

fn escape_segment(seg: &str) -> String {
    seg.replace('~', "~0").replace('/', "~1")
}

/// Calls `f` on every value the pointer reaches; a `*` segment fans out over
/// the elements of an array or the values of an object.
fn visit_mut(v: &mut Value, segs: &[String], f: &mut dyn FnMut(&mut Value)) {
    let Some((head, rest)) = segs.split_first() else {
        f(v);
        return;
    };
    match v {
        Value::Object(m) => {
            if head == "*" {
                for child in m.values_mut() {
                    visit_mut(child, rest, f);
                }
            } else if let Some(child) = m.get_mut(head.as_str()) {
                visit_mut(child, rest, f);
            }
        }
        Value::Array(a) => {
            if head == "*" {
                for child in a.iter_mut() {
                    visit_mut(child, rest, f);
                }
            } else if let Some(child) = head.parse::<usize>().ok().and_then(|i| a.get_mut(i)) {
                visit_mut(child, rest, f);
            }
        }
        _ => {}
    }
}

/// Removes the field (or array element) the pointer names; a last `*` empties
/// the container. A pointer that reaches nothing removes nothing.
fn remove_at(v: &mut Value, pointer: &str) {
    let segs = pointer_segments(pointer);
    let Some((last, parents)) = segs.split_last() else {
        *v = Value::Null;
        return;
    };
    visit_mut(v, parents, &mut |parent| match parent {
        Value::Object(m) => {
            if last == "*" {
                m.clear();
            } else {
                m.remove(last.as_str());
            }
        }
        Value::Array(a) => {
            if last == "*" {
                a.clear();
            } else if let Some(i) = last.parse::<usize>().ok().filter(|i| *i < a.len()) {
                a.remove(i);
            }
        }
        _ => {}
    });
}

/// Sorts the array(s) at `pointer` by the value at `key` inside each element,
/// so two arrays holding the same elements compare equal.
fn sort_set(v: &mut Value, pointer: &str, key: &str) {
    let segs = pointer_segments(pointer);
    visit_mut(v, &segs, &mut |slot| {
        if let Value::Array(a) = slot {
            a.sort_by_key(|e| e.pointer(key).map(|k| k.to_string()).unwrap_or_default());
        }
    });
}

fn step_applies(spec: &Value, index: usize) -> bool {
    match spec {
        Value::String(s) => s == "*",
        Value::Number(n) => n.as_u64() == Some(index as u64),
        _ => false,
    }
}

/// `body` as it is compared at step `index`: every `ignore` that applies
/// removed, every `unordered` array that applies sorted.
fn effective(doc: &Value, index: usize, body: &mut Value) {
    for ig in doc["ignore"].as_array().into_iter().flatten() {
        if step_applies(&ig["step"], index) {
            remove_at(body, ig["path"].as_str().expect("ignore.path is a string"));
        }
    }
    for un in doc["unordered"].as_array().into_iter().flatten() {
        if step_applies(&un["step"], index) {
            sort_set(
                body,
                un["path"].as_str().expect("unordered.path is a string"),
                un["key"].as_str().expect("unordered.key is a string"),
            );
        }
    }
}

// ── Symbols, counters, timestamps ──────────────────────────────────────────

/// One recorded token standing for a value the hub makes up.
enum Token {
    /// `{{ts}}`: an RFC 3339 string.
    Ts,
    /// `{{v0}}` / `{{v0+3}}`: base name and offset.
    Counter(String, i64),
}

/// The token a whole string is, if it is one. A string that opens `{{` and
/// closes `}}` but is none this replayer implements panics: a recorded rule
/// that is silently skipped would pass for nothing.
fn token_of(s: &str) -> Option<Token> {
    let inner = s.strip_prefix("{{")?.strip_suffix("}}")?;
    if inner == "ts" {
        return Some(Token::Ts);
    }
    let (name, k) = match inner.split_once('+') {
        Some((name, k)) => (name, k.parse::<i64>().ok()),
        None => (inner, Some(0)),
    };
    let is_name = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && !matches!(name, "sig" | "hubkey");
    match (is_name, k) {
        (true, Some(k)) => Some(Token::Counter(name.to_string(), k)),
        _ => panic!("token {s} is not implemented by the replayer"),
    }
}

/// A timestamp the rendered wire body can carry for `{{ts}}`.
const RENDERED_TS: &str = "2026-01-01T00:00:00Z";

/// The ids bound to the fixture's symbols, and the counter bases bound so far.
struct Bindings {
    /// `project:P` → the fake's project id, and so on.
    symbols: BTreeMap<String, String>,
    bases: BTreeMap<String, i64>,
}

impl Bindings {
    fn symbol_text(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (sym, id) in &self.symbols {
            out = out.replace(&format!("{{{{{sym}}}}}"), id);
        }
        out
    }

    /// `v` with every symbol replaced by its id; counters and `{{ts}}` stay.
    fn with_symbols(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.symbol_text(s)),
            Value::Array(a) => Value::Array(a.iter().map(|e| self.with_symbols(e)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, e)| (k.clone(), self.with_symbols(e)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn base(&self, name: &str) -> i64 {
        *self.bases.get(name).unwrap_or_else(|| {
            panic!("counter {name} is used before step 0 captured it")
        })
    }

    /// `v` as concrete JSON: symbols replaced, a counter rendered as the
    /// integer it stands for, `{{ts}}` as a fixed timestamp.
    fn concrete(&self, v: &Value) -> Value {
        match self.with_symbols(v) {
            Value::String(s) => match token_of(&s) {
                Some(Token::Ts) => json!(RENDERED_TS),
                Some(Token::Counter(name, k)) => json!(self.base(&name) + k),
                None => Value::String(s),
            },
            Value::Array(a) => Value::Array(a.iter().map(|e| self.concrete(e)).collect()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, e)| (k.clone(), self.concrete(e)))
                    .collect(),
            ),
            other => other,
        }
    }
}

// ── The comparer ───────────────────────────────────────────────────────────

/// One reported line under a step: the text after the four-space indent.
#[derive(Debug, PartialEq)]
struct Difference(String);

impl Difference {
    fn values(pointer: &str, hub: &str, double: &str) -> Self {
        let pointer = if pointer.is_empty() { "(body)" } else { pointer };
        Difference(format!("{pointer}: hub {hub}, double {double}"))
    }

    fn note(label: &str, text: &str) -> Self {
        Difference(format!("{label}: {text}"))
    }
}

fn show(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 160 {
        let cut: String = s.chars().take(160).collect();
        format!("{cut}...")
    } else {
        s
    }
}

/// Every pointer at which the double's answer differs from the hub's. `hub`
/// has its symbols substituted already; its counters and `{{ts}}` are checked
/// as rules, everything else for equality.
fn diff_body(
    pointer: &str,
    hub: &Value,
    double: &Value,
    bases: &BTreeMap<String, i64>,
    out: &mut Vec<Difference>,
) {
    match (hub, double) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let child = format!("{pointer}/{}", escape_segment(k));
                match (a.get(k), b.get(k)) {
                    (Some(x), Some(y)) => diff_body(&child, x, y, bases, out),
                    (Some(x), None) => out.push(Difference::values(&child, &show(x), MISSING)),
                    (None, Some(y)) => out.push(Difference::values(&child, MISSING, &show(y))),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                let child = format!("{pointer}/{i}");
                match (a.get(i), b.get(i)) {
                    (Some(x), Some(y)) => diff_body(&child, x, y, bases, out),
                    (Some(x), None) => out.push(Difference::values(&child, &show(x), MISSING)),
                    (None, Some(y)) => out.push(Difference::values(&child, MISSING, &show(y))),
                    (None, None) => {}
                }
            }
        }
        (Value::String(s), double) => match token_of(s) {
            Some(Token::Ts) => {
                let ok = double
                    .as_str()
                    .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok());
                if !ok {
                    out.push(Difference::values(
                        pointer,
                        "{{ts}} (an RFC 3339 string)",
                        &show(double),
                    ));
                }
            }
            Some(Token::Counter(name, k)) => {
                let base = *bases
                    .get(&name)
                    .unwrap_or_else(|| panic!("counter {name} is used before step 0 captured it"));
                if double.as_i64() != Some(base + k) {
                    out.push(Difference::values(
                        pointer,
                        &format!("{s} = {}", base + k),
                        &show(double),
                    ));
                }
            }
            None => {
                if hub != double {
                    out.push(Difference::values(pointer, &show(hub), &show(double)));
                }
            }
        },
        (hub, double) => {
            if hub != double {
                out.push(Difference::values(pointer, &show(hub), &show(double)));
            }
        }
    }
}

/// One step's header line and what differed in it.
struct StepReport {
    header: String,
    lines: Vec<Difference>,
}

/// The fixture's report, or `None` when the double answered like the hub.
fn format_report(fixture: &str, commit: &str, steps: &[StepReport]) -> Option<String> {
    let total: usize = steps.iter().map(|s| s.lines.len()).sum();
    if total == 0 {
        return None;
    }
    let mut msg = format!("hub contract {fixture} (hub {commit}): {total} difference(s)\n");
    for step in steps.iter().filter(|s| !s.lines.is_empty()) {
        msg.push_str(&format!("  {}\n", step.header));
        for line in &step.lines {
            msg.push_str(&format!("    {}\n", line.0));
        }
    }
    msg.push_str(
        "The hub is right: change the fake hub to answer like the recorded answer. A wire or \
         classify line is a defect of the client, not of the fake.",
    );
    Some(msg)
}

// ── The wire check ─────────────────────────────────────────────────────────

fn parses_as<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, String> {
    serde_json::from_value::<T>(body.clone()).map_err(|e| e.to_string())
}

/// Parses the hub's concrete body into the type the client reads it as.
fn check_wire_ok(wire: &str, body: &Value) -> Result<(), String> {
    match wire {
        "HoldersSnapshotWire" => parses_as::<HoldersSnapshotWire>(body).map(drop),
        "HoldersReportReplyWire" => parses_as::<HoldersReportReplyWire>(body).map(drop),
        "VersionsReplyWire" => {
            let reply = parses_as::<VersionsReplyWire>(body)?;
            match reply
                .results
                .iter()
                .find(|r| r.status == VersionStatus::Unknown)
            {
                Some(r) => Err(format!("result for {} has a status this client does not know", r.uuid)),
                None => Ok(()),
            }
        }
        "AnnounceFramesWire" => parses_as::<AnnounceFramesWire>(body).map(drop),
        "NewVersionWire" => parses_as::<NewVersionWire>(body).map(drop),
        "ManifestPageWire" => parses_as::<ManifestPageWire>(body).map(drop),
        "Vec<MyProjectWire>" => parses_as::<Vec<MyProjectWire>>(body).map(drop),
        "PublishingDeviceSwitchWire" => parses_as::<PublishingDeviceSwitchWire>(body).map(drop),
        "none" => {
            if body.is_null() {
                Ok(())
            } else {
                Err(format!("the recorded body is {}, not empty", show(body)))
            }
        }
        other => panic!("wire {other} is not implemented by the replayer"),
    }
}

/// Runs the hub's recorded error answer through the client's own classifier
/// and checks the variant, and its fields, against what the fixture names.
async fn check_wire_error(variant: &str, status: u16, body: &Value) -> Result<(), String> {
    let bytes = match body {
        Value::Null => Vec::new(),
        Value::String(s) => s.clone().into_bytes(),
        other => serde_json::to_vec(other).expect("a JSON value serializes"),
    };
    let text = match body {
        Value::String(s) => s.trim().to_string(),
        other => other
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    };
    let device = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let got = classify_for_contract(status, bytes).await;
    let ok = match (variant, &got) {
        ("Unauthorized", AccountClientError::Unauthorized)
        | ("Forbidden", AccountClientError::Forbidden)
        | ("RateLimited", AccountClientError::RateLimited)
        | ("CollabApiOutdated", AccountClientError::CollabApiOutdated)
        | ("SessionGone", AccountClientError::SessionGone) => true,
        ("Http", AccountClientError::Http { status: s, message }) => {
            *s == status && message.contains(&text)
        }
        ("Gone", AccountClientError::Gone(e)) => *e == text,
        ("VersionConflict", AccountClientError::VersionConflict { content_version }) => {
            body.get("contentVersion").and_then(Value::as_i64) == Some(i64::from(*content_version))
        }
        (
            "PublishingDevice",
            AccountClientError::PublishingDevice {
                device_id,
                device_name,
            },
        )
        | (
            "NotPublishingDevice",
            AccountClientError::NotPublishingDevice {
                device_id,
                device_name,
            },
        ) => {
            device("deviceId").as_deref().unwrap_or_default() == device_id
                && device("deviceName") == *device_name
        }
        ("Unauthorized" | "Forbidden" | "RateLimited" | "CollabApiOutdated" | "SessionGone"
        | "Http" | "Gone" | "VersionConflict" | "PublishingDevice" | "NotPublishingDevice", _) => {
            false
        }
        (other, _) => panic!("error variant {other} is not implemented by the replayer"),
    };
    if ok {
        Ok(())
    } else {
        Err(format!("the client classifies the hub's answer as {got:?}"))
    }
}

async fn check_wire(wire: &str, status: u16, body: &Value) -> Result<(), String> {
    match wire.strip_prefix("error:") {
        Some(variant) => {
            if status < 400 {
                return Err(format!("status {status} is not an error"));
            }
            check_wire_error(variant, status, body).await
        }
        None => check_wire_ok(wire, body),
    }
}

// ── Building the given and replaying ───────────────────────────────────────

fn assert_keys(v: &Value, expected: &[&str], what: &str) {
    let mut got: Vec<&str> = v
        .as_object()
        .unwrap_or_else(|| panic!("fixture {what} is not an object"))
        .keys()
        .map(String::as_str)
        .collect();
    got.sort_unstable();
    let mut want = expected.to_vec();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "fixture {what} carries keys this replayer does not build"
    );
}

fn uuid_like(prefix: char, n: usize) -> String {
    format!("{prefix}0000000-0000-4000-8000-{n:012}")
}

fn pubkey_of(seed: u8) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode([seed; 32])
}

/// A fake hub holding the fixture's `given`, the ids bound to its symbols, and
/// the token of every device.
struct Built {
    hub: FakeHub,
    bindings: Bindings,
    /// device symbol → bearer token.
    tokens: BTreeMap<String, String>,
}

async fn build(given: &Value) -> Built {
    assert_keys(
        given,
        &["epoch", "project", "accounts"],
        "given",
    );
    let project = &given["project"];
    assert_keys(
        project,
        &[
            "sym",
            "title",
            "slug",
            "requireApproval",
            "gateVersion",
            "dictionaryVersion",
        ],
        "given.project",
    );
    assert_eq!(project["gateVersion"], json!(1), "the replayer builds gate version 1 only");
    assert_eq!(
        project["dictionaryVersion"],
        json!(1),
        "the replayer builds dictionary version 1 only"
    );

    let hub = FakeHub::start().await;
    let mut symbols = BTreeMap::new();
    let project_id = uuid_like('c', 1);
    symbols.insert(
        format!("project:{}", project["sym"].as_str().expect("project.sym")),
        project_id.clone(),
    );

    let mut tokens = BTreeMap::new();
    let mut members: Vec<(String, String, bool)> = Vec::new();
    let mut caps: Vec<(String, Vec<String>)> = Vec::new();
    let mut device_n = 0;
    for (i, account) in given["accounts"]
        .as_array()
        .expect("given.accounts")
        .iter()
        .enumerate()
    {
        assert_keys(
            account,
            &[
                "sym",
                "memberName",
                "coordinator",
                "dataRole",
                "trusted",
                "govCaps",
                "devices",
            ],
            "given.accounts[]",
        );
        assert_eq!(account["trusted"], json!(false), "the replayer builds untrusted members only");
        let sym = account["sym"].as_str().expect("account.sym");
        let account_id = uuid_like('a', i + 1);
        symbols.insert(format!("account:{sym}"), account_id.clone());
        for device in account["devices"].as_array().expect("account.devices") {
            assert_keys(
                device,
                &["sym", "pubkeySeed", "pubkey", "name", "relayUrl"],
                "given.accounts[].devices[]",
            );
            device_n += 1;
            let dsym = device["sym"].as_str().expect("device.sym");
            let seed = device["pubkeySeed"].as_u64().expect("device.pubkeySeed");
            let pubkey = device["pubkey"].as_str().expect("device.pubkey");
            assert_eq!(
                pubkey,
                pubkey_of(u8::try_from(seed).expect("pubkeySeed is 1..=255")),
                "device {dsym}: pubkey is not base64 of [seed; 32]"
            );
            let device_id = uuid_like('d', device_n);
            symbols.insert(format!("device:{dsym}"), device_id.clone());
            let token = format!("tok-{dsym}");
            hub.add_account(
                &token,
                &account_id,
                account["memberName"].as_str().expect("memberName"),
                pubkey,
                device["relayUrl"].as_str(),
            );
            hub.add_device(
                &account_id,
                pubkey,
                &device_id,
                device["name"].as_str().expect("device.name"),
                None,
            );
            tokens.insert(dsym.to_string(), token);
        }
        members.push((
            account_id.clone(),
            account["dataRole"].as_str().expect("dataRole").to_string(),
            account["coordinator"].as_bool().expect("coordinator"),
        ));
        let gov: Vec<String> = account["govCaps"]
            .as_array()
            .expect("govCaps")
            .iter()
            .map(|c| c.as_str().expect("a cap is a string").to_string())
            .collect();
        if !gov.is_empty() {
            caps.push((account_id, gov));
        }
    }

    let member_refs: Vec<(&str, &str, bool)> = members
        .iter()
        .map(|(a, r, c)| (a.as_str(), r.as_str(), *c))
        .collect();
    hub.add_project(
        &project_id,
        project["slug"].as_str().expect("project.slug"),
        &member_refs,
        project["requireApproval"].as_bool().expect("requireApproval"),
    );
    {
        let mut st = hub.state.lock().expect("fake hub state");
        st.epoch = given["epoch"].as_str().expect("given.epoch").to_string();
        st.projects
            .get_mut(&project_id)
            .expect("the project just added")
            .title = project["title"].as_str().expect("project.title").to_string();
    }
    hub.set_thresholds_version(&project_id, 1);
    for (account_id, gov) in &caps {
        let refs: Vec<&str> = gov.iter().map(String::as_str).collect();
        hub.set_caps(&project_id, account_id, &refs);
    }

    Built {
        hub,
        bindings: Bindings {
            symbols,
            bases: BTreeMap::new(),
        },
        tokens,
    }
}

fn step_header(step: &Value) -> String {
    format!(
        "step {} {} {} as {} - {}",
        step["n"],
        step["method"].as_str().unwrap_or("?"),
        step["path"].as_str().unwrap_or("?"),
        step["who"].as_str().unwrap_or("nobody"),
        step["note"].as_str().unwrap_or("")
    )
}

/// Replays `doc` on a fresh fake and returns the report, `None` when the fake
/// answered like the hub at every step.
async fn replay(doc: &Value) -> Option<String> {
    assert_eq!(doc["format"], json!(1), "unknown fixture format");
    let fixture = doc["fixture"].as_str().expect("fixture name").to_string();
    let Built {
        hub,
        mut bindings,
        tokens,
    } = build(&doc["given"]).await;
    let client = reqwest::Client::new();
    let mut reports = Vec::new();

    for (index, step) in doc["steps"].as_array().expect("steps").iter().enumerate() {
        assert_eq!(step["n"], json!(index), "steps are numbered from 0 in order");
        let mut lines = Vec::new();

        let path = bindings.symbol_text(step["path"].as_str().expect("step.path"));
        let method = reqwest::Method::from_bytes(step["method"].as_str().expect("method").as_bytes())
            .expect("a valid method");
        let mut request = client.request(method, format!("{}{path}", hub.uri()));
        if let Some(who) = step["who"].as_str() {
            let token = tokens
                .get(who)
                .unwrap_or_else(|| panic!("step {index}: no device {who} in the given"));
            request = request.bearer_auth(token);
        }
        if !step["body"].is_null() {
            request = request.json(&bindings.concrete(&step["body"]));
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|e| panic!("step {index}: the request failed: {e}"));
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .await
            .unwrap_or_else(|e| panic!("step {index}: reading the answer failed: {e}"));
        let double: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };

        let hub_status = step["status"].as_u64().expect("step.status");
        if u64::from(status) != hub_status {
            lines.push(Difference::values(
                "status",
                &hub_status.to_string(),
                &status.to_string(),
            ));
        }

        let mut captured_all = true;
        for (name, pointer) in step["capture"].as_object().expect("step.capture") {
            let pointer = pointer.as_str().expect("a capture is a pointer");
            match double.pointer(pointer).and_then(Value::as_i64) {
                Some(v) => {
                    bindings.bases.insert(name.clone(), v);
                }
                None => {
                    captured_all = false;
                    lines.push(Difference::note(
                        &format!("capture {name}"),
                        &format!(
                            "no integer at {pointer} in the double's answer ({})",
                            double.pointer(pointer).map_or(MISSING.to_string(), show)
                        ),
                    ));
                }
            }
        }

        if captured_all {
            let mut hub_body = bindings.with_symbols(&step["response"]);
            let mut double_body = double.clone();
            effective(doc, index, &mut hub_body);
            effective(doc, index, &mut double_body);
            diff_body("", &hub_body, &double_body, &bindings.bases, &mut lines);

            let wire = step["wire"].as_str().expect("step.wire");
            let concrete = bindings.concrete(&step["response"]);
            if let Err(e) = check_wire(wire, hub_status as u16, &concrete).await {
                lines.push(Difference::note(&format!("wire {wire}"), &e));
            }
        }

        reports.push(StepReport {
            header: step_header(step),
            lines,
        });
        if !captured_all {
            break;
        }
    }

    format_report(&fixture, &hub_commit(), &reports)
}

async fn replay_fixture(stem: &str) {
    let doc = load(stem);
    assert_eq!(doc["fixture"], json!(stem), "the file's own name differs from its stem");
    if let Some(report) = replay(&doc).await {
        panic!("{report}");
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hub_contract_01_snapshot_after_announce() {
        replay_fixture("01-snapshot_after_announce").await;
    }

    #[test]
    fn every_fixture_is_replayed() {
        let mut on_disk: Vec<String> = std::fs::read_dir(fixtures_dir())
            .expect("the fixtures folder exists")
            .filter_map(|e| {
                let path = e.expect("a directory entry").path();
                (path.extension().and_then(|x| x.to_str()) == Some("json"))
                    .then(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string))
                    .flatten()
            })
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> = FIXTURES.iter().map(|s| s.to_string()).collect();
        listed.sort();
        assert_eq!(
            on_disk, listed,
            "FIXTURES and the files in tests/fixtures/hub_contract differ"
        );
        let commit = std::fs::read_to_string(fixtures_dir().join("HUB_COMMIT"))
            .expect("HUB_COMMIT is next to the fixtures");
        assert_eq!(
            commit.lines().count(),
            1,
            "HUB_COMMIT is one line: <hub sha> <commit date>"
        );
    }

    #[test]
    fn the_comparer_names_step_pointer_and_both_values() {
        let hub = json!({
            "a": 1,
            "b": {"c": "x", "d": 2},
            "n": "{{v0+3}}",
            "t": "{{ts}}",
            "z": 5
        });
        let double = json!({
            "a": 1,
            "b": {"c": "y"},
            "n": 6,
            "t": "2026-11-12T10:00:00.123456789+00:00",
            "z": 5
        });
        let mut bases = BTreeMap::new();
        bases.insert("v0".to_string(), 3);
        let mut found = Vec::new();
        diff_body("", &hub, &double, &bases, &mut found);
        assert_eq!(
            found,
            vec![
                Difference("/b/c: hub \"x\", double \"y\"".to_string()),
                Difference("/b/d: hub 2, double <missing>".to_string()),
            ]
        );

        // A counter off by one and a timestamp that is not one.
        let double = json!({
            "a": 1, "b": {"c": "x", "d": 2}, "n": 7, "t": "yesterday", "z": 5
        });
        let mut found = Vec::new();
        diff_body("", &hub, &double, &bases, &mut found);
        assert_eq!(
            found,
            vec![
                Difference("/n: hub {{v0+3}} = 6, double 7".to_string()),
                Difference("/t: hub {{ts}} (an RFC 3339 string), double \"yesterday\"".to_string()),
            ]
        );

        let report = format_report(
            "03-example",
            "abc123",
            &[
                StepReport {
                    header: "step 0 GET /x as A1 - before".to_string(),
                    lines: Vec::new(),
                },
                StepReport {
                    header: "step 1 GET /y as B1 - after".to_string(),
                    lines: found,
                },
            ],
        )
        .expect("two differences are a report");
        assert!(report.starts_with(
            "hub contract 03-example (hub abc123): 2 difference(s)\n  step 1 GET /y as B1 - after\n    /n: hub {{v0+3}} = 6, double 7\n"
        ));
        assert!(!report.contains("step 0"));
    }
}
