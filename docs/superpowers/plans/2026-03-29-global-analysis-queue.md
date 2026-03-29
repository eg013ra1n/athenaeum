# Global Analysis Queue Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace page-local analysis tracking with a global FIFO queue that persists across navigation, supports cancellation, and shows progress in the sidebar.

**Architecture:** Frontend-managed FIFO queue (React Context). Backend gains cancel_flag support (matching scan/export pattern). One analysis runs at a time; others wait in queue.

**Tech Stack:** Rust (AtomicBool cancel flag, Tauri events), TypeScript/React (Context + Hook), Axum SSE

---

## Task 1: Backend — Add AnalysisHandle to ServiceContext

**Files:**
- Modify: `crates/athenaeum-core/src/services/mod.rs`

- [ ] **Step 1: Add AnalysisHandle and active_analyses field**

```rust
/// Handle to track an active analysis operation.
pub struct AnalysisHandle {
    pub cancel_flag: Arc<AtomicBool>,
}
```

Add to `ServiceContext`:
```rust
pub active_analyses: Arc<Mutex<HashMap<i64, AnalysisHandle>>>,
```

- [ ] **Step 2: Initialize in both backends**

In `crates/athenaeum-tauri/src/lib.rs`, add to ServiceContext init:
```rust
active_analyses: Arc::new(Mutex::new(HashMap::new())),
```

In `crates/athenaeum-web/src/main.rs`, same.

- [ ] **Step 3: Verify compilation**

Run: `cargo check --workspace`

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: add AnalysisHandle with cancel_flag to ServiceContext"
```

---

## Task 2: Backend — Add frame_set_id to progress events + completion event

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/analysis.rs`
- Modify: `crates/athenaeum-web/src/routes/analysis.rs`

- [ ] **Step 1: Add frame_set_id to AnalysisProgressEvent**

In both files, update the struct:
```rust
#[derive(Clone, Serialize)]
struct AnalysisProgressEvent {
    frame_set_id: i64,  // NEW
    current: usize,
    total: usize,
    current_file: String,
    percent: f64,
}
```

Update all `emit`/`send` calls to include `frame_set_id`.

- [ ] **Step 2: Add AnalysisCompleteEvent**

```rust
#[derive(Clone, Serialize)]
struct AnalysisCompleteEvent {
    frame_set_id: i64,
    analyzed: usize,
    skipped: usize,
    failed: usize,
    errors: Vec<String>,
    cancelled: bool,
}
```

- [ ] **Step 3: Add `cancelled` field to AnalyzeFrameSetResult**

```rust
pub struct AnalyzeFrameSetResult {
    pub analyzed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,  // NEW
}
```

- [ ] **Step 4: Emit analysis-complete event at end of analyze_frame_set**

Before the `Ok(AnalyzeFrameSetResult { ... })` return, emit:
```rust
let _ = app_handle.emit("analysis-complete", AnalysisCompleteEvent {
    frame_set_id, analyzed, skipped: 0, failed, errors: errors.clone(), cancelled: false,
});
```

(Web version uses `event_tx.send(SseEvent { ... })`)

- [ ] **Step 5: Commit**

```bash
git commit -m "feat: add frame_set_id to analysis progress events + completion event"
```

---

## Task 3: Backend — Add cancellation to analyze_frame_set

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/analysis.rs`
- Modify: `crates/athenaeum-web/src/routes/analysis.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs` (register command)
- Modify: `crates/athenaeum-web/src/routes/mod.rs` (register route)

- [ ] **Step 1: Register cancel_flag at start of analyze_frame_set**

At the top of the function, before building the analyzer:
```rust
// Guard against concurrent analysis of same frame set
{
    let analyses = state.ctx.active_analyses.lock().unwrap();
    if analyses.contains_key(&frame_set_id) {
        return Err("Analysis already in progress for this frame set".into());
    }
}

