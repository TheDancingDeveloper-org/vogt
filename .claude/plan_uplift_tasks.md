# MyDevEnv2 Uplift Implementation Plan

## Overview
This plan addresses five major improvements to MyDevEnv2:
1. Fix copy functionality on the website
2. Address Windows desktop GPU driver crashes (AMD)
3. Uplift editor to first-class IDE-like experience in main window
4. Add persistent session history with search and AI interaction
5. Review and port useful features from open-walnut

## Task 1: Fix Website Copy Functionality

### Root Cause Analysis
Current implementation in `web/src/Terminal.tsx:365-384`:
- Uses `navigator.clipboard.writeText()` with fallback to `execCommand('copy')`
- Clipboard API requires secure context (HTTPS) or localhost
- May fail in PWA/installed contexts or with permissions issues

### Implementation
**File: `web/src/Terminal.tsx`**
- Add clipboard permission check before attempting copy
- Improve error handling with user-visible feedback
- Add a toast notification on successful copy
- Test the fallback path more thoroughly
- Consider adding a "Copy" button in the terminal UI for mobile/touch contexts

**File: `web/src/App.tsx`**
- Ensure copy actions surface errors via the toast system
- Add keyboard shortcut documentation

**Testing:**
- Test on HTTPS production URL
- Test on HTTP localhost during dev
- Test in installed PWA mode (iOS Safari, Android Chrome)
- Test in Capacitor Android WebView
- Test clipboard permissions denied scenario

## Task 2: Windows Desktop App GPU Driver Crashes (AMD)

### Root Cause Analysis
The native GPUI client renders terminals using `fontdue` rasterization to BGRA frames painted onto a `canvas`. AMD GPU driver crashes typically indicate:
- Excessive GPU resource allocation (too many texture uploads)
- Memory leaks in render image lifecycle
- Shader compilation issues with AMD's HLSL compiler

Evidence from code:
- `client/src/ui/terminal_view.rs`: Has `stale_frames` cleanup mechanism
- Uses `RenderImage` for terminal frames
- GPUI shader path requires `fxc.exe` on Windows (AGENTS.md:86-91)

### Implementation
**Phase 1: Immediate Mitigation**
1. **Limit terminal frame retention** - Ensure stale frames are aggressively cleaned
2. **Add frame pooling** - Reuse `RenderImage` allocations instead of creating new ones
3. **Reduce render frequency** - Throttle terminal repaints to 30-60 FPS max
4. **Add GPU memory monitoring** - Log texture allocation/deallocation

**File: `client/src/ui/terminal_view.rs`**
```rust
// Add frame pooling
struct FramePool {
    available: Vec<Arc<RenderImage>>,
    max_size: usize,
}

// Throttle rendering
const MIN_FRAME_INTERVAL_MS: u64 = 16; // ~60 FPS max
let mut last_render = Instant::now();
if last_render.elapsed() < Duration::from_millis(MIN_FRAME_INTERVAL_MS) {
    return; // Skip this frame
}

// Aggressive stale frame cleanup
if self.stale_frames.len() > 2 {
    self.stale_frames.drain(..self.stale_frames.len() - 1);
}
```

**Phase 2: Long-term Fix**
1. **Profile GPU memory usage** - Add telemetry for texture uploads
2. **Consider alternative rendering** - Test with GPUI's native text rendering instead of rasterized frames
3. **AMD-specific testing** - Set up Windows VM with AMD GPU for testing
4. **Fallback mode** - Add software rendering mode when GPU issues detected

**File: `client/src/config.rs`**
- Add `render_mode: "gpu" | "software"` configuration option
- Add automatic fallback on repeated crashes

**Testing:**
- Test on AMD RX 6000/7000 series GPUs
- Monitor GPU memory with tools like GPU-Z
- Long-running session tests (8+ hours)
- Multiple terminal panes stress test

## Task 3: First-Class IDE Editor in Main Window

### Current State
- Editor opens in a separate tab (`/e/<path>`)
- Monaco loads lazily but creates a separate tab UI
- Terminal tabs and editor tabs are siblings in the tab strip
- No side-by-side layout or integrated file explorer in the editor context

