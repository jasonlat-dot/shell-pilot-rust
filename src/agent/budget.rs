//! Conservative request planning, independent of a model provider's tokenizer.
//! Provider-reported usage should calibrate these estimates once transports are wired.
use super::{Error, Event, Payload, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelBudget {
    pub context_window: u64,
    pub output_reserve: u64,
    pub safety_reserve: u64,
    pub compact_percent: u8,
}
#[derive(Clone, Debug, Default)]
pub struct InputCosts {
    /// Serialized system/developer instructions and tool definitions, including framing.
    pub fixed_tokens: u64,
    /// Latest user request, task/checkpoint and other mandatory input.
    pub pinned_tokens: u64,
    /// Model-specific images/audio/PDF input estimate supplied by the adapter.
    pub media_tokens: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryGroup {
    pub event_ids: Vec<u64>,
    pub tokens: u64,
    pub complete: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub selected_event_ids: Vec<u64>,
    pub available_input_tokens: u64,
    pub estimated_input_tokens: u64,
    pub estimated_full_input_tokens: u64,
    pub dropped_groups: usize,
    pub should_compact: bool,
}
impl ModelBudget {
    pub fn available_input(&self) -> Result<u64> {
        if !(1..=99).contains(&self.compact_percent) || self.output_reserve == 0 {
            return Err(Error::Invalid(
                "model budget has no output reserve or invalid threshold".into(),
            ));
        }
        self.context_window
            .checked_sub(self.output_reserve)
            .and_then(|value| value.checked_sub(self.safety_reserve))
            .filter(|value| *value > 0)
            .ok_or(Error::BudgetExceeded)
    }
    pub fn plan(&self, costs: &InputCosts, groups: &[HistoryGroup]) -> Result<Plan> {
        let available = self.available_input()?;
        let pinned = add(
            add(costs.fixed_tokens, costs.pinned_tokens)?,
            costs.media_tokens,
        )?;
        if pinned > available {
            return Err(Error::BudgetExceeded);
        }
        // Never send an unresolved call group; recovery must first record a real
        // outcome or an explicit unknown/cancelled result, not fabricate success.
        if groups.iter().any(|group| !group.complete) {
            return Err(Error::ToolProtocol);
        }
        let mut unique = HashSet::new();
        for group in groups {
            if group.event_ids.is_empty()
                || group.tokens == 0
                || group
                    .event_ids
                    .iter()
                    .any(|id| *id == 0 || !unique.insert(*id))
            {
                return Err(Error::Invalid("invalid history group".into()));
            }
        }
        let full = groups
            .iter()
            .try_fold(pinned, |tokens, group| add(tokens, group.tokens))?;
        let mut selected = Vec::new();
        let mut used = pinned;
        for group in groups.iter().rev() {
            let next = add(used, group.tokens)?;
            if next > available {
                break;
            }
            used = next;
            selected.push(group);
        }
        selected.reverse();
        let dropped_groups = groups.len() - selected.len();
        Ok(Plan {
            selected_event_ids: selected
                .into_iter()
                .flat_map(|group| group.event_ids.iter().copied())
                .collect(),
            available_input_tokens: available,
            estimated_input_tokens: used,
            estimated_full_input_tokens: full,
            dropped_groups,
            should_compact: dropped_groups > 0
                || u128::from(full) * 100
                    >= u128::from(available) * u128::from(self.compact_percent),
        })
    }
}
/// UTF-8 byte heuristic, intentionally not advertised as exact tokenization.
pub fn estimate_text(text: &str) -> u64 {
    (text.len() as u64).div_ceil(3)
}
pub fn estimate_event(event: &Event) -> Result<u64> {
    // Include call arguments and roles/IDs, not just content strings.
    add(estimate_text(&serde_json::to_string(&event.payload)?), 12)
}
/// Group contiguous parallel tool calls and all matching results atomically.
/// Intermediate messages remain in that group to preserve chronology.
pub fn group_history(events: &[Event]) -> Result<Vec<HistoryGroup>> {
    let mut groups = Vec::new();
    let mut pending = HashSet::new();
    let mut seen_calls = HashSet::new();
    let mut last_id = 0;
    let mut open = HistoryGroup {
        event_ids: Vec::new(),
        tokens: 0,
        complete: false,
    };
    let session = events.first().map(|event| &event.session_id);
    for event in events {
        if event.id <= last_id || Some(&event.session_id) != session {
            return Err(Error::Invalid(
                "history is unordered or crosses sessions".into(),
            ));
        }
        last_id = event.id;
        match &event.payload {
            Payload::ToolCall { call_id, .. } => {
                if !seen_calls.insert(call_id.clone()) || !pending.insert(call_id.clone()) {
                    return Err(Error::ToolProtocol);
                }
            }
            Payload::ToolResult { call_id, .. } if !pending.remove(call_id) => {
                return Err(Error::ToolProtocol);
            }
            _ => {}
        }
        open.tokens = add(open.tokens, estimate_event(event)?)?;
        open.event_ids.push(event.id);
        if pending.is_empty() {
            open.complete = true;
            groups.push(open);
            open = HistoryGroup {
                event_ids: Vec::new(),
                tokens: 0,
                complete: false,
            };
        }
    }
    if !open.event_ids.is_empty() {
        groups.push(open);
    }
    Ok(groups)
}
fn add(left: u64, right: u64) -> Result<u64> {
    left.checked_add(right).ok_or(Error::BudgetExceeded)
}
