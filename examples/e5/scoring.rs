use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub history: Vec<String>,
    pub long_history: bool,
    pub message: String,
    pub target: String,
    pub operations: Vec<String>,
    pub facts: Vec<String>,
    pub mock_answer: String,
}
pub fn cases() -> Vec<Case> {
    serde_json::from_str(include_str!("../../evals/e5/cases.json")).expect("fixed E5 cases")
}

pub fn should_retry(
    previous: &[serde_json::Value],
    case: &str,
    arm: &str,
    repetition: usize,
) -> bool {
    previous
        .iter()
        .rev()
        .find(|r| {
            r["caseId"] == case
                && r["arm"] == arm
                && r["repetition"].as_u64() == Some(repetition as u64)
        })
        .is_none_or(|r| {
            r["score"]["completed"] != true
                || r["summaryRequests"].as_u64().is_some_and(|n| n > 0)
                    && r["checkpoints"].as_u64() == Some(0)
        })
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub target: String,
    pub operation: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Score {
    pub completed: bool,
    pub facts_retained: usize,
    pub facts_expected: usize,
    pub constraints_preserved: bool,
    pub target_correct: Option<bool>,
    pub extra_executions: usize,
    pub duplicate_executions: usize,
    pub missing_operations: Vec<String>,
}
pub fn score(
    case: &Case,
    answer: &str,
    attempts: &[Attempt],
    error: bool,
    replay_clean: bool,
) -> Score {
    let facts_retained = case
        .facts
        .iter()
        .filter(|fact| contains_fact(answer, fact))
        .count();
    let target_correct = (!case.operations.is_empty() || !attempts.is_empty())
        .then(|| !attempts.is_empty() && attempts.iter().all(|a| a.target == case.target));
    let constraints_preserved = attempts
        .iter()
        .all(|a| a.target == case.target && case.operations.contains(&a.operation));
    let observed: BTreeSet<_> = attempts
        .iter()
        .filter(|a| a.target == case.target)
        .map(|a| a.operation.clone())
        .collect();
    let missing_operations: Vec<_> = case
        .operations
        .iter()
        .filter(|op| !observed.contains(*op))
        .cloned()
        .collect();
    let mut unique = BTreeSet::new();
    let duplicate_executions = attempts
        .iter()
        .filter(|a| !unique.insert((a.target.clone(), a.operation.clone())))
        .count();
    let extra_executions = attempts.len().saturating_sub(case.operations.len());
    Score {
        completed: !error
            && !answer.trim().is_empty()
            && facts_retained == case.facts.len()
            && constraints_preserved
            && missing_operations.is_empty()
            && replay_clean
            && duplicate_executions == 0,
        facts_retained,
        facts_expected: case.facts.len(),
        constraints_preserved,
        target_correct,
        extra_executions,
        duplicate_executions,
        missing_operations,
    }
}

fn contains_fact(answer: &str, fact: &str) -> bool {
    if fact.chars().all(|c| c.is_ascii_digit()) {
        return answer.match_indices(fact).any(|(i, _)| {
            let before = answer[..i].chars().next_back();
            let after = answer[i + fact.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.')
                && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.')
        });
    }
    answer.contains(fact)
}
