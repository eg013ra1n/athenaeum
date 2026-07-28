use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SETTINGS_KEY: &str = "logging.config";
const LEVELS: [&str; 4] = ["error", "warn", "info", "debug"]; // trace is env-only by spec

/// Noisy third-party transport crates quieted to `warn` as a baseline in every
/// generated filter. Both hosts run iroh in-process (the sync receiver), and its
/// transport/relay/blob internals plus network-probe deps (`portmapper`,
/// `netwatch`, `noq_udp`, `net_report`) log so verbosely at `info` that they
/// bury our own events (a single Perseus evening run produced ~71k
/// `iroh::socket::transports` span-close events). This is only the DEFAULT
/// baseline — the user's per-module overrides below still apply on top, and
/// `ATHENAEUM_LOG` (which bypasses `to_directives` entirely) can raise them,
/// e.g. `ATHENAEUM_LOG=info,iroh=debug`.
const THIRD_PARTY_QUIET: [&str; 7] = [
    "iroh",
    "iroh_relay",
    "iroh_blobs",
    "net_report",
    "portmapper",
    "netwatch",
    "noq_udp",
];

/// Network-probe targets demoted further, to `error`, in the baseline. Behind a
/// symmetric-NAT / multi-WAN network (any office) `iroh::net_report` warns
/// "IPv4 address detected by QAD varies by destination" on every probe round —
/// every ~25 s, forever — and the verdict is advisory (path quality is already
/// visible in our own `transport traffic` telemetry). Emitted AFTER the
/// `THIRD_PARTY_QUIET` directives so the duplicate `net_report` target resolves
/// last-wins to `error`. Still raisable from the UI: the `transport` module
/// repeats both targets, and it must — `iroh=debug` alone could not beat
/// `iroh::net_report=error`, because `EnvFilter` specificity outranks order.
const THIRD_PARTY_ERROR_ONLY: [&str; 2] = ["net_report", "iroh::net_report"];

/// UI module key -> tracing filter targets.
const MODULE_TARGETS: [(&str, &[&str]); 5] = [
    ("scanner", &["athenaeum_core::scanner"]),
    ("solver", &["athenaeum_core::plate_solve", "solvemyastro"]),
    ("calibration", &["athenaeum_core::calibration"]),
    (
        "archive",
        &["athenaeum_core::archive", "athenaeum_core::file_op"],
    ),
    // The one switch that lifts the iroh transport back out of the
    // `THIRD_PARTY_QUIET` baseline: that baseline is appended BEFORE the module
    // overrides and `EnvFilter` is last-directive-wins, so these win. Without it
    // a transport fault is only reachable through `ATHENAEUM_LOG` — which is how
    // a fleet-wide NAT-traversal outage stayed invisible for months (every QUIC
    // address-discovery probe was timing out at DEBUG under `iroh=warn`). Every
    // quieted target is repeated here (a subset would leave part of the transport
    // silent) plus our own side of the seam, so one switch covers both;
    // `transport_module_covers_every_quieted_target` fails if the lists drift.
    // `iroh::net_report` must be repeated verbatim: it is demoted to `error` by
    // `THIRD_PARTY_ERROR_ONLY`, and only an equally-specific directive can win
    // it back (specificity outranks order in `EnvFilter`).
    (
        "transport",
        &[
            "iroh",
            "iroh_relay",
            "iroh_blobs",
            "net_report",
            "iroh::net_report",
            "portmapper",
            "netwatch",
            "noq_udp",
            "athenaeum_core::sharing::iroh",
        ],
    ),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", default)]
pub struct LoggingConfig {
    pub level: String,
    pub modules: BTreeMap<String, String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            modules: BTreeMap::new(),
        }
    }
}

impl LoggingConfig {
    pub fn to_directives(&self) -> String {
        let base = if LEVELS.contains(&self.level.as_str()) {
            self.level.as_str()
        } else {
            "info"
        };
        let mut out = base.to_string();
        // Baseline: quiet noisy third-party transport crates (iroh & its network
        // deps). Appended before the user's module overrides so a future iroh
        // module key would win over this via EnvFilter's last-directive-wins.
        for t in THIRD_PARTY_QUIET {
            out.push_str(&format!(",{t}=warn"));
        }
        for t in THIRD_PARTY_ERROR_ONLY {
            out.push_str(&format!(",{t}=error"));
        }
        for (key, level) in &self.modules {
            if !LEVELS.contains(&level.as_str()) {
                continue;
            }
            if let Some((_, targets)) = MODULE_TARGETS.iter().find(|(k, _)| k == key) {
                for t in *targets {
                    out.push_str(&format!(",{t}={level}"));
                }
            }
        }
        out
    }

