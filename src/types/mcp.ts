// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type McpStatus = { 
/**
 * The setting: assistant access is turned on (it may still be starting, or served by another instance).
 */
enabled: boolean, 
/**
 * `off` | `starting` | `serving` | `elsewhere` | `failed`.
 */
state: string, 
/**
 * `athenaeum` (release) or `athenaeum-dev` (debug build): the name an assistant lists this build under.
 */
serverName: string, 
/**
 * Desktop: where the local channel listens (`unix:<path>` or `pipe:<name>`); `null` when nothing listens.
 */
endpoint: string | null, 
/**
 * Web only: the key an assistant presents, shown so it can be copied; never set on the desktop.
 */
token: string | null, 
/**
 * Desktop: where the bridge program is expected (next to the app).
 */
bridgePath: string | null, 
/**
 * Desktop: the bridge program is there.
 */
bridgeFound: boolean, 
/**
 * Desktop: the JSON an assistant's config file takes to start the bridge.
 */
clientConfig: string | null, 
/**
 * Desktop: the shell command that adds the bridge to Claude Code.
 */
claudeCodeCommand: string | null, 
/**
 * Why it is not serving: the channel could not start, or another instance owns it.
 */
lastError: string | null, 
/**
 * Web without `ATHENAEUM_API_KEY`: why turning it on is refused.
 */
blockedReason: string | null, };

