"""Semantic contract regressions, with controls for silent field/variant loss."""
import copy
import gzip
import json
from pathlib import Path
import unittest

import compatibility as c


def doc(schema, definitions=None):
    return {'dialect': 'draft-2020-12', 'schema': schema, 'definitions': definitions or {}}


def obj(properties=None, required=()):
    return {'type': 'object', 'properties': properties or {}, 'required': list(required),
            'additionalProperties': True}


def variant(tag, extra=None):
    return obj({'kind': {'type': 'string', 'enum': [tag]}, **(extra or {})}, ['kind'])


class Compatibility(unittest.TestCase):
    def compare(self, old, new):
        return c.compare(new, old, expectations=())

    def test_deleted_optional_payload_budget_flag_is_breaking(self):
        # This is an explicitly synthetic older schema, not a claim that3991
        # declares this obsolete field or that3814 exported stream internals.
        old = {'stream': doc({'anyOf': [variant('snapshot', {
            'payloadBudgetExceeded': {'type': 'boolean'}, 'projection': obj()})]})}
        self.assertEqual(self.compare(old, copy.deepcopy(old))['status'], 'PASS')
        new = copy.deepcopy(old)
        del new['stream']['schema']['anyOf'][0]['properties']['payloadBudgetExceeded']
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'FAIL')
        self.assertTrue(any('payloadBudgetExceeded' in x['path'] for x in result['breaking']))

    def test_additive_field_rpc_and_discriminated_variant_are_info(self):
        old = {'rpc': doc({'anyOf': [variant('snapshot'), variant('event')]})}
        new = copy.deepcopy(old)
        new['rpc']['schema']['anyOf'].reverse()
        new['rpc']['schema']['anyOf'][0]['properties']['optionalHint'] = {'type': 'string'}
        new['rpc']['schema']['anyOf'].append(variant('future'))
        new['newRpc'] = doc(obj())
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'PASS', result)
        self.assertFalse(result['breaking']); self.assertFalse(result['errors'])
        self.assertGreaterEqual(len(result['informational']), 3)

    def test_removed_rpc_variant_and_new_required_fields_fail(self):
        old = {'rpc': doc({'anyOf': [variant('snapshot'), variant('event')]}),
               'other': doc(obj({'optional': {'type': 'string'}}))}
        for mutate in [lambda n: n.pop('other'),
                       lambda n: n['rpc']['schema']['anyOf'].pop(),
                       lambda n: n['other']['schema']['required'].append('optional'),
                       lambda n: n['other']['schema'].update(
                           properties={'new': {'type': 'string'}}, required=['new'])]:
            new = copy.deepcopy(old); mutate(new)
            with self.subTest(new=new):
                self.assertEqual(self.compare(old, new)['status'], 'FAIL')

    def test_narrowing_types_enums_and_constraints_fail(self):
        for old, new in [({'type': 'number'}, {'type': 'integer'}),
                         ({'type': 'string'}, {'type': 'boolean'}),
                         ({'type': 'string'}, {'type': 'string', 'maxLength': 10}),
                         ({'type': 'integer', 'minimum': 0}, {'type': 'integer', 'minimum': 1}),
                         ({'type': 'string', 'enum': ['a', 'b']}, {'type': 'string', 'enum': ['b']})]:
            with self.subTest(old=old, new=new):
                self.assertEqual(self.compare({'x': doc(old)}, {'x': doc(new)})['status'], 'FAIL')

    def test_widening_and_enum_reordering_pass(self):
        for old, new in [({'type': 'integer'}, {'type': 'number'}),
                         ({'type': 'string', 'maxLength': 10}, {'type': 'string', 'maxLength': 20}),
                         ({'type': 'string', 'enum': ['a', 'b']}, {'type': 'string', 'enum': ['b', 'a', 'c']})]:
            with self.subTest(old=old, new=new):
                self.assertEqual(self.compare({'x': doc(old)}, {'x': doc(new)})['status'], 'PASS')

    def test_nested_discriminators_are_not_array_positions(self):
        def event(tag):
            result = variant('event', {'event': obj({'type': {'type': 'string', 'enum': [tag]},
                                                    'payload': obj()}, ['type'])})
            # The real stream requires event; an optional nested value is not a
            # guaranteed discriminator and must not be used to match variants.
            result['required'].append('event')
            return result
        old = {'x': doc({'anyOf': [event('created'), event('updated')]})}
        new = {'x': doc({'anyOf': [event('updated'), event('future'), event('created')]})}
        self.assertEqual(self.compare(old, new)['status'], 'PASS')
        new['x']['schema']['anyOf'] = [event('updated')]
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'FAIL')
        self.assertTrue(any('event.type=' in x['path'] for x in result['breaking']))

    def test_recursive_refs_and_renamed_definitions_keep_variant_identity(self):
        def tree(name):
            return {'tree': doc({'$ref': '#/$defs/' + name}, {name: {'anyOf': [
                variant('leaf', {'optional': {'type': 'boolean'}}),
                variant('branch', {'children': {'type': 'array', 'items': {'$ref': '#/$defs/' + name}}})]}})}
        old, new = tree('Before'), tree('After')
        new['tree']['definitions']['After']['anyOf'].reverse()
        self.assertEqual(self.compare(old, new)['status'], 'PASS')
        del new['tree']['definitions']['After']['anyOf'][1]['properties']['optional']
        self.assertEqual(self.compare(old, new)['status'], 'FAIL')

    def test_unchanged_inline_ref_must_still_check_changed_definition(self):
        old = {'x': doc(obj({'node': {'$ref': '#/$defs/Node'}}),
                        {'Node': obj({'optional': {'type': 'string'}})})}
        new = copy.deepcopy(old)
        new['x']['definitions']['Node']['properties'].clear()
        self.assertEqual(old['x']['schema'], new['x']['schema'])
        self.assertEqual(self.compare(old, new)['status'], 'FAIL')

    def test_unknown_keyword_bad_refs_and_ambiguous_union_fail_closed(self):
        invalid = [doc({'type': 'object', 'unevaluatedProperties': False}),
                   doc({'$ref': 'https://example.invalid/schema'}),
                   doc({'$ref': '#/$defs/Missing'}),
                   doc({'$ref': '#/$defs/Loop'}, {'Loop': {'$ref': '#/$defs/Loop'}}),
                   doc({'required': 'not-an-array'})]
        for value in invalid:
            with self.subTest(schema=value):
                result = self.compare({'x': value}, {'x': value})
                self.assertEqual(result['status'], 'FAIL'); self.assertTrue(result['errors'])
        old = {'x': doc({'anyOf': [obj({'a': {'type': 'string'}}), obj({'b': {'type': 'integer'}})]})}
        new = {'x': doc({'anyOf': [obj({'c': {'type': 'string'}}), obj({'d': {'type': 'integer'}})]})}
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'FAIL'); self.assertTrue(result['errors'])

    def nullable_usage(self):
        def usage(status):
            return obj({
                'usageStatus': {'type': 'string', 'enum': status},
                'tokens': {'type': 'integer', 'minimum': 0},
                'optionalHint': {'type': 'string'},
            }, ['usageStatus', 'tokens'])
        return {'usage': doc({'anyOf': [
            {'$ref': '#/$defs/Complete'}, {'type': 'null'},
            {'$ref': '#/$defs/Partial'},
        ]}, {'Complete': usage(['complete']),
             'Partial': usage(['partial', 'unavailable'])})}

    def widen_nullable_usage(self, old):
        new = copy.deepcopy(old)
        for branch in new['usage']['definitions'].values():
            branch['properties']['localCommand'] = {
                'anyOf': [{'type': 'boolean'}, {'type': 'null'}]}
        new['usage']['schema']['anyOf'].reverse()
        return new

    def test_nullable_discriminated_union_additions_reconsider_unmatched_branches(self):
        old = self.nullable_usage()
        new = self.widen_nullable_usage(old)
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'PASS', result)
        additions = [item for item in result['informational']
                     if item['path'].endswith('.localCommand')]
        self.assertEqual(len(additions), 2)
        self.assertTrue(all('usageStatus=' in item['path'] for item in additions))

    def test_nullable_discriminated_union_still_rejects_breaking_changes(self):
        old = self.nullable_usage()
        for change in ('optional field removed', 'required field removed',
                       'variant removed', 'required field added', 'type narrowed',
                       'constraint narrowed', 'null removed'):
            new = self.widen_nullable_usage(old)
            complete = new['usage']['definitions']['Complete']
            if change == 'optional field removed':
                del complete['properties']['optionalHint']
            elif change == 'required field removed':
                del complete['properties']['tokens']
                complete['required'].remove('tokens')
            elif change == 'variant removed':
                new['usage']['schema']['anyOf'].pop(0)
            elif change == 'required field added':
                complete['required'].append('localCommand')
            elif change == 'type narrowed':
                complete['properties']['optionalHint'] = {'type': 'boolean'}
            elif change == 'constraint narrowed':
                complete['properties']['tokens']['minimum'] = 1
            else:
                new['usage']['schema']['anyOf'].pop(1)
            with self.subTest(change=change):
                result = self.compare(old, new)
                self.assertEqual(result['status'], 'FAIL', result)
                self.assertTrue(result['breaking'] or result['errors'], result)

    def test_nullable_ambiguous_union_still_fails_closed_after_common_branch(self):
        old = {'x': doc({'anyOf': [
            obj({'a': {'type': 'string'}}), obj({'b': {'type': 'integer'}}),
            {'type': 'null'}]})}
        new = {'x': doc({'anyOf': [
            obj({'c': {'type': 'string'}}), obj({'d': {'type': 'integer'}}),
            {'type': 'null'}]})}
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'FAIL', result)
        self.assertTrue(result['errors'], result)

    def background_tasks(self):
        """T3 OrchestrationV2PendingBackgroundTask as encoded by kindUnionWithFallback:
        four variants tagged by a required kind literal plus one decode-only
        fallback whose kind is optional and not a literal (#2796)."""
        def nullable(kind='string'):
            return {'anyOf': [{'type': kind}, {'type': 'null'}]}
        common = {'taskId': {'type': 'string'}, 'description': nullable(),
                  'parentTaskId': nullable(),
                  'awaited': {'anyOf': [{'type': 'boolean', 'enum': [True]}, {'type': 'null'}]}}
        branches = [variant('subagent', {**common, 'providerInstanceId': nullable(),
                                         'childThreadId': nullable()})]
        branches += [variant(kind, common) for kind in ('command', 'monitor', 'background_task')]
        for branch in branches:
            branch['required'] = ['taskId', 'kind']
        branches.append(obj({**common, 'kind': nullable()}, ['taskId']))
        return {'tasks': doc({'anyOf': [
            {'type': 'array', 'items': {'anyOf': branches}}, {'type': 'null'}]})}

    def task_branches(self, schemas):
        return schemas['tasks']['schema']['anyOf'][0]['items']['anyOf']

    def widen_background_tasks(self, old):
        """Upstream t3code 16a6c1c4 (#33647): optional fields on every branch."""
        new = copy.deepcopy(old)
        for branch in self.task_branches(new):
            for name in ('command', 'itemId', 'sourceThreadId'):
                branch['properties'][name] = {'anyOf': [{'type': 'string'}, {'type': 'null'}]}
        self.task_branches(new).reverse()
        return new

    def test_kind_union_with_fallback_additions_pair_by_literal(self):
        old = self.background_tasks()
        new = self.widen_background_tasks(old)
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'PASS', result)
        self.assertFalse(result['breaking']); self.assertFalse(result['errors'])
        additions = [x['path'] for x in result['informational'] if x['message'] == 'field added']
        self.assertEqual(len(additions), 15, result)
        for kind in ('subagent', 'command', 'monitor', 'background_task'):
            self.assertEqual(sum(f'[kind="{kind}"].' in x for x in additions), 3, additions)
        self.assertEqual(sum('[kind fallback].' in x for x in additions), 3, additions)
        # A new tagged variant beside the fallback is an ordinary informational addition.
        self.task_branches(new).append(variant('future', {'taskId': {'type': 'string'}}))
        result = self.compare(old, new)
        self.assertEqual(result['status'], 'PASS', result)
        self.assertTrue(any(x['message'] == 'variant added' and 'kind="future"' in x['path']
                            for x in result['informational']), result)

    def test_kind_union_with_fallback_still_rejects_breaking_changes(self):
        old = self.background_tasks()
        for change in ('fallback field removed', 'tagged field removed', 'tagged variant removed',
                       'fallback field required', 'tagged field required', 'fallback kind narrowed'):
            new = self.widen_background_tasks(old)
            branches = self.task_branches(new)
            fallback = next(x for x in branches if 'kind' not in x['required'])
            subagent = next(x for x in branches if x['properties']['kind'].get('enum') == ['subagent'])
            if change == 'fallback field removed':
                del fallback['properties']['parentTaskId']
            elif change == 'tagged field removed':
                del subagent['properties']['childThreadId']
            elif change == 'tagged variant removed':
                branches.remove(next(x for x in branches if x['properties']['kind'].get('enum') == ['monitor']))
            elif change == 'fallback field required':
                fallback['required'].append('command')
            elif change == 'tagged field required':
                subagent['required'].append('itemId')
            else:
                fallback['properties']['kind'] = {'type': 'string'}
            with self.subTest(change=change):
                result = self.compare(old, new)
                self.assertEqual(result['status'], 'FAIL', result)
                self.assertTrue(result['breaking'], result)
                self.assertFalse(result['errors'], result)

    def test_kind_union_with_fallback_ambiguity_still_fails_closed(self):
        old = self.background_tasks()
        for change in ('two untagged branches', 'fallback removed', 'fallback added only',
                       'single literal'):
            base, new = copy.deepcopy(old), self.widen_background_tasks(old)
            branches = self.task_branches(new)
            fallback = next(x for x in branches if 'kind' not in x['required'])
            if change == 'two untagged branches':
                branches.append(obj({'other': {'type': 'string'}}, ['other']))
            elif change == 'fallback removed':
                branches.remove(fallback)
            elif change == 'fallback added only':
                self.task_branches(base).pop()
            else:
                # One tagged branch and the fallback: one literal is no discriminator.
                for schemas in (base, new):
                    kept = [x for x in self.task_branches(schemas) if 'kind' not in x['required']
                            or x['properties']['kind'].get('enum') == ['command']]
                    self.task_branches(schemas)[:] = kept
            with self.subTest(change=change):
                result = self.compare(base, new)
                self.assertEqual(result['status'], 'FAIL', result)
                self.assertTrue(result['errors'], result)

    def test_actual_current_consumer_baseline_and_opaque_rpc_are_distinct(self):
        baseline = c.load_baseline()
        result = c.compare(copy.deepcopy(baseline), baseline)
        self.assertEqual(result['status'], 'PASS', result['errors'])
        # The historical RPC schema was Unknown. Decoded roots are separately
        # inspected current3991 contracts, with explicit provenance.
        self.assertEqual(baseline['subscribeThread.output']['schema'], {})
        historical = json.loads((Path(__file__).parent / 'snapshot.json').read_text())
        self.assertNotIn(c.STREAM, historical)
        self.assertNotIn(c.BOUNDED, historical)
        for root in [c.STREAM, c.BOUNDED]:
            for replacement in [None, doc({}), doc(True)]:
                new = copy.deepcopy(baseline)
                if replacement is None:
                    new.pop(root)
                else:
                    new[root] = replacement
                result = c.compare(new, baseline)
                with self.subTest(root=root, replacement=replacement):
                    self.assertEqual(result['status'], 'FAIL'); self.assertTrue(result['errors'])

    def test_deep_optional_consumer_field_removal_is_not_hidden_by3814(self):
        baseline = c.load_baseline()
        candidate = copy.deepcopy(baseline)
        document = c.Document(candidate[c.BOUNDED])
        thread = document.resolve(c._field(document, document.root, 'projection.thread'))
        self.assertIn('snoozedAt', thread['properties'])
        self.assertNotIn('snoozedAt', thread['required'])
        del thread['properties']['snoozedAt']
        result = c.compare(candidate, baseline)
        self.assertEqual(result['status'], 'FAIL')
        self.assertTrue(any('snoozedAt' in change['path'] for change in result['breaking']))

    def test_union_operator_and_overlapping_oneof_fail_closed(self):
        branches = [variant('a'), variant('b')]
        old = {'x': doc({'anyOf': branches})}
        changed = {'x': doc({'oneOf': copy.deepcopy(branches)})}
        result = self.compare(old, changed)
        self.assertEqual(result['status'], 'FAIL'); self.assertTrue(result['errors'])
        ambiguous = {'x': doc({'oneOf': [{'type': 'string'}, {'type': 'string'}]})}
        result = self.compare(ambiguous, ambiguous)
        self.assertEqual(result['status'], 'FAIL'); self.assertTrue(result['errors'])
        tagged = {'x': doc({'oneOf': [variant('b'), variant('a'), variant('c')]})}
        self.assertEqual(self.compare(changed, tagged)['status'], 'PASS')

    def test_fingerprint_is_bounded_for_deeply_nested_contracts(self):
        schema = {'type': 'boolean'}
        for _ in range(32):
            schema = obj({'child': schema}, ['child'])
        document = c.Document(doc(schema))
        fingerprint = document.fingerprint(document.root)
        self.assertLess(len(fingerprint), 1024)
        self.assertEqual(fingerprint, document.fingerprint(document.root))

    def test_current_consumer_baseline_provenance_and_deterministic_gzip(self):
        directory = Path(__file__).parent
        meta = json.loads((directory / 'consumer-baseline.json').read_text())
        self.assertEqual(meta['tag'], 'v0.1.7-nightly.20261031.3991')
        self.assertEqual(meta['source_commit'], '15fe2832727893cbdd2214383f9f971474849695')
        packed = (directory / meta['snapshot']).read_bytes()
        self.assertEqual(packed[4:8], bytes(4))
        self.assertEqual(set(json.loads(gzip.decompress(packed))), {c.STREAM, c.BOUNDED})


if __name__ == '__main__':
    unittest.main()