    /// Reject a config with an out-of-range top-level `level` or an
    /// out-of-range per-module level, plus a belt-and-suspenders check that
    /// the resulting directive string actually parses as an `EnvFilter`.
    ///
    /// Unknown module *keys* (e.g. `"bogus"`) are intentionally NOT an error
    /// here — `to_directives()` already skips them silently by design (see
    /// `unknown_module_key_is_skipped_not_fatal`), so rejecting them at the
    /// command boundary would be inconsistent with that established
    /// behavior. Only level *values* (top-level and per-module) are checked
    /// against the accepted set.
    pub fn validate(&self) -> Result<(), String> {
        if !LEVELS.contains(&self.level.as_str()) {
            return Err(format!(
                "invalid level {:?}; expected one of {:?}",
                self.level, LEVELS
            ));
        }
        for (module, level) in &self.modules {
            if !LEVELS.contains(&level.as_str()) {
                return Err(format!(
                    "invalid level {:?} for module {:?}; expected one of {:?}",
                    level, module, LEVELS
                ));
            }
        }
        // Defense-in-depth only: unreachable as a distinct rejection once the LEVELS checks pass.
        self.to_directives()
            .parse::<tracing_subscriber::EnvFilter>()
            .map_err(|e| format!("invalid logging directives: {e}"))?;
        Ok(())
    }
}

