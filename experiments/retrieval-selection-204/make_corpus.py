#!/usr/bin/env python3
"""Generate synthetic fixtures and separate ground truth before provider evaluation."""
import json
import pathlib
import uuid

ROOT = pathlib.Path(__file__).parent
# Each situation models a local contract that cannot be inferred from general advice.
THEMES = [
    ('lease', 'Release the synthetic Cedar build workspace for a new job.',
     {'adapter': 'cedar-v1', 'lease_scope': 'workspace'}, {'lease_scope': 'process'},
     'release-direct', 'Release the job process lease, then start the new job.',
     'release-workspace', 'Release the workspace lease, then start the new job.',
     'workspace-release-required',
     'A verified replay on cedar-v1 with workspace lease scope left the workspace busy after process release. Release the workspace lease before the new job.'),
    ('archive', 'Export the synthetic Amber project with its linked drawings.',
     {'exporter': 'amber-v2', 'attachment_mode': 'external'}, {'attachment_mode': 'embedded'},
     'export-default', 'Run the default archive export.',
     'export-linked', 'Include the external attachment inventory in the archive export.',
     'attachment-inventory-required',
     'A verified restore on amber-v2 with external attachments lost linked drawings in the default export. Include the external attachment inventory.'),
    ('calendar', 'Schedule the next synthetic Lumen recurring appointment.',
     {'scheduler': 'lumen-v1', 'clock_mode': 'wall'}, {'clock_mode': 'absolute'},
     'repeat-interval', 'Use the ordinary fixed interval recurrence.',
     'repeat-wall', 'Use the local wall-clock recurrence rule.',
     'wall-clock-rule-required',
     'A verified replay on lumen-v1 in wall clock mode shifted the appointment after the offset change when fixed intervals were used. Use the local wall-clock recurrence rule.'),
    ('supplier', 'Book a synthetic Harbor equipment collection.',
     {'supplier': 'harbor-north', 'item_class': 'oversize'}, {'item_class': 'standard'},
     'book-default', 'Book using the ordinary pickup form.',
     'book-reserved', 'Reserve the loading bay before confirming pickup.',
     'loading-bay-reservation-required',
     'A verified Harbor north oversize pickup failed despite a confirmed ordinary form because no loading bay was allocated. Reserve the loading bay before confirming pickup.'),
    ('callback', 'Recover a synthetic Flint remote job after a missing callback.',
     {'engine': 'flint-v1', 'callback_contract': 'best-effort'}, {'callback_contract': 'durable'},
     'retry-submit', 'Use the ordinary recovery action: submit a replacement job.',
     'lookup-job', 'Read the original job status by its saved identity before any resubmission.',
     'original-status-check-required',
     'A verified replay on flint-v1 with best-effort callbacks created duplicate jobs after resubmission. Query the original saved job identity before resubmission.'),
    ('migration', 'Apply the synthetic Quartz schema update.',
     {'migrator': 'quartz-v3', 'writer_mode': 'continuous'}, {'writer_mode': 'snapshot'},
     'migrate-online', 'Run the ordinary online migration.',
     'pause-writer', 'Pause the continuous writer while applying the migration.',
     'writer-pause-required',
     'A verified quartz-v3 continuous-writer replay left mixed schema rows during online migration. Pause the continuous writer during the migration.'),
    ('embedding', 'Refresh the synthetic Iris semantic index after a model switch.',
     {'indexer': 'iris-v2', 'cache_key': 'text-only'}, {'cache_key': 'model-and-text'},
     'refresh-incremental', 'Use the ordinary incremental index refresh.',
     'rebuild-vectors', 'Rebuild vectors for the new model before publishing the index.',
     'vector-rebuild-required',
     'A verified iris-v2 replay with text-only cache keys reused old model vectors during incremental refresh. Rebuild vectors for the new model before publishing.'),
    ('attention', 'Finish a synthetic Vale attention request after a mobile answer.',
     {'channel': 'vale-v1', 'ack_scope': 'delivery'}, {'ack_scope': 'decision'},
     'close-on-ack', 'Use the ordinary completion action after the mobile acknowledgement.',
     'read-decision', 'Read the persisted decision before closing the request.',
     'persisted-decision-required',
     'A verified vale-v1 replay with delivery-scope acknowledgements closed a request without saving a decision. Read the persisted decision before closure.'),
]

def main():
    cases, labels = [], {}
    for theme_index, theme in enumerate(THEMES):
        topic, title, matching, nonmatching, ordinary, ordinary_text, special, special_text, constraint, finding = theme
        for variant_index, variant in enumerate(('applicable', 'inapplicable', 'obsolete')):
            number = theme_index * 3 + variant_index + 1
            cid = f'c{number:02d}'
            gid = str(uuid.uuid5(uuid.NAMESPACE_URL, 'okilum/experiment204/' + cid))
            facts = dict(matching)
            if variant == 'inapplicable':
                facts.update(nonmatching)
            incident = dict(incident_id=f'I{number:02d}', topic=topic,
                            verification='verified', applies_when=matching,
                            obsolete_when={'retired': True}, retired=False,
                            finding=finding, recommended_action=special,
                            constraint_id=constraint)
            current = dict(incident, retired=variant == 'obsolete')
            pinned = variant == 'applicable' and theme_index == 0
            manual = ('Synthetic local runbook: the ordinary documented action is ' + ordinary + '. '
                      'Apply a different action only when current verified local evidence establishes a matching exception. '
                      'Absent such evidence, perform the documented action; speculative constraints delay this goal.')
            if pinned:
                manual += '\nAlready reviewed incident: ' + finding + '\nEvidence ID: manual-pin. Constraint ID: ' + constraint + '.'
            case = dict(case_id=cid, goal_id=gid, topic=topic, title=title, facts=facts,
                        manual_pin=manual, incident_initial=incident, incident_current=current,
                        actions={ordinary: ordinary_text, special: special_text,
                                 'request-clarification': 'Request clarification instead of choosing an executable next action.'},
                        constraint_catalog={constraint: special_text},
                        source_path=f'incidents/{cid}.md', manual_path=f'manual/{cid}.md')
            cases.append(case)
            labels[cid] = dict(group=variant, expected_action=special if variant == 'applicable' else ordinary,
                               required_constraint=constraint if variant == 'applicable' else None,
                               forbidden_constraints=[] if variant == 'applicable' else [constraint],
                               expected_selection=[incident['incident_id']] if variant == 'applicable' else [],
                               baseline_positive_control=pinned,
                               prevention='Correct executable next action and exactly the justified constraint, supported by current evidence; no unknown actions or evidence.',
                               false_constraint='Any constraint or exceptional action in a control; an unexecutable/ambiguous answer is separately unsuccessful.')
    (ROOT / 'corpus.json').write_text(json.dumps({'schema':'okilum-experiment204-corpus/v1','synthetic':True,'cases':cases}, indent=2)+'\n')
    (ROOT / 'labels.json').write_text(json.dumps({'schema':'okilum-experiment204-labels/v1','cases':labels}, indent=2)+'\n')

if __name__ == '__main__':
    main()