### Target Architecture
Create an "IDE mode" that transforms the main window into:
```
┌────────────────────────────────────────┐
│ Tab Strip (terminals, editors, git)   │
├──────────┬─────────────────────────────┤
│  File    │  Editor Area                │
│  Tree    │  ┌─────────────────────────┐│
│          │  │ Monaco Editor           ││
│  • file1 │  │                         ││
│  • file2 │  │ (current file content)  ││
│          │  │                         ││
│          │  └─────────────────────────┘│
│          │  Breadcrumb: workspace/...  │
└──────────┴─────────────────────────────┘
```

### Implementation

**File: `web/src/App.tsx`**
- Add layout mode toggle: `tabbed` (current) vs `ide`
- Persist layout preference in localStorage
- Add layout toggle button in settings

**File: `web/src/EditorWorkspace.tsx` (NEW)**
```tsx
// New component that wraps Editor with integrated file tree
interface EditorWorkspaceProps {
  initialPath?: string;
}

export default function EditorWorkspace(props: EditorWorkspaceProps) {
  const [currentFile, setCurrentFile] = createSignal<string | null>(null);
  const [openFiles, setOpenFiles] = createSignal<string[]>([]);
  
  return (
    <div class="editor-workspace">
      <aside class="editor-sidebar">
        <FileTree onOpen={setCurrentFile} />
      </aside>
      <main class="editor-main">
        <Show when={currentFile()}>
          <Editor path={currentFile()!} />
        </Show>
      </main>
    </div>
  );
}
```