let cancel_flag = Arc::new(AtomicBool::new(false));
{
    let mut analyses = state.ctx.active_analyses.lock().unwrap();
    analyses.insert(frame_set_id, AnalysisHandle { cancel_flag: cancel_flag.clone() });
}
```

- [ ] **Step 2: Check cancel_flag in worker loop**

Inside the worker `loop { ... }`, add check before pulling next frame and after analysis:
```rust
if cancel_flag.load(Ordering::Relaxed) { break; }
let item = work.lock().unwrap().next();
// ... analyze ...
if cancel_flag.load(Ordering::Relaxed) { break; }
```

- [ ] **Step 3: Clean up and set cancelled in result**

After `spawn_blocking` returns, determine if cancelled:
```rust
let was_cancelled = cancel_flag.load(Ordering::Relaxed);
// ... existing partition/persist logic ...
// Clean up active_analyses
{
    let mut analyses = state.ctx.active_analyses.lock().unwrap();
    analyses.remove(&frame_set_id);
}
// Emit completion event with cancelled flag
```

- [ ] **Step 4: Add cancel_analysis command**

```rust
#[tauri::command]
pub async fn cancel_analysis(
    frame_set_id: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let analyses = state.ctx.active_analyses.lock().unwrap();
    if let Some(handle) = analyses.get(&frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(())
    } else {
        Err("No active analysis for this frame set".into())
    }
}
```

- [ ] **Step 5: Register in invoke_handler (Tauri) and routes (Web)**

Tauri `lib.rs`: add `commands::cancel_analysis,`
Web `mod.rs`: add `.route("/api/cancel_analysis", post(analysis::cancel_analysis))`

- [ ] **Step 6: Verify and commit**

```bash
cargo check --workspace
git commit -m "feat: add analysis cancellation support with cancel_flag"
```

---

## Task 4: Frontend — Update types

**Files:**
- Modify: `src/types/models.ts`

- [ ] **Step 1: Update AnalysisProgressEvent**

Add `frame_set_id: number` field.

- [ ] **Step 2: Add AnalysisCompleteEvent**

```typescript
export interface AnalysisCompleteEvent {
  frame_set_id: number;
  analyzed: number;
  skipped: number;
  failed: number;
  errors: string[];
  cancelled: boolean;
}
```

- [ ] **Step 3: Add cancelled to AnalyzeFrameSetResult**

Add `cancelled: boolean` field.

- [ ] **Step 4: Commit**

```bash
git commit -m "feat: add analysis queue types — frame_set_id in progress, completion event"
```

---

## Task 5: Frontend — Analysis queue hook

**Files:**
- Create: `src/hooks/useAnalysisProgress.ts`

- [ ] **Step 1: Create the hook**

The hook manages:
- `queue`: array of `{ frameSetId, frameSetName?, force }` items
- `activeAnalyses`: `Map<number, ActiveAnalysis>` tracking state per frame set
- Event listeners for `analysis-progress` and `analysis-complete`
- `processNext()` function that dequeues and invokes one at a time

Key exports:
- `enqueueAnalysis(frameSetId, frameSetName?, force?)` — add to queue
- `cancelAnalysis(frameSetId)` — cancel running or remove from queue
- `cancelAll()` — cancel everything
- `currentAnalysis` — the currently-running analysis
- `queueLength` — pending count
- `hasActiveAnalyses` — any non-complete entries
- `isAnalyzing(frameSetId)` — check specific frame set

- [ ] **Step 2: Commit**

```bash
git commit -m "feat: add useAnalysisProgress hook with FIFO queue"
```

---

## Task 6: Frontend — Analysis context

**Files:**
- Create: `src/contexts/AnalysisProgressContext.tsx`

- [ ] **Step 1: Create context following ExportProgressContext pattern**

```typescript
import { createContext, useContext, ReactNode } from 'react';
import { useAnalysisProgress } from '../hooks/useAnalysisProgress';

type AnalysisProgressContextType = ReturnType<typeof useAnalysisProgress>;
const AnalysisProgressContext = createContext<AnalysisProgressContextType | null>(null);

export function AnalysisProgressProvider({ children }: { children: ReactNode }) {
  const value = useAnalysisProgress();
  return <AnalysisProgressContext.Provider value={value}>{children}</AnalysisProgressContext.Provider>;
}

export function useAnalysisProgressContext() {
  const ctx = useContext(AnalysisProgressContext);
  if (!ctx) throw new Error('useAnalysisProgressContext must be used within AnalysisProgressProvider');
  return ctx;
}
```

- [ ] **Step 2: Commit**

```bash
git commit -m "feat: add AnalysisProgressContext"
```

---

## Task 7: Frontend — Sidebar indicator

**Files:**
- Create: `src/components/AnalysisQueueIndicator.tsx`
- Modify: `src/components/Layout.tsx`

- [ ] **Step 1: Create AnalysisQueueIndicator**

Props: `{ collapsed: boolean }`

Behavior:
- Only renders when `hasActiveAnalyses`
- Collapsed: icon (Activity from lucide) with badge showing queue count
- Expanded: frame set name (truncated) + thin progress bar + queue count + cancel button
- Styled like sidebar nav items

- [ ] **Step 2: Add provider and indicator to Layout.tsx**

Wrap with `AnalysisProgressProvider` in the provider chain.

Insert `<AnalysisQueueIndicator collapsed={collapsed} />` above the collapse button div (before line 69).

- [ ] **Step 3: Commit**

```bash
git commit -m "feat: add analysis queue indicator to sidebar"
```

---

## Task 8: Frontend — Refactor LightsAnalysisView to use context

**Files:**
- Modify: `src/components/LightsAnalysisView.tsx`

- [ ] **Step 1: Replace local state with context**

Remove:
- `const [analyzing, setAnalyzing] = useState(false)`
- `const [analysisProgress, setAnalysisProgress] = useState(...)`

Add:
```typescript
const { enqueueAnalysis, isAnalyzing, cancelAnalysis, activeAnalyses } = useAnalysisProgressContext();
const analyzing = isAnalyzing(frameSetId);
const currentAnalysis = activeAnalyses.get(frameSetId);
const analysisProgress = currentAnalysis?.progress ?? null;
```

- [ ] **Step 2: Simplify handleAnalyzeAll**

Replace the entire function body with:
```typescript
const handleAnalyzeAll = useCallback((force?: boolean) => {
  enqueueAnalysis(frameSetId, frameSetName, force);
}, [frameSetId, frameSetName, enqueueAnalysis]);
```

- [ ] **Step 3: Add cancel button to progress bar**

- [ ] **Step 4: Reload data on completion**

Add useEffect watching `currentAnalysis?.isComplete`:
```typescript
useEffect(() => {
  if (currentAnalysis?.isComplete) {
    loadAnalysisData();
  }
}, [currentAnalysis?.isComplete, loadAnalysisData]);
```

- [ ] **Step 5: Commit**

```bash
git commit -m "refactor: use global analysis context in LightsAnalysisView"
```

---

## Verification

1. `cargo check --workspace` — Rust compiles
2. `npx tsc --noEmit` — TS compiles
3. Trigger analysis on frame set A → sidebar shows progress + page shows detailed progress
4. Navigate away → sidebar still shows progress
5. Navigate back → page picks up progress from context
6. Click cancel → analysis stops
7. Trigger A then B → B queues, starts after A finishes
8. Trigger A while A runs → shows "already in progress"