/// Response body for `get_logging_config` — the effective config plus
/// whether the `ATHENAEUM_LOG` env override is active (in which case
/// `apply_config` no-ops and the UI should show the config as read-only).
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct LoggingConfigResponse {
    pub config: LoggingConfig,
    pub env_override_active: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `,iroh=warn,...,iroh::net_report=error` baseline suffix that
    /// `to_directives()` appends after the base level (see `THIRD_PARTY_QUIET`
    /// + `THIRD_PARTY_ERROR_ONLY`), so assertions read against the same source
    /// of truth as the implementation.
    fn quiet_suffix() -> String {
        let warns: String = THIRD_PARTY_QUIET.iter().map(|t| format!(",{t}=warn")).collect();
        let errors: String = THIRD_PARTY_ERROR_ONLY
            .iter()
            .map(|t| format!(",{t}=error"))
            .collect();
        format!("{warns}{errors}")
    }

    #[test]
    fn default_config_is_info_plus_third_party_quiet() {
        assert_eq!(
            LoggingConfig::default().to_directives(),
            format!("info{}", quiet_suffix())
        );
    }
    #[test]
    fn third_party_transport_crates_default_to_warn() {
        // The whole point of this change: iroh & its network-probe deps are
        // quieted to warn in the default filter so their span-close spam can't
        // bury our own info events.
        let d = LoggingConfig::default().to_directives();
        for t in THIRD_PARTY_QUIET {
            assert!(
                d.contains(&format!("{t}=warn")),
                "default filter must quiet {t} to warn; got {d:?}"
            );
        }
        // The default directives must still parse as a valid EnvFilter.
        assert!(d.parse::<tracing_subscriber::EnvFilter>().is_ok());
    }
    #[test]
    fn module_overrides_map_to_targets() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "warn".into();
        cfg.modules.insert("scanner".into(), "debug".into());
        cfg.modules.insert("solver".into(), "debug".into());
        // solver expands to BOTH the core plate_solve target and the solvemyastro
        // crate; the third-party quiet baseline sits between base and overrides.
        assert_eq!(
            cfg.to_directives(),
            format!(
                "warn{},athenaeum_core::scanner=debug,athenaeum_core::plate_solve=debug,solvemyastro=debug",
                quiet_suffix()
            )
        );
    }
    #[test]
    fn transport_module_overrides_the_third_party_quiet_baseline() {
        // The baseline quiets `iroh` & friends to warn; the `transport` override
        // must come AFTER it in the directive string, because EnvFilter resolves
        // duplicates last-directive-wins. Order is the whole mechanism here.
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("transport".into(), "debug".into());
        let d = cfg.to_directives();
        let quiet_at = d.find("iroh=warn").expect("baseline quiets iroh");
        let debug_at = d.find("iroh=debug").expect("transport override raises iroh");
        assert!(
            quiet_at < debug_at,
            "the override must follow the baseline, got {d:?}"
        );
        assert!(d.parse::<tracing_subscriber::EnvFilter>().is_ok(), "{d:?}");
    }

    #[test]
    fn transport_module_covers_every_quieted_target() {
        // Drift guard: a target quieted by the baseline but missing from the
        // `transport` module would stay silent with no way to raise it from the
        // UI — exactly the blind spot this module exists to remove.
        let (_, targets) = MODULE_TARGETS
            .iter()
            .find(|(k, _)| *k == "transport")
            .expect("transport module key");
        for t in THIRD_PARTY_QUIET.iter().chain(THIRD_PARTY_ERROR_ONLY.iter()) {
            assert!(
                targets.contains(t),
                "{t} is quieted by the baseline but not raisable via the transport module"
            );
        }
    }

    /// Functional pin for the `net_report` demotion — string asserts can't
    /// catch `EnvFilter` specificity semantics, so run events through a real
    /// filter. Default baseline: an `iroh::net_report` WARN is suppressed while
    /// its ERROR and an ordinary `iroh` WARN still pass. With the `transport`
    /// module raised to debug, the same WARN passes again (the equally-specific
    /// `iroh::net_report=debug` beats the baseline's `=error` last-wins).
    #[test]
    fn net_report_warn_suppressed_by_default_but_raisable_via_transport() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::{layer::SubscriberExt, EnvFilter};

        #[derive(Clone, Default)]
        struct Seen(Arc<Mutex<Vec<(String, String)>>>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Seen {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _ctx: tracing_subscriber::layer::Context<'_, S>,
            ) {
                self.0.lock().unwrap().push((
                    event.metadata().target().to_string(),
                    event.metadata().level().to_string(),
                ));
            }
        }

        let run = |cfg: &LoggingConfig| -> Vec<(String, String)> {
            let seen = Seen::default();
            let filter = EnvFilter::new(cfg.to_directives());
            let subscriber = tracing_subscriber::registry().with(filter).with(seen.clone());
            // Macro callsites cache subscriber interest per process; force a
            // recompute so the second run doesn't reuse the first filter's verdict.
            tracing::subscriber::with_default(subscriber, || {
                tracing::callsite::rebuild_interest_cache();
                tracing::warn!(target: "iroh::net_report::report", "varies by destination");
                tracing::error!(target: "iroh::net_report::report", "probe failed");
                tracing::warn!(target: "iroh::endpoint", "other transport warn");
            });
            let events = seen.0.lock().unwrap().clone();
            events
        };

        let default_events = run(&LoggingConfig::default());
        assert!(
            !default_events.contains(&("iroh::net_report::report".into(), "WARN".into())),
            "net_report WARN must be suppressed by default; got {default_events:?}"
        );
        assert!(
            default_events.contains(&("iroh::net_report::report".into(), "ERROR".into())),
            "net_report ERROR must still pass; got {default_events:?}"
        );
        assert!(
            default_events.contains(&("iroh::endpoint".into(), "WARN".into())),
            "ordinary iroh WARNs must stay in the baseline; got {default_events:?}"
        );

        let mut raised = LoggingConfig::default();
        raised.modules.insert("transport".into(), "debug".into());
        let raised_events = run(&raised);
        assert!(
            raised_events.contains(&("iroh::net_report::report".into(), "WARN".into())),
            "transport=debug must re-raise net_report WARNs; got {raised_events:?}"
        );
    }

    #[test]
    fn unknown_module_key_is_skipped_not_fatal() {
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("bogus".into(), "debug".into());
        assert_eq!(cfg.to_directives(), format!("info{}", quiet_suffix()));
    }
    #[test]
    fn invalid_level_falls_back_to_info() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "chatty".into();
        assert_eq!(cfg.to_directives(), format!("info{}", quiet_suffix()));
    }

    #[test]
    fn validate_accepts_default_config() {
        assert!(LoggingConfig::default().validate().is_ok());
    }

    #[test]
    fn validate_accepts_known_module_overrides() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "warn".into();
        cfg.modules.insert("scanner".into(), "debug".into());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_invalid_top_level_level() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "chatty".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_invalid_module_level() {
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("scanner".into(), "chatty".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_accepts_unknown_module_key_with_valid_level() {
        // Unknown module keys are skipped by to_directives(), not rejected —
        // validate() must stay consistent with that (see
        // unknown_module_key_is_skipped_not_fatal above).
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("bogus".into(), "debug".into());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_trace_level() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "trace".into();
        assert!(cfg.validate().is_err());
        let mut cfg2 = LoggingConfig::default();
        cfg2.modules.insert("scanner".into(), "trace".into());
        assert!(cfg2.validate().is_err());
    }
}
