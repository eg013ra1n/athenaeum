//! Assistant access (MCP, plan 2026-11-19): what both hosts' `get_mcp_status`, `set_mcp_enabled` and
//! `regenerate_mcp_token` commands call, and the plan window's `get_mcp_plan` and `answer_mcp_plan`. The logic is `McpHost`'s (`mcp/lifecycle.rs`); these are the command-facing
//! names. `settings::set_setting` refuses `mcp.enabled`, so `set_mcp_enabled` is the only way to turn it on or off.

use std::path::Path;
use std::sync::Arc;

use crate::api::ApiError;
use crate::mcp::claude_desktop::{self, ClaudeDesktopAdded};
use crate::mcp::{claude_desktop_config_dir, McpHost, McpPlanView, McpStatus, PlanAnswer};

/// What the Settings page shows: on or off, the state of the channel, the key (web), the bridge and the
/// configuration to copy (desktop).
pub fn get_mcp_status(host: &McpHost) -> McpStatus {
    host.status()
}

/// Turns assistant access on or off. On the web build, on is refused with an `Invalid` error that says why when
/// `ATHENAEUM_API_KEY` is not set.
pub fn set_mcp_enabled(host: &Arc<McpHost>, enabled: bool) -> Result<McpStatus, ApiError> {
    host.set_enabled(enabled)
}

/// Makes a new key and cuts off every connected assistant.
pub fn regenerate_mcp_token(host: &Arc<McpHost>) -> Result<McpStatus, ApiError> {
    host.regenerate_token()
}

/// The plan the window shows: the one by `plan_id`, or the newest when it is `None` (a page that opens later finds the
/// plan that waits). `None` when there is no such plan, because Athenaeum restarted since or it is older than a day.
pub fn get_mcp_plan(host: &McpHost, plan_id: Option<String>) -> Option<McpPlanView> {
    host.plans.view(plan_id.as_deref())
}

/// The window's answer: Apply the user's selection, or Cancel. The one way a plan is applied or cancelled; no MCP tool
/// calls it. An apply returns at once with the plan `applying`; the window learns the end from `mcp-plan-updated`.
pub fn answer_mcp_plan(host: &Arc<McpHost>, plan_id: String, answer: PlanAnswer) -> Result<McpPlanView, ApiError> {
    host.plans.answer(host, &plan_id, answer)
}

/// Merges Athenaeum's entry into the user's Claude desktop configuration (`claude_desktop_config.json`), keeping every
/// other key and server and a `.bak` copy of the old file. `status` is what the desktop's `get_mcp_status` answers, so
/// the entry is the one the copy block shows (the app-data copy of the bridge on Windows and AppImage included).
pub fn add_to_claude_desktop(status: &McpStatus) -> Result<ClaudeDesktopAdded, ApiError> {
    let Some(dir) = claude_desktop_config_dir() else {
        let reason = "Claude desktop was not found for this user.";
        tracing::warn!(reason, "add to claude desktop refused");
        return Err(ApiError::Invalid(reason.to_string()));
    };
    add_to_claude_desktop_at(status, &dir)
}

