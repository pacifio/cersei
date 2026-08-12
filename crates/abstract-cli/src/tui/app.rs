//! TUI application state.

use crate::permissions::SharedPermissionMode;
use crate::tui::scroll::ScrollState;
use cersei_tools::permissions::PermissionDecision;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tokio::sync::oneshot;

/// A single message turn in the conversation.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub content: String,
    pub tools: Vec<ToolCall>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

/// A tool invocation with its status.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub input_summary: String,
    pub status: ToolStatus,
    pub output_preview: Option<String>,
    pub started_at: Instant,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

/// Overlay currently displayed on top of the main content.
#[derive(Debug, Clone)]
pub enum Overlay {
    None,
    Help,
    Permission(PermissionOverlay),
    Recovery(RecoveryOverlay),
    Graph(crate::tui::widgets::graph::GraphOverlayState),
}

impl PartialEq for Overlay {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::None, Self::None)
                | (Self::Help, Self::Help)
                | (Self::Permission(_), Self::Permission(_))
                | (Self::Recovery(_), Self::Recovery(_))
                | (Self::Graph(_), Self::Graph(_))
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionOverlay {
    pub tool_name: String,
    pub description: String,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryOverlay {
    pub error_msg: String,
    pub options: Vec<String>,
    pub selected: usize,
}

/// Permission mode (Shift+Tab to cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Auto,
    Plan,
    Editor,
    Bypass,
    BypassAlert,
}

impl PermissionMode {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Plan,
            Self::Plan => Self::Editor,
            Self::Editor => Self::Bypass,
            Self::Bypass => Self::BypassAlert,
            Self::BypassAlert => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Plan => "Plan",
            Self::Editor => "Editor",
            Self::Bypass => "Bypass",
            Self::BypassAlert => "Bypass+Alert",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => "Ask for permissions interactively",
            Self::Plan => "Read-only: plan without executing",
            Self::Editor => "All permissions except shell commands",
            Self::Bypass => "Bypass all permissions",
            Self::BypassAlert => "Bypass all, notify on shell commands",
        }
    }
}

/// Side panel tab selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanelTab {
    GitDiff,
    FileTree,
}

impl SidePanelTab {
    pub fn toggle(self) -> Self {
        match self {
            Self::GitDiff => Self::FileTree,
            Self::FileTree => Self::GitDiff,
        }
    }
}

/// Full application state for the TUI.
pub struct AppState {
    // ── Conversation ──
    pub turns: Vec<Turn>,
    pub streaming_text: String,
    pub streaming_thinking: String,
    pub is_streaming: bool,
    pub active_tools: Vec<ToolCall>,

    // ── Input ──
    pub input: String,
    pub cursor_pos: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // ── Scroll + Virtual List ──
    pub scroll: ScrollState,
    pub virtual_list: crate::tui::virtual_list::VirtualList,
    pub messages_dirty: bool,

    // ── Status ──
    pub model: String,
    pub session_id: String,
    pub effort: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Session-cumulative prompt tokens served from the provider's prompt
    /// cache (Anthropic cache reads, OpenAI/Gemini cached-token counts).
    pub cache_read_tokens: u64,
    /// Session-cumulative prompt tokens written to the provider's prompt
    /// cache, when the provider reports that counter.
    pub cache_creation_tokens: u64,
    pub cost_usd: f64,
    pub cost_available: bool,
    pub context_pct: f64,
    pub turn_count: u32,
    pub tool_count: u32,
    pub stream_start: Option<Instant>,
    /// Last terminal mouse position, used for compact hover help.
    pub mouse_position: Option<(u16, u16)>,

    // ── Permission mode ──
    pub permission_mode: PermissionMode,
    pub shared_permission_mode: Option<SharedPermissionMode>,

    // ── Side panel ──
    pub side_panel_open: bool,
    pub side_panel_focused: bool,
    pub side_panel_tab: SidePanelTab,
    pub side_panel_scroll: ScrollState,
    pub side_panel_diff: String,
    pub side_panel_tree: String,

    // ── Overlay ──
    pub overlay: Overlay,
    /// Pending permission response sender (for TUI-based permission flow).
    pub pending_permission_tx: Option<oneshot::Sender<PermissionDecision>>,

    // ── Animation ──
    pub frame_count: u64,

    // ── Flags ──
    pub should_quit: bool,
    pub dirty: bool,
}

