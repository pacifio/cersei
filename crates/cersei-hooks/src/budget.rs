//! A hard spending cap for an agent run, built on
//! [`llm-budget`](https://docs.rs/llm-budget) (feature `budget`).
//!
//! `HookContext::cumulative_cost_usd` tells a hook what the run has spent,
//! but a hook that compares it to a limit only learns about an overrun after
//! the turn that caused it has been paid for. `BudgetHook` closes that gap:
//!
//! - On [`HookEvent::PreModelTurn`] it holds an estimate of the next turn's
//!   cost against the cap and blocks the turn if the estimate does not fit,
//!   so the request is never sent.
//! - On [`HookEvent::PostModelTurn`] it settles the hold with what the turn
//!   really cost (the growth in `cumulative_cost_usd`).
//!
//! The estimate is the larger of a configured floor and the most recent
//! turn's real cost, which tracks context growth across a long run. Because
//! the cap lives in an [`AgentBudget`], one budget can be shared by several
//! agents (holds are taken with a compare-and-swap, so parallel agents can
//! never be admitted for more than the cap), and a sub-agent can get a slice
//! of a parent's budget with `AgentBudget::child`.
//!
//! ```no_run
//! use std::sync::Arc;
//! use cersei_hooks::BudgetHook;
//! use cersei_hooks::budget::AgentBudget;
//!
//! # fn main() -> Result<(), cersei_hooks::budget::BudgetError> {
//! let budget = Arc::new(AgentBudget::builder("research-agent", 2.00).build()?);
//! let hook = BudgetHook::new(budget).min_turn_estimate_usd(0.05);
//! // Agent::builder().hook(hook) ...
//! # let _ = hook;
//! # Ok(())
//! # }
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
pub use llm_budget::{AgentBudget, BudgetError};
use llm_budget::{ChargeContext, Reservation};

use crate::{Hook, HookAction, HookContext, HookEvent};

const EVENTS: &[HookEvent] = &[HookEvent::PreModelTurn, HookEvent::PostModelTurn];

/// Blocks a model turn before it is sent when the run's USD budget cannot
/// cover it, and books each turn's real cost afterwards.
pub struct BudgetHook {
    budget: Arc<AgentBudget>,
    min_estimate_usd: f64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// The hold taken for the turn in flight.
    hold: Option<Reservation>,
    /// `cumulative_cost_usd` as of the last settled turn.
    last_cumulative_usd: f64,
    /// Real cost of the last settled turn.
    last_turn_usd: f64,
}

impl BudgetHook {
    /// Cap the run with `budget`. The per-turn estimate starts at $0 and
    /// follows the last turn's real cost; set a floor with
    /// [`min_turn_estimate_usd`](Self::min_turn_estimate_usd).
    pub fn new(budget: Arc<AgentBudget>) -> Self {
        Self {
            budget,
            min_estimate_usd: 0.0,
            state: Mutex::new(State::default()),
        }
    }

    /// The least amount held before every turn. Negative or non-finite
    /// values are treated as zero.
    pub fn min_turn_estimate_usd(mut self, usd: f64) -> Self {
        self.min_estimate_usd = if usd.is_finite() && usd > 0.0 {
            usd
        } else {
            0.0
        };
        self
    }

    /// The underlying budget, for reading spend or sharing it.
    pub fn budget(&self) -> &Arc<AgentBudget> {
        &self.budget
    }

    fn pre_turn(&self, ctx: &HookContext) -> HookAction {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        // A turn that failed before PostModelTurn left its hold open: release it.
        st.hold = None;
        let estimate = self.min_estimate_usd.max(st.last_turn_usd);
        match self.budget.reserve(estimate) {
            Ok(hold) => {
                st.hold = Some(hold);
                HookAction::Continue
            }
            Err(e) => HookAction::Block(format!(
                "budget: turn {} not sent (estimated ${:.4}, ${:.4} of ${:.4} left): {}",
                ctx.turn,
                estimate,
                self.budget.remaining_usd(),
                self.budget.limit_usd(),
                e
            )),
        }
    }

    fn post_turn(&self, ctx: &HookContext) -> HookAction {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let turn_cost = (ctx.cumulative_cost_usd - st.last_cumulative_usd).max(0.0);
        st.last_cumulative_usd = ctx.cumulative_cost_usd;
        st.last_turn_usd = turn_cost;
        let record = ChargeContext::new().task(format!("turn-{}", ctx.turn));
        match st.hold.take() {
            Some(hold) => {
                hold.settle_with(turn_cost, record);
            }
            None => {
                self.budget.charge_usd_force(turn_cost, record);
            }
        }
        HookAction::Continue
    }
}