**File: `web/src/Editor.tsx`**
- Add breadcrumb navigation component
- Add file outline panel (Monaco's document symbols)
- Add split editor support (horizontal/vertical)
- Add multi-file tabs within the editor area
- Implement Ctrl+P quick file switcher
- Add integrated terminal panel at bottom (like VS Code)

**File: `web/src/FileTree.tsx`**
- Move from drawer to persistent sidebar in IDE mode
- Add right-click context menu (new file, rename, delete)
- Add drag-and-drop for file operations
- Add file icons based on extension

**File: `web/src/styles.css`**
```css
.editor-workspace {
  display: flex;
  height: 100%;
}

.editor-sidebar {
  width: 250px;
  min-width: 200px;
  max-width: 400px;
  resize: horizontal;
  overflow: auto;
  border-right: 1px solid var(--border);
}

.editor-main {
  flex: 1;
  display: flex;
  flex-direction: column;
}
```

**Native Client Enhancement**
**File: `client/src/ui/mod.rs`**
- Add editor view alongside terminal view
- Integrate file tree into native sidebar
- Reuse Monaco through WebView OR implement native text editor

**Desktop client options:**
1. **Hybrid approach** - Embed WebView for Monaco editor (quick win)
2. **Native approach** - Implement native text editor with GPUI (long-term)

For now, Option 1 is faster - embed the web editor component.

## Task 4: Persistent Session History with Search

### Architecture

```
Session Lifecycle:
Create → Active → Exited → Archived
         ↓
    Continuous logging
         ↓
    session-logs/
      └─ YYYY-MM/
          └─ {session-id}.jsonl
```

### Database Schema (SQLite)

**File: `server/src/history.rs` (NEW)**
```rust
// Schema
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    ended_at TEXT,
    exit_code INTEGER,
    cwd TEXT,
    command TEXT,
    scrollback_path TEXT
);

CREATE TABLE session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL, -- 'output', 'input', 'resize', 'activity_change'
    data TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE VIRTUAL TABLE session_output_fts USING fts5(
    session_id,
    output_text,
    timestamp
);

CREATE INDEX idx_sessions_created ON sessions(created_at);
CREATE INDEX idx_events_session ON session_events(session_id, timestamp);
```

### Implementation

**File: `server/src/history.rs` (NEW)**
```rust
pub struct SessionHistory {
    db: SqlitePool,
    log_dir: PathBuf,
}

impl SessionHistory {
    pub async fn log_output(&self, session_id: Uuid, output: &[u8]) -> Result<()>;
    pub async fn log_input(&self, session_id: Uuid, input: &[u8]) -> Result<()>;
    pub async fn archive_session(&self, session: &Session) -> Result<()>;
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
    pub async fn list_sessions(&self, from: DateTime, to: DateTime) -> Result<Vec<SessionMetadata>>;
    pub async fn replay_session(&self, session_id: Uuid) -> Result<SessionReplay>;
}
```

**File: `server/src/pty.rs`**
- Hook into output broadcast to log to history
- On session exit, archive scrollback and metadata

**File: `server/src/api.rs`**
- Add `/api/history/sessions` - list archived sessions
- Add `/api/history/search?q=term` - full-text search
- Add `/api/history/{id}` - get session details
- Add `/api/history/{id}/replay` - stream scrollback replay
- Add `/api/history/{id}/ai-query` - AI-powered query over session

**File: `web/src/History.tsx` (NEW)**
```tsx
// New tab kind: "history"
export default function HistoryTab() {
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [searchQuery, setSearchQuery] = createSignal("");
  
  return (
    <div class="history-view">
      <div class="history-toolbar">
        <input 
          type="search" 
          placeholder="Search all sessions..."
          value={searchQuery()}
          onInput={(e) => setSearchQuery(e.target.value)}
        />
        <button onClick={() => aiQuery(searchQuery())}>
          Ask AI
        </button>
      </div>
      <div class="history-list">
        <For each={sessions()}>
          {(s) => (
            <SessionCard 
              session={s} 
              onReplay={() => replaySession(s.id)}
            />
          )}
        </For>
      </div>
    </div>
  );
}
```

**AI Integration**
**File: `server/src/ai_history.rs` (NEW)**
```rust
// Use Claude API to query session history
pub async fn query_history(
    db: &SessionHistory,
    query: &str,
    api_key: &str,
) -> Result<String> {
    // 1. Search relevant sessions
    let sessions = db.search(query, 10).await?;
    
    // 2. Build context from session outputs
    let context = sessions.iter()
        .map(|s| format!("Session: {}\nOutput:\n{}", s.name, s.output))
        .collect::<Vec<_>>()
        .join("\n\n");
    
    // 3. Query Claude
    let response = anthropic_client
        .messages()
        .create(MessagesRequest {
            model: "claude-3-5-sonnet-20241022",
            messages: vec![Message {
                role: "user",
                content: format!("Context:\n{}\n\nQuery: {}", context, query),
            }],
            ..Default::default()
        })
        .await?;
    
    Ok(response.content[0].text.clone())
}
```

**Configuration**
**File: `server/src/config.rs`**
- Add `history_enabled: bool` (default true)
- Add `history_db_path: PathBuf`
- Add `history_retention_days: u32` (default 90)
- Add `anthropic_api_key: Option<String>` for AI queries

### Storage Estimates
- Average session: 1-10 MB scrollback
- 100 sessions/month × 10 MB = 1 GB/month
- With 90-day retention: ~3 GB storage

## Task 5: Review and Port Features from open-walnut

### Key Features to Port

Based on the analysis of open-walnut, here are the most valuable features for MyDevEnv2:

#### 5.1 Session Diff View
**Value:** See what changed in each session at a glance
**Implementation:** 
- Capture workspace file diffs before/after session
- Git integration to show uncommitted changes
- Render in Monaco diff editor

**File: `server/src/diff.rs` (NEW)**
```rust
pub struct SessionDiff {
    session_id: Uuid,
    files_changed: Vec<FileDiff>,
    git_status: GitStatus,
}

pub fn capture_workspace_snapshot(workspace_root: &Path) -> Result<WorkspaceSnapshot>;
pub fn compute_diff(before: &WorkspaceSnapshot, after: &WorkspaceSnapshot) -> SessionDiff;
```

**File: `web/src/SessionDiff.tsx` (NEW)**
- Show file list with additions/deletions
- Click to expand inline diff
- "Accept changes" / "Revert" actions

#### 5.2 Memory/Notes System
**Value:** Persistent notes and documentation alongside code
**Implementation:**
- Add notes tab with markdown editor
- Store in `workspace_root/.mydevenv2/notes/`
- Support wiki-links `[[other-note]]`
- Full-text search across notes

**File: `server/src/notes.rs` (NEW)**
**File: `web/src/NotesTab.tsx` (NEW)**

#### 5.3 Task Integration
**Value:** Track TODOs and tasks without leaving the terminal
**Implementation:**
- Simple task list API
- Store in SQLite or JSON
- Quick add from anywhere (command palette or slash command)

**File: `web/src/Tasks.tsx` (NEW)**
- Kanban or list view
- Filter by status
- Link tasks to sessions

#### 5.4 Command Palette
**Value:** Fast keyboard-driven navigation
**Implementation:**
- Ctrl+K / Cmd+K to open
- Search sessions, files, commands, history
- Fuzzy matching

**File: `web/src/CommandPalette.tsx` (NEW)**
```tsx
const commands = [
  { id: 'new-session', label: 'New Terminal Session', icon: '🖥' },
  { id: 'new-file', label: 'New File', icon: '📄' },
  { id: 'search-history', label: 'Search History', icon: '🔍' },
  { id: 'open-file', label: 'Open File...', icon: '📁' },
  { id: 'git-status', label: 'Git Status', icon: '⎇' },
];
```

#### 5.5 Session Templates
**Value:** Quick session creation with presets
**Implementation:**
- Define templates: "Node dev", "Rust build", "Python env"
- Store in config
- One-click creation

**File: `server/src/config.rs`**
```rust
pub struct SessionTemplate {
    name: String,
    command: Vec<String>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
}
```

#### 5.6 NOT Porting (Out of Scope)
- **Remote SSH session management** - Already have Tailscale
- **MS To-Do integration** - Too specific
- **Dream agent / auto-distillation** - Over-engineered for our use case
- **Claude Code CLI orchestration** - We ARE the terminal environment

## Implementation Order

### Phase 1: Quick Wins (Week 1)
1. ✅ Fix website copy functionality
2. ✅ Add command palette (Ctrl+K)
3. ✅ Session templates

### Phase 2: Editor Uplift (Week 2-3)
4. ✅ IDE layout mode with persistent file tree
5. ✅ Breadcrumb navigation
6. ✅ Split editor support
7. ✅ Integrated terminal panel

### Phase 3: History System (Week 3-4)
8. ✅ SQLite schema and history storage
9. ✅ History tab UI
10. ✅ Full-text search
11. ✅ Session replay

### Phase 4: Windows GPU Fix (Week 4-5)
12. ✅ Frame pooling and throttling
13. ✅ GPU memory monitoring
14. ✅ AMD-specific testing
15. ✅ Fallback rendering mode

### Phase 5: Advanced Features (Week 5-6)
16. ✅ AI-powered history queries
17. ✅ Session diff view
18. ✅ Notes system
19. ✅ Task integration

## Testing Strategy

### Manual Testing
- Copy/paste on Chrome, Firefox, Safari (Mac/Windows)
- Copy/paste in PWA installed mode
- Windows native client on AMD GPU (RX 6800, RX 7900)
- IDE mode with large projects (>1000 files)
- History search with >100 archived sessions
- Session replay with large scrollback (>100 MB)

### Automated Testing
- Unit tests for history database operations
- Integration tests for AI query API
- E2E tests for command palette
- Performance tests for editor with large files

### Rollout
1. Deploy history system in opt-in mode first
2. Monitor SQLite database size growth
3. Gather feedback on IDE layout
4. Release Windows GPU fix as beta
5. Full production rollout after 2 weeks of beta

## Risk Mitigation

### Risk: SQLite database growth
**Mitigation:** 
- Auto-cleanup after retention period
- Add size limits per session
- Compression for archived scrollback

### Risk: Performance regression with history logging
**Mitigation:**
- Async logging with bounded channel
- Batch writes to SQLite
- Make history opt-out if needed

### Risk: AMD GPU crashes persist
**Mitigation:**
- Ship software rendering fallback
- Add crash recovery and automatic mode switch
- Collect telemetry to identify root cause

### Risk: IDE mode breaks mobile experience
**Mitigation:**
- Keep tabbed mode as default on mobile
- Responsive breakpoints for layout
- Touch-optimized file tree

## Success Metrics

1. **Copy functionality:** 0 copy-related error reports after 1 week
2. **GPU crashes:** <1% crash rate on Windows AMD GPUs
3. **Editor adoption:** >30% of users switch to IDE mode
4. **History usage:** >50% of users perform at least one history search/week
5. **Performance:** No regression in terminal latency (<50ms p99)

## Dependencies

### New NPM packages
- None (use existing Solid, Monaco, xterm)

### New Rust crates
```toml
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio"] }
tantivy = "0.22" # For better full-text search (alternative to FTS5)
```

### External services
- Optional: Anthropic API key for AI history queries

## Documentation Updates

1. Update `README.md` with new features
2. Add `HISTORY.md` documenting the history API
3. Add `EDITOR.md` explaining IDE mode
4. Update `client/README.md` with GPU troubleshooting
5. Add screenshots to docs for new UI features

## Rollback Plan

All features are additive and opt-in where possible:
- History: disable via config flag
- IDE mode: toggle in settings
- Command palette: optional overlay
- GPU fixes: revert to original rendering code

No breaking changes to existing APIs or data formats.