impl AppState {
    pub fn new(model: &str, session_id: &str, effort: &str) -> Self {
        Self {
            turns: Vec::new(),
            streaming_text: String::new(),
            streaming_thinking: String::new(),
            is_streaming: false,
            active_tools: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            scroll: ScrollState::new(),
            virtual_list: crate::tui::virtual_list::VirtualList::new(),
            messages_dirty: true,
            model: model.to_string(),
            session_id: session_id[..session_id.floor_char_boundary(8)].to_string(),
            effort: effort.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
            cost_available: true,
            context_pct: 0.0,
            turn_count: 0,
            tool_count: 0,
            stream_start: None,
            mouse_position: None,
            permission_mode: PermissionMode::Auto,
            shared_permission_mode: None,
            side_panel_open: false,
            side_panel_focused: false,
            side_panel_tab: SidePanelTab::GitDiff,
            side_panel_scroll: ScrollState::new(),
            side_panel_diff: String::new(),
            side_panel_tree: String::new(),
            overlay: Overlay::None,
            pending_permission_tx: None,
            frame_count: 0,
            should_quit: false,
            dirty: true,
        }
    }

    /// Commit the current streaming text into a completed turn.
    pub fn commit_turn(&mut self) {
        if !self.streaming_text.is_empty() || !self.active_tools.is_empty() {
            self.turns.push(Turn {
                role: TurnRole::Assistant,
                content: std::mem::take(&mut self.streaming_text),
                tools: std::mem::take(&mut self.active_tools),
                thinking: if self.streaming_thinking.is_empty() {
                    None
                } else {
                    Some(std::mem::take(&mut self.streaming_thinking))
                },
            });
            self.turn_count += 1;
        }
        self.is_streaming = false;
        self.stream_start = None;
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Add a user message turn.
    pub fn push_user(&mut self, text: &str) {
        self.turns.push(Turn {
            role: TurnRole::User,
            content: text.to_string(),
            tools: Vec::new(),
            thinking: None,
        });
        self.messages_dirty = true;
        self.dirty = true;
    }

    /// Elapsed time since streaming started.
    pub fn elapsed_ms(&self) -> u64 {
        self.stream_start
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    /// Cycle permission mode and update the shared atomic.
    pub fn cycle_permission_mode(&mut self) {
        self.permission_mode = self.permission_mode.next();
        if let Some(ref shared) = self.shared_permission_mode {
            let val = match self.permission_mode {
                PermissionMode::Auto => 0,
                PermissionMode::Plan => 1,
                PermissionMode::Editor => 2,
                PermissionMode::Bypass => 3,
                PermissionMode::BypassAlert => 4,
            };
            shared.store(val, Ordering::Relaxed);
        }
    }

    /// Set the shared permission mode reference.
    pub fn set_shared_mode(&mut self, mode: SharedPermissionMode) {
        self.shared_permission_mode = Some(mode);
    }

    /// Apply a `CostUpdate` event. The event carries the session-cumulative
    /// totals (the runner accumulates every turn before emitting it), so the
    /// state is *set*, never added to.
    pub fn apply_cost_update(
        &mut self,
        cumulative_cost: f64,
        cost_available: bool,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    ) {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
        self.cache_read_tokens = cache_read_tokens;
        self.cache_creation_tokens = cache_creation_tokens;
        self.cost_usd = cumulative_cost;
        self.cost_available = cost_available;
    }

    /// Track the model that actually served the current response. Kept
    /// separate from the configured model so a mid-session model switch is
    /// priced and displayed with the right identity.
    pub fn apply_model_response_start(&mut self, model: &str) {
        if !model.is_empty() {
            self.model = model.to_string();
        }
    }

    /// Current usage shaped for the local cost estimators. The cumulative
    /// tokens mirror the session totals in `CostUpdate`; `cost_usd` is left
    /// unset so the estimator never double-counts an already-reported cost.
    pub fn usage_snapshot(&self) -> cersei_types::Usage {
        cersei_types::Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_read_input_tokens: self.cache_read_tokens,
            cache_creation_input_tokens: self.cache_creation_tokens,
            ..Default::default()
        }
    }

    /// Total prompt tokens of the session, cache-aware. Matches the
    /// `Usage` doc: `input + cache_creation + cache_read`.
    pub fn total_prompt_tokens(&self) -> u64 {
        self.usage_snapshot().prompt_tokens()
    }

    /// Session KV-cache hit ratio: cached-read tokens over total prompt
    /// tokens. Cache *writes* are excluded — creating a cache entry is not a
    /// hit. Returns `None` when there are no prompt tokens (division guard).
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let total = self.total_prompt_tokens();
        if total == 0 {
            None
        } else {
            Some(self.cache_read_tokens as f64 / total as f64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_update_sets_cumulative_totals() {
        let mut state = AppState::new("deepseek/deepseek-chat", "session-12345678", "auto");
        state.apply_cost_update(0.4281, true, 100_000, 20_000, 75_000, 0);
        assert_eq!(state.input_tokens, 100_000);
        assert_eq!(state.output_tokens, 20_000);
        assert_eq!(state.cache_read_tokens, 75_000);
        assert!((state.cost_usd - 0.4281).abs() < 1e-9);
    }

    #[test]
    fn unavailable_price_is_explicit_in_state() {
        let mut state = AppState::new("unknown/model", "session-12345678", "auto");
        state.cost_available = false;
        state.apply_cost_update(0.0, false, 25_000, 0, 75_000, 0);
        assert!(!state.cost_available);
        assert_eq!(state.cache_hit_ratio(), Some(0.75));
    }

    /// Test 1 + Test 6: cumulative updates accumulate across turns; the state
    /// is driven by the runner's session-cumulative `CostUpdate` events, and
    /// per-turn snapshots must never be able to shrink it.
    #[test]
    fn cumulative_totals_across_turns() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        // Turn 1: 100k prompt (25k uncached + 75k cached reads), 20k out.
        state.apply_cost_update(0.40, true, 25_000, 20_000, 75_000, 0);
        assert_eq!(state.input_tokens, 25_000);
        // Turn 2 (cumulative): 200k prompt (50k uncached + 150k cached), 40k out.
        state.apply_cost_update(0.85, true, 50_000, 40_000, 150_000, 0);
        assert_eq!(state.input_tokens, 50_000);
        assert_eq!(state.cache_read_tokens, 150_000);
        assert_eq!(state.output_tokens, 40_000);
        assert!((state.cost_usd - 0.85).abs() < 1e-9);
    }

    #[test]
    fn model_response_start_tracks_real_model() {
        let mut state = AppState::new("openai/gpt-5-chat", "session-12345678", "auto");
        state.apply_model_response_start("deepseek/deepseek-chat");
        assert_eq!(state.model, "deepseek/deepseek-chat");
    }

    /// Test 5: cache ratio math — 75k reads over 100k prompt = 75%.
    #[test]
    fn cache_ratio_75_percent() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        state.apply_cost_update(1.0, true, 25_000, 0, 75_000, 0);
        let ratio = state.cache_hit_ratio().expect("ratio");
        assert!((ratio - 0.75).abs() < 1e-9);
    }

    #[test]
    fn cache_ratio_0_percent() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        state.apply_cost_update(1.0, true, 25_000, 0, 0, 0);
        assert_eq!(state.cache_hit_ratio(), Some(0.0));
    }

    #[test]
    fn cache_ratio_100_percent() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        state.apply_cost_update(1.0, true, 0, 0, 25_000, 0);
        assert_eq!(state.cache_hit_ratio(), Some(1.0));
    }

    #[test]
    fn cache_ratio_guard_when_no_prompt() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        assert_eq!(state.cache_hit_ratio(), None);
        // Cache writes are not hits and must not move the ratio.
        state.apply_cost_update(1.0, true, 0, 0, 0, 25_000);
        assert_eq!(state.cache_hit_ratio(), Some(0.0));
    }

    /// Test 5 (cumulative): ratio aggregates across turns.
    #[test]
    fn cache_ratio_cumulative_across_turns() {
        let mut state = AppState::new("claude-sonnet-4-6", "session-12345678", "auto");
        state.apply_cost_update(1.0, true, 25_000, 0, 75_000, 0); // turn 1: 75%
        state.apply_cost_update(1.0, true, 25_000, 0, 75_000, 0); // turn 2: 75%
        state.apply_cost_update(1.0, true, 50_000, 0, 150_000, 0); // 200k/200k reads
        let ratio = state.cache_hit_ratio().expect("ratio");
        assert!((ratio - 0.75).abs() < 1e-9);
    }
}
