import copy
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    'e5_report', Path(__file__).resolve().parents[1] / 'evals/e5/report.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def fixture():
    runs = []
    for arm in module.ARMS:
        for rep, latency in [(1, 100), (2, 300)]:
            runs.append({'caseId': 'sample', 'arm': arm, 'repetition': rep,
                         'parameterFingerprint': 'same', 'parameters': {'model': 'same'},
                         'model': 'same', 'compactPercent': 99 if arm == module.ARMS[0] else 60,
                         'score': {'completed': True, 'targetCorrect': None,
                                   'factsRetained': 1, 'factsExpected': 1,
                                   'constraintsPreserved': True, 'extraExecutions': 0,
                                   'duplicateExecutions': 0},
                         'summaryRequests': 0, 'checkpoints': 0, 'replayClean': True,
                         'requests': [{'usage': {'prompt_tokens': 10, 'completion_tokens': 2,
                                                 'total_tokens': 12}}],
                         'elapsedMs': latency, 'toolCalls': 0, 'extraToolCalls': 0})
    return {'mode': 'real_model', 'matrixComplete': True, 'retryOnly': False,
            'previousPassesSkipped': 0, 'abortedReason': None, 'repetitions': 2,
            'runs': runs}, [{'id': 'sample', 'longHistory': False}]


class ReportTests(unittest.TestCase):
    def test_even_median_and_provider_totals(self):
        report, cases = fixture()
        result = module.analyze(report, cases)
        self.assertEqual(result['aggregate'][0]['latencyMedianMs'], 200)
        self.assertEqual(result['aggregate'][0]['providerTokens']['total_tokens'], 24)
        self.assertTrue(result['allTaskAndCompactionGatesPassed'])

    def test_mixed_models_and_duplicate_trials_are_rejected(self):
        report, cases = fixture()
        report['runs'][0]['parameterFingerprint'] = 'other-model'
        with self.assertRaises(ValueError):
            module.analyze(report, cases)
        report, cases = fixture()
        report['runs'][0] = copy.deepcopy(report['runs'][1])
        with self.assertRaises(ValueError):
            module.analyze(report, cases)

    def test_selective_retries_cannot_be_relabelled_as_full_comparison(self):
        report, cases = fixture()
        report['retryOnly'] = True
        with self.assertRaises(ValueError):
            module.analyze(report, cases)

    def test_missing_usage_is_not_zero_and_missing_checkpoint_is_not_a_pass(self):
        report, cases = fixture()
        report['runs'][0]['requests'][0]['usage'] = None
        report['runs'][0]['summaryRequests'] = 1
        result = module.analyze(report, cases)
        self.assertIsNone(result['aggregate'][0]['providerTokens']['total_tokens'])
        self.assertFalse(result['allProviderUsagePresent'])
        self.assertFalse(result['allTaskAndCompactionGatesPassed'])


if __name__ == '__main__':
    unittest.main()
