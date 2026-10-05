"""Scoring and evidence-gate regression tests; never provider measurements."""
import copy
import json
import unittest

import run

CASES = json.loads((run.ROOT / 'corpus.json').read_text())['cases']
LABELS = json.loads((run.ROOT / 'labels.json').read_text())['cases']


def output(case, evidence):
    label = LABELS[case['case_id']]
    return json.dumps(dict(goal_id=case['goal_id'], action_id=label['expected_action'],
                           constraint_ids=[label['required_constraint']] if label['required_constraint'] else [],
                           evidence_ids=evidence))


def perfect_results():
    results = []
    for case in CASES:
        label = LABELS[case['case_id']]
        selected = [dict(evidence_id=x) for x in label['expected_selection']]
        for arm in ('baseline', 'candidate'):
            supplements = selected if arm == 'candidate' else []
            evidence = [x['evidence_id'] for x in supplements] or ['manual-pin']
            results.append(dict(case_id=case['case_id'], arm=arm, status='complete',
                score=run.parse_score(output(case, evidence), case, label, supplements),
                provider=dict(done=True, models=['fixture'], requested_model='fixture', finish_reasons=['stop'],
                              usage=dict(prompt_tokens=100, completion_tokens=20, total_tokens=120))))
    return results


class EvidenceGates(unittest.TestCase):
    def test_manual_pin_does_not_support_unseen_exception(self):
        for case in CASES:
            label = LABELS[case['case_id']]
            score = run.parse_score(output(case, ['manual-pin']), case, label, [])
            self.assertEqual(score['success'], label['group'] != 'applicable' or case['case_id'] == 'c01')

    def test_supported_improvements_and_positive_control(self):
        summary = run.summarize(perfect_results(), LABELS, True)
        self.assertTrue(summary['advance'])
        self.assertEqual(len(summary['applicable_improvements']), 7)
        self.assertTrue(summary['baseline_positive'])
        self.assertFalse(run.summarize(perfect_results(), LABELS, False)['advance'])

    def test_incomplete_provider_evidence_never_advances(self):
        for mutation in (dict(usage=None), dict(done=False), dict(models=['other']),
                         dict(finish_reasons=['length']), dict(finish_reasons=[]),
                         dict(usage=dict(prompt_tokens=8193, completion_tokens=20, total_tokens=8213)),
                         dict(usage=dict(prompt_tokens=100, completion_tokens=20, total_tokens=121)),
                         dict(usage=dict(prompt_tokens=True, completion_tokens=20, total_tokens=21))):
            with self.subTest(mutation=mutation):
                results = perfect_results()
                results[7]['provider'].update(mutation)
                summary = run.summarize(results, LABELS, True)
                self.assertFalse(summary['advance'])
                self.assertFalse(summary['provider_complete'])
                self.assertIn('c04', summary['incomplete_pairs'])

    def test_new_false_constraint_and_wrong_goal_block_advance(self):
        for field in ('false_constraint', 'wrong_goal', 'ambiguous'):
            results = perfect_results()
            results[3]['score'][field] = True
            results[3]['score']['success'] = False
            self.assertFalse(run.summarize(results, LABELS, True)['advance'])

    def test_unknown_ids_and_duplicate_ids_are_ambiguous(self):
        case = CASES[0]
        for evidence in (['unknown'], ['manual-pin', 'manual-pin']):
            self.assertTrue(run.parse_score(output(case, evidence), case, LABELS['c01'], [])['ambiguous'])


if __name__ == '__main__':
    unittest.main()