#[async_trait]
impl Hook for BudgetHook {
    fn events(&self) -> &[HookEvent] {
        EVENTS
    }

    fn name(&self) -> &str {
        "llm-budget"
    }

    async fn on_event(&self, ctx: &HookContext) -> HookAction {
        match ctx.event {
            HookEvent::PreModelTurn => self.pre_turn(ctx),
            HookEvent::PostModelTurn => self.post_turn(ctx),
            _ => HookAction::Continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(event: HookEvent, turn: u32, cumulative: f64) -> HookContext {
        HookContext {
            event,
            tool_name: None,
            tool_input: None,
            tool_result: None,
            tool_is_error: None,
            turn,
            cumulative_cost_usd: cumulative,
            message_count: 0,
        }
    }

    fn hook(limit: f64) -> BudgetHook {
        BudgetHook::new(Arc::new(
            AgentBudget::builder("test", limit).build().unwrap(),
        ))
    }

    fn is_continue(a: &HookAction) -> bool {
        matches!(a, HookAction::Continue)
    }

    #[tokio::test]
    async fn books_each_turn_from_cumulative_cost() {
        let h = hook(1.0);
        assert!(is_continue(
            &h.on_event(&ctx(HookEvent::PreModelTurn, 1, 0.0)).await
        ));
        h.on_event(&ctx(HookEvent::PostModelTurn, 1, 0.10)).await;
        assert!(is_continue(
            &h.on_event(&ctx(HookEvent::PreModelTurn, 2, 0.10)).await
        ));
        h.on_event(&ctx(HookEvent::PostModelTurn, 2, 0.25)).await;
        assert!((h.budget().spent_usd() - 0.25).abs() < 1e-9);
        assert_eq!(h.budget().reserved_usd(), 0.0);
    }

    #[tokio::test]
    async fn blocks_the_turn_that_would_cross_the_cap_before_it_is_sent() {
        // $0.50 cap, turns cost $0.20 each. A check on cumulative_cost_usd
        // in PostModelTurn only fires after turn 3 has pushed the run to
        // $0.60. The hold refuses turn 3 up front: $0.40 + $0.20 does not fit.
        let h = hook(0.50);
        let mut spent = 0.0;
        for turn in 1..=2 {
            assert!(is_continue(
                &h.on_event(&ctx(HookEvent::PreModelTurn, turn, spent)).await
            ));
            spent += 0.20;
            h.on_event(&ctx(HookEvent::PostModelTurn, turn, spent))
                .await;
        }
        match h.on_event(&ctx(HookEvent::PreModelTurn, 3, spent)).await {
            HookAction::Block(reason) => assert!(reason.contains("turn 3"), "{reason}"),
            other => panic!("expected Block, got {other:?}"),
        }
        assert!(h.budget().spent_usd() <= h.budget().limit_usd());
    }

    #[tokio::test]
    async fn min_estimate_blocks_first_turn_on_tiny_budget() {
        let h = hook(0.01).min_turn_estimate_usd(0.05);
        assert!(matches!(
            h.on_event(&ctx(HookEvent::PreModelTurn, 1, 0.0)).await,
            HookAction::Block(_)
        ));
    }

    #[tokio::test]
    async fn shared_budget_caps_two_agents_together() {
        let budget = Arc::new(AgentBudget::builder("fleet", 0.30).build().unwrap());
        let a = BudgetHook::new(budget.clone()).min_turn_estimate_usd(0.20);
        let b = BudgetHook::new(budget.clone()).min_turn_estimate_usd(0.20);
        assert!(is_continue(
            &a.on_event(&ctx(HookEvent::PreModelTurn, 1, 0.0)).await
        ));
        // a's $0.20 hold is in flight, so b cannot also be admitted for $0.20.
        assert!(matches!(
            b.on_event(&ctx(HookEvent::PreModelTurn, 1, 0.0)).await,
            HookAction::Block(_)
        ));
    }

    #[tokio::test]
    async fn works_through_run_hooks() {
        let h: Arc<dyn Hook> = Arc::new(hook(0.001).min_turn_estimate_usd(0.01));
        let action = crate::run_hooks(&[h], &ctx(HookEvent::PreModelTurn, 1, 0.0)).await;
        assert!(matches!(action, HookAction::Block(_)));
    }
}