/// [`add_to_claude_desktop`] over a given Claude desktop folder (the tests' temp folder).
pub fn add_to_claude_desktop_at(status: &McpStatus, dir: &Path) -> Result<ClaudeDesktopAdded, ApiError> {
    let refuse = |reason: &str| {
        tracing::warn!(path = %dir.join(claude_desktop::CONFIG_FILE).display(), reason, "add to claude desktop refused");
        ApiError::Invalid(reason.to_string())
    };
    let entry = status
        .client_config
        .as_deref()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|mut config| config.get_mut("mcpServers")?.get_mut(status.server_name.as_str()).map(serde_json::Value::take));
    let Some(entry) = entry.filter(|_| status.bridge_found) else {
        return Err(refuse("The bridge program (athenaeum-mcp) was not found next to Athenaeum, so there is nothing to add."));
    };
    if status.bridge_path.as_deref().is_some_and(|p| p.contains("/AppTranslocation/")) {
        return Err(refuse(
            "Athenaeum is running from a temporary location (macOS App Translocation), so the bridge's path would change. Move Athenaeum to Applications, start it from there and try again.",
        ));
    }
    claude_desktop::add_entry_in(dir, &status.server_name, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "athenaeum-dev";

    fn status(bridge_found: bool) -> McpStatus {
        let bridge = Path::new("bridge-dir").join("athenaeum-mcp");
        let (config, command) = crate::mcp::desktop_client_config(NAME, &bridge, "dev", Some(Path::new("app-data")));
        McpStatus {
            enabled: true,
            state: "serving".into(),
            server_name: NAME.into(),
            endpoint: None,
            token: None,
            bridge_path: Some(bridge.to_string_lossy().into_owned()),
            bridge_found,
            client_config: Some(config),
            claude_code_command: Some(command),
            claude_desktop_config: None,
            last_error: None,
            blocked_reason: None,
        }
    }

    fn file(dir: &Path) -> std::path::PathBuf {
        dir.join(claude_desktop::CONFIG_FILE)
    }

    fn bak(dir: &Path) -> std::path::PathBuf {
        dir.join(format!("{}.bak", claude_desktop::CONFIG_FILE))
    }

    fn assert_refused(err: ApiError, needle: &str) {
        match err {
            ApiError::Invalid(m) => assert!(m.contains(needle), "{m}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn add_creates_the_file_when_absent() {
        let tmp = tempfile::tempdir().expect("tmp");
        let added = add_to_claude_desktop_at(&status(true), tmp.path()).expect("added");
        assert!(!added.replaced);
        assert_eq!(added.backup, None);
        assert_eq!(added.path, file(tmp.path()).to_string_lossy());
        let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file(tmp.path())).expect("file")).expect("json");
        let (config, _) = crate::mcp::desktop_client_config(NAME, &Path::new("bridge-dir").join("athenaeum-mcp"), "dev", Some(Path::new("app-data")));
        assert_eq!(written, serde_json::from_str::<serde_json::Value>(&config).expect("json"), "the same entry the copy block shows");
        assert!(!bak(tmp.path()).exists());
    }

    #[test]
    fn add_writes_backup_then_file() {
        let tmp = tempfile::tempdir().expect("tmp");
        let old = "{\"preferences\":{\"a\":1},\"mcpServers\":{\"other\":{\"command\":\"x\"}}}";
        std::fs::write(file(tmp.path()), old).expect("seed");
        let added = add_to_claude_desktop_at(&status(true), tmp.path()).expect("added");
        assert!(!added.replaced);
        assert_eq!(added.backup.as_deref(), Some(bak(tmp.path()).to_string_lossy().as_ref()));
        assert_eq!(std::fs::read_to_string(bak(tmp.path())).expect("bak"), old, "the backup is the old bytes");
        let now: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(file(tmp.path())).expect("file")).expect("json");
        assert_eq!(now["preferences"]["a"], 1);
        assert_eq!(now["mcpServers"]["other"]["command"], "x");
        assert!(now["mcpServers"][NAME]["command"].is_string());
        // A second run replaces its own entry and the older backup.
        let again = add_to_claude_desktop_at(&status(true), tmp.path()).expect("again");
        assert!(again.replaced);
        assert_eq!(std::fs::read_to_string(bak(tmp.path())).expect("bak"), std::fs::read_to_string(file(tmp.path())).expect("file"));
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path()).expect("dir").filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
        assert!(leftovers.is_empty(), "no temp file is left behind");
    }

    #[test]
    fn add_refused_leaves_file_byte_identical() {
        for (seed, needle) in [
            (b"{ not json".as_slice(), "not valid JSON"),
            (b"[1,2]".as_slice(), "JSON object"),
            (br#"{"mcpServers":[]}"#.as_slice(), "is not a JSON object"),
            (&[0xff, 0xfe, 0x00][..], "not UTF-8"),
        ] {
            let tmp = tempfile::tempdir().expect("tmp");
            std::fs::write(file(tmp.path()), seed).expect("seed");
            let err = add_to_claude_desktop_at(&status(true), tmp.path()).expect_err("refused");
            assert_refused(err, needle);
            assert_eq!(std::fs::read(file(tmp.path())).expect("file"), seed, "{needle}: the file is untouched");
            assert!(!bak(tmp.path()).exists(), "{needle}: no backup");
            assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 1, "{needle}: nothing else is written");
        }
    }

    #[test]
    fn add_refused_without_bridge_or_when_translocated() {
        let tmp = tempfile::tempdir().expect("tmp");
        assert_refused(add_to_claude_desktop_at(&status(false), tmp.path()).expect_err("no bridge"), "bridge program");
        let mut translocated = status(true);
        translocated.bridge_path = Some("/private/var/folders/x/AppTranslocation/ABC/d/Athenaeum.app/Contents/MacOS/athenaeum-mcp".into());
        assert_refused(add_to_claude_desktop_at(&translocated, tmp.path()).expect_err("translocated"), "App Translocation");
        assert_eq!(std::fs::read_dir(tmp.path()).expect("dir").count(), 0);
    }

    #[test]
    fn add_refused_on_web_status_without_configuration() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut web = status(true);
        web.client_config = None;
        assert_refused(add_to_claude_desktop_at(&web, tmp.path()).expect_err("web"), "bridge program");
    }
}
