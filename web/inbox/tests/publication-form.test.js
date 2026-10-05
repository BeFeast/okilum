import test from 'node:test';
import assert from 'node:assert/strict';
import { publicationProblem } from '../publication-form.js';
test('requires an explicit allowed folder, Markdown filename and bounded content', () => {
 const valid = { folder: 'Projects', filename: 'Идея.md', content: '# Idea' };
 assert.equal(publicationProblem(valid, ['Projects']), '');
 assert.match(publicationProblem({ ...valid, folder: '' }, ['Projects']), /Choose a PARA folder/);
 assert.match(publicationProblem(valid, ['Areas']), /Choose a PARA folder/);
 assert.match(publicationProblem({ ...valid, filename: '../Idea.md' }), /filename/);
 assert.match(publicationProblem({ ...valid, content: 'я'.repeat(32769) }), /64 KB/);
});

test('legacy missing destination and filename identifies both fields, without blaming preserved content', async () => {
 const { publicationProblems } = await import('../publication-form.js');
 const errors = publicationProblems({ folder: '', filename: '', content: '# Saved draft' }, ['Projects']);
 assert.deepEqual(Object.keys(errors), ['destination', 'filename']);
 assert.deepEqual(Object.keys(publicationProblems({ folder: 'Projects', filename: '', content: '# Saved draft' }, ['Projects'])), ['filename']);
});

test('title suggestion and implied extension preserve Unicode while paths stay relative', async () => {
 const { markdownFilename, suggestedFilename, publicationLabel } = await import('../publication-form.js');
 assert.equal(suggestedFilename('Intro\n# План: Inbox / исполнение\nBody'), 'План  Inbox   исполнение');
 assert.equal(markdownFilename('tessera/Мой план'), 'tessera/Мой план.md');
 assert.equal(markdownFilename('idea.md'), 'idea.md');
 assert.equal(markdownFilename(''), '');
 const valid = { folder: 'Projects', filename: 'tessera/Мой план.md', content: '# Plan' };
 assert.equal(publicationProblem(valid), '');
 for (const filename of ['/plan.md', '../plan.md', 'a/../plan.md', 'a//plan.md', 'a/.git/plan.md', 'a\\plan.md']) assert.ok(publicationProblem({ ...valid, filename }));
 assert.equal(publicationLabel({ state: 'published' }), 'Published');
 assert.equal(publicationLabel({ state: 'prepared', conflict: 'file_exists' }), 'Not published: file exists');
 assert.equal(publicationLabel({ state: 'prepared', conflict: 'occupied' }), 'File exists; earlier delivery not confirmed');
 assert.match(publicationLabel({ state: 'prepared' }), /Not confirmed/);
});
