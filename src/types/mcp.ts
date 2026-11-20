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

export type McpSource = { 
/**
 * The build: the server name (`athenaeum` / `athenaeum-dev`).
 */
build: string, version: string, 
/**
 * The catalog database's full path.
 */
catalog: string, };

export type PlanState = { "kind": "preparing", done: number, total: number, } | { "kind": "waiting" } | { "kind": "applying" } | { "kind": "applied" } | { "kind": "cancelled" } | { "kind": "replaced" } | { "kind": "expired" } | { "kind": "failed", error: string, };

export type PlanKind = "black_hole" | "metadata" | "plate_solve";

export type PlanColumn = { key: string, label: string, };

export type PlanItem = { 
/**
 * `item:<file_id>` (metadata `item:<frame_id>:<field>`).
 */
id: string, fileName: string, folder: string, 
/**
 * Column key to the text shown in that column.
 */
values: { [key in string]: string }, 
/**
 * What applying does to this item, in words.
 */
action: string, 
/**
 * Why the planner proposes it (a byte check, a rule).
 */
evidence: string | null, 
/**
 * Included in the plan's proposal; the user's tick overrides it.
 */
selected: boolean, 
/**
 * Why this item cannot be included; a locked item never is.
 */
locked: string | null, 
/**
 * May be the group's chosen copy (`chosen` must be one).
 */
candidate: boolean, sizeBytes: number | null, };

export type PlanGroup = { 
/**
 * `group:<n>`.
 */
id: string, title: string, note: string | null, 
/**
 * Set when the user picks one candidate of the group: the label of that choice ("Copy that stays").
 */
chooseOne: string | null, 
/**
 * The id of the chosen candidate.
 */
chosen: string | null, items: Array<PlanItem>, };

export type ReportLine = { itemId: string, fileName: string, folder: string, reason: string, };

export type PlanReport = { applied: Array<ReportLine>, skipped: Array<ReportLine>, failed: Array<ReportLine>, 
/**
 * The run a plan started (a plate-solve batch).
 */
runId: string | null, };

export type McpPlanView = { planId: string, kind: PlanKind, title: string, source: McpSource, state: PlanState, 
/**
 * Set when `state` is `replaced`: the plan that took its place.
 */
replacedBy: string | null, 
/**
 * `YYYY-MM-DD HH:MM:SS` (local time), display only; set once the plan waits.
 */
expiresAt: string | null, columns: Array<PlanColumn>, groups: Array<PlanGroup>, notes: Array<string>, report: PlanReport | null, };

export type GroupSelection = { groupId: string, chosen: string | null, 
/**
 * The ids of the items the user included.
 */
included: Array<string>, };

export type PlanSelection = { groups: Array<GroupSelection>, };

export type PlanAnswer = { "action": "apply", selection: PlanSelection, } | { "action": "cancel" };

