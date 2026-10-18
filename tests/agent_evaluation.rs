#[path = "../examples/e5/scoring.rs"]
mod scoring;
use scoring::*;

#[test]
fn suite_has_unique_cases_and_valid_objectives() {
    let suite = cases();
    assert_eq!(suite.len(), 6);
    let unique: std::collections::HashSet<_> = suite.iter().map(|c| &c.id).collect();
    assert_eq!(unique.len(), suite.len());
    assert_eq!(suite.iter().filter(|c| c.long_history).count(), 4);
    for c in suite {
        assert!(!c.message.is_empty());
        assert!(!c.facts.is_empty());
        let attempts: Vec<_> = c
            .operations
            .iter()
            .map(|op| Attempt {
                target: c.target.clone(),
                operation: op.clone(),
            })
            .collect();
        assert!(
            score(&c, &c.mock_answer, &attempts, false, true).completed,
            "{}",
            c.id
        );
    }
}
#[test]
fn right_words_without_required_tool_evidence_do_not_pass() {
    let c = &cases()[0];
    let s = score(c, &c.mock_answer, &[], false, true);
    assert!(!s.completed);
    assert_eq!(s.target_correct, Some(false));
    assert_eq!(s.missing_operations, vec!["disk"]);
}
#[test]
fn wrong_target_and_forbidden_operations_fail_even_with_correct_answer() {
    let c = &cases()[0];
    for a in [
        Attempt {
            target: "beta".into(),
            operation: "disk".into(),
        },
        Attempt {
            target: "alpha".into(),
            operation: "restart".into(),
        },
    ] {
        let s = score(c, &c.mock_answer, &[a], false, true);
        assert!(!s.completed);
        assert!(!s.constraints_preserved);
    }
}
#[test]
fn duplicate_reads_replays_errors_and_missing_facts_cannot_pass() {
    let c = &cases()[0];
    let a = Attempt {
        target: "alpha".into(),
        operation: "disk".into(),
    };
    let duplicate = score(c, &c.mock_answer, &[a.clone(), a.clone()], false, true);
    assert!(!duplicate.completed);
    assert_eq!(duplicate.duplicate_executions, 1);
    assert_eq!(duplicate.extra_executions, 1);
    assert!(!score(c, &c.mock_answer, std::slice::from_ref(&a), false, false).completed);
    assert!(!score(c, &c.mock_answer, std::slice::from_ref(&a), true, true).completed);
    assert!(!score(c, "alpha", &[a], false, true).completed);
}
#[test]
fn no_tool_case_does_not_gain_target_credit_or_allow_unneeded_queries() {
    let c = &cases()[2];
    assert!(c.operations.is_empty());
    assert!(!c.history.is_empty());
    assert_eq!(
        score(c, &c.mock_answer, &[], false, true).target_correct,
        None
    );
    assert!(
        !score(
            c,
            &c.mock_answer,
            &[Attempt {
                target: "alpha".into(),
                operation: "disk".into()
            }],
            false,
            true
        )
        .completed
    );
}

#[test]
fn numeric_fact_is_not_a_substring_of_a_different_count() {
    let c = &cases()[1];
    let attempts: Vec<_> = c
        .operations
        .iter()
        .map(|op| Attempt {
            target: c.target.clone(),
            operation: op.clone(),
        })
        .collect();
    for answer in [
        "alpha 41%，容器18个",
        "alpha 41%，容器8.5个",
        "alpha 41%，容器0.8个",
    ] {
        assert!(!score(c, answer, &attempts, false, true).completed);
    }
}

#[test]
fn retry_selection_skips_success_but_keeps_failed_missing_and_uncommitted_summary_trials() {
    use serde_json::json;
    let records = vec![
        json!({"caseId":"disk","arm":"A_late_99","repetition":1,"score":{"completed":true},"summaryRequests":0,"checkpoints":0}),
        json!({"caseId":"disk","arm":"B_proactive_60","repetition":1,"score":{"completed":false},"summaryRequests":0,"checkpoints":0}),
        json!({"caseId":"early-facts","arm":"B_proactive_60","repetition":1,"score":{"completed":true},"summaryRequests":2,"checkpoints":0}),
    ];
    assert!(!should_retry(&records, "disk", "A_late_99", 1));
    assert!(should_retry(&records, "disk", "B_proactive_60", 1));
    assert!(should_retry(&records, "early-facts", "B_proactive_60", 1));
    assert!(should_retry(&records, "disk", "A_late_99", 2));
}
