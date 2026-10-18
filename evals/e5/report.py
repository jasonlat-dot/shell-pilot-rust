"""Validate a fresh paired E5 run and derive comparable metrics from raw trials."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import statistics

ARMS = ('A_late_99', 'B_proactive_60')


def analyze(report, cases):
    if report.get('mode') != 'real_model' or not report.get('matrixComplete'):
        raise ValueError('A complete real-model run is required')
    if report.get('retryOnly') or report.get('previousPassesSkipped', 0):
        raise ValueError('Selective retries cannot form a fresh paired comparison')
    if report.get('abortedReason'):
        raise ValueError('An aborted matrix cannot form a paired comparison')
    runs = report['runs']
    repetitions = report['repetitions']
    expected = {(c['id'], arm, rep) for c in cases for arm in ARMS
                for rep in range(1, repetitions + 1)}
    actual = [(r['caseId'], r['arm'], r['repetition']) for r in runs]
    if len(actual) != len(set(actual)) or set(actual) != expected:
        raise ValueError('Missing, duplicate or unexpected paired trials')
    if len({r['parameterFingerprint'] for r in runs}) != 1:
        raise ValueError('Model configuration differs between trials')
    if len({json.dumps(r['parameters'], sort_keys=True) for r in runs}) != 1:
        raise ValueError('Effective model parameters differ between trials')
    if len({r['model'] for r in runs}) != 1:
        raise ValueError('More than one model was used')
    for r in runs:
        if r['compactPercent'] != (99 if r['arm'] == ARMS[0] else 60):
            raise ValueError('Unexpected compaction strategy')
    by_id = {c['id']: c for c in cases}
    issues = []
    for r in runs:
        reasons = []
        if not r['score']['completed']:
            reasons.append('task_gate_failed')
        if not r['replayClean']:
            reasons.append('request_replay_failed')
        if r['summaryRequests'] and not r['checkpoints']:
            reasons.append('compaction_not_committed')
        if (r['arm'] == ARMS[1] and by_id[r['caseId']]['longHistory']
                and not r['checkpoints']):
            reasons.append('proactive_long_history_not_verified')
        if reasons:
            issues.append({'caseId': r['caseId'], 'arm': r['arm'],
                           'repetition': r['repetition'], 'reasons': reasons})
    aggregates = []
    for arm in ARMS:
        group = [r for r in runs if r['arm'] == arm]
        requests = [q for r in group for q in r['requests']]
        usages = [q['usage'] for q in requests]
        complete_usage = bool(usages) and all(u is not None for u in usages)
        targets = [r['score']['targetCorrect'] for r in group
                   if r['score']['targetCorrect'] is not None]
        aggregates.append({
            'arm': arm, 'trials': len(group),
            'taskPasses': sum(r['score']['completed'] for r in group),
            'factsRetained': sum(r['score']['factsRetained'] for r in group),
            'factsExpected': sum(r['score']['factsExpected'] for r in group),
            'constraintTracePasses': sum(r['score']['constraintsPreserved'] for r in group),
            'targetCorrect': sum(targets), 'targetScored': len(targets),
            'modelRequests': len(requests),
            'summaryRequests': sum(r['summaryRequests'] for r in group),
            'checkpoints': sum(r['checkpoints'] for r in group),
            'toolCalls': sum(r['toolCalls'] for r in group),
            'extraToolCalls': sum(r['extraToolCalls'] for r in group),
            'extraExecutions': sum(r['score']['extraExecutions'] for r in group),
            'duplicateExecutions': sum(r['score']['duplicateExecutions'] for r in group),
            'replayPasses': sum(r['replayClean'] for r in group),
            'latencyMedianMs': statistics.median(r['elapsedMs'] for r in group),
            'latencyMeanMs': statistics.mean(r['elapsedMs'] for r in group),
            'latencyTotalMs': sum(r['elapsedMs'] for r in group),
            'usageReportedRequests': sum(u is not None for u in usages),
            'usageComplete': complete_usage,
            'providerTokens': {k: sum(u[k] for u in usages) if complete_usage else None
                               for k in ('prompt_tokens', 'completion_tokens', 'total_tokens')},
        })
    return {'kind': 'fresh_same_model_paired_evaluation',
            'model': runs[0]['model'], 'parameters': runs[0]['parameters'],
            'parameterFingerprint': runs[0]['parameterFingerprint'],
            'trialCount': len(runs), 'repetitions': repetitions,
            'allTaskAndCompactionGatesPassed': not issues,
            'allProviderUsagePresent': all(a['usageComplete'] for a in aggregates),
            'issues': issues, 'aggregate': aggregates}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input', required=True, type=Path)
    parser.add_argument('--implementation', required=True, type=Path)
    parser.add_argument('--out-dir', required=True, type=Path)
    args = parser.parse_args()
    case_file = Path(__file__).with_name('cases.json')
    report = json.loads(args.input.read_text(encoding='utf-8'))
    if hashlib.sha256(case_file.read_bytes()).hexdigest() != report['suiteSha256']:
        raise ValueError('Case suite fingerprint differs from this report')
    analysis = analyze(report, json.loads(case_file.read_text(encoding='utf-8')))
    analysis['implementation'] = json.loads(args.implementation.read_text(encoding='utf-8'))
    analysis['rawReportSha256'] = hashlib.sha256(args.input.read_bytes()).hexdigest()
    analysis['analysisScriptSha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    analysis['medianDefinition'] = 'Arithmetic mean of two middle values for even samples'
    analysis['rawAggregateMedianDefinition'] = 'Upper middle value; original aggregate retained in raw.json'
    args.out_dir.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(args.input, args.out_dir / 'raw.json')
    shutil.copyfile(args.input.with_suffix('.jsonl'), args.out_dir / 'raw.jsonl')
    shutil.copyfile(args.implementation, args.out_dir / 'implementation.json')
    (args.out_dir / 'analysis.json').write_text(
        json.dumps(analysis, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(analysis, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
