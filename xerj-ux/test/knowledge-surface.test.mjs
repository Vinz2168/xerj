// The knowledge surface — one read that answers, the moment indexing
// finishes: how large the corpus is, which data is in it, and what can be
// done with it. These tests pin the DATA (payload → cards/rows/strip),
// never the styling. Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';

// renderCorpus and friends are pure safe-dom; set a fetch stub before the
// imports that could touch the network.
const calls = [];
let respond = () => json(200, {});
globalThis.fetch = async (url, init) => { calls.push(String(url)); return respond(String(url), init); };
function json(status, body) {
  return { status, ok: status >= 200 && status < 300, json: async () => body, text: async () => JSON.stringify(body) };
}

const { parseKnowledge } = await import('../src/data/catalog.js');
const { renderCorpusCard, renderRelations, renderCapabilities } = await import('../src/ux/corpus-render.js');
const { corpus } = await import('../src/dashboards/corpus.js');
const { textOf, findAll } = await import('../src/ux/safe-dom.js');

// A payload shaped like GET /_xerj-console/api/v1/knowledge (pinned
// server-side by xerj-console-api/tests/knowledge_surface.rs — the same
// fixture facts, so the two suites cannot drift).
const PAYLOAD = () => ({ data: {
  catalog: true,
  totals: { datasets: 2, records: 105, files: 18, bytes: 3_594_132, docs: 4, relations: 2 },
  datasets: [
    {
      index: 'ax-mail', slug: 'mail', formats: ['eml'], records: 91, junk: 2, files: 14,
      bytes: 482_133, live_docs: 3, store_bytes: 190_000,
      time_field: 'email_date', time_min: '2026-01-05T00:00:00.000Z', time_max: '2026-09-19T00:00:00.000Z',
      semantic_field: 'body',
      fields: [
        { name: 'email_from', type: 'keyword', semantic: false, cardinality: 12, cardinality_overflow: false, null_ratio: 0.0, coverage: 1.0, examples: ['sam@acme.example', 'jo@grid.example'] },
        { name: 'body', type: 'semantic_text', semantic: true, cardinality: 91, cardinality_overflow: false, null_ratio: 0.0, coverage: 1.0, examples: ['term sheet attached'] },
        { name: 'page', type: 'long', semantic: false, cardinality: 4, cardinality_overflow: false, null_ratio: 0.75, coverage: 0.25, examples: ['1', '2'] },
      ],
      sample_queries: ['{"query":{"match":{"body":"term sheet"}},"size":3}'],
      notes: [], run_id: 'run-1',
    },
    { index: 'ax-pdfs', slug: 'pdfs', formats: ['pdf'], records: 14, junk: 0, files: 4, bytes: 3_111_999, live_docs: 1, store_bytes: 88_000, semantic_field: 'text', fields: [], sample_queries: [], notes: [], run_id: 'run-1' },
  ],
  others: [{ index: 'weblogs', docs: 1, store_bytes: 4_096 }],
  relations: [
    { kind: 'key_overlap', a_dataset: 'mail', a_index: 'ax-mail', a_field: 'email_from', b_dataset: 'pdfs', b_index: 'ax-pdfs', b_field: 'title', overlap: 7, containment: 0.58, grade: 'likely', confirmed_values: 7, tested_values: 12, examples: ['sam@acme.example'] },
    { kind: 'time_alignment', a_index: 'ax-mail', a_field: 'email_date', b_index: 'ax-pdfs', b_field: 'title', range_overlap: 0.62, shared_buckets: 31, pearson_r: 0.71, activity_correlated: true },
  ],
  brains: [{ name: 'casefile', nodes_index: 'ax-mail', links: 2 }],
  capabilities: [
    { id: 'search', title: 'Search it', blurb: 'Seven query types.', href: '#/discover', kind: 'console' },
    { id: 'read', title: 'Read the documents', blurb: 'Page through records.', href: '#/reader', kind: 'console' },
    { id: 'graph', title: 'See the links', blurb: "Brain 'casefile' holds 2 links.", href: '#/dashboards/second-brain?brain=casefile', kind: 'console' },
    { id: 'map', title: 'Print the data map', blurb: 'Markdown for agents.', command: 'xerj autoindex map', kind: 'cli' },
  ],
} });

test('parseKnowledge: the payload → the corpus state, every gate answered', () => {
  const st = parseKnowledge(PAYLOAD());
  assert.equal(st.status, 'ok');
  // gate 1 — size
  assert.equal(st.totals.records, 105);
  assert.equal(st.totals.bytes, 3_594_132);
  assert.equal(st.totals.datasets, 2);
  assert.equal(st.totals.catalog, true);
  // gate 2 — data: datasets in card shape, largest first, with the LIVE counts
  assert.equal(st.datasets[0].index, 'ax-mail');
  assert.equal(st.datasets[0].liveDocs, 3);
  assert.equal(st.datasets[0].storeBytes, 190_000);
  assert.equal(st.datasets[0].fields[2].nullRatio, 0.75);
  assert.deepEqual(st.summaries[0], { index: 'weblogs', records: 1, storeBytes: 4096, emails: 0, attachments: 0, formats: [] });
  // gate 2b — relations, in display shape
  assert.equal(st.relations[0].kind, 'key_overlap');
  assert.equal(st.relations[0].confirmed, 7);
  assert.equal(st.relations[1].kind, 'time_alignment');
  assert.equal(st.relations[1].pearsonR, 0.71);
  // gate 3 — capabilities + brains
  assert.equal(st.capabilities.length, 4);
  assert.equal(st.brains[0].links, 2);
  // a malformed payload degrades to empty, never throws
  const bad = parseKnowledge({ data: null });
  assert.equal(bad.status, 'ok');
  assert.deepEqual(bad.datasets, []);
});

test('a card shows the field facts the catalog measured — as text, not a hover hint', () => {
  const st = parseKnowledge(PAYLOAD());
  const card = renderCorpusCard(st.datasets[0], {});
  const text = textOf(card);
  // the example value is VISIBLE (it lived only in a title attribute before)
  assert.match(text, /sam@acme\.example/);
  // coverage and null% are the catalog's own numbers
  assert.match(text, /75% null/);
  assert.match(text, /25%/); // page coverage
  assert.match(text, /12 distinct/); // email_from cardinality
  // the LIVE count is the headline number, not the run's stale one
  assert.match(text, /3 records/);
  assert.doesNotMatch(text, /91 records/);
});

test('a dataset with more than eight fields folds the rest into an ALL FIELDS disclosure — nothing is capped away', () => {
  const st = parseKnowledge(PAYLOAD());
  const many = { ...st.datasets[0], fields: Array.from({ length: 11 }, (_, i) => ({ name: `f${i}`, type: 'keyword', semantic: false, coverage: 1, cardinality: null, nullRatio: null, examples: [] })) };
  const card = renderCorpusCard(many, {});
  const details = findAll(card, (n) => n.tag === 'details');
  assert.equal(details.length, 1);
  assert.match(textOf(details[0]), /ALL 11 FIELDS/);
  // every field name is reachable in the payload the card renders
  assert.match(textOf(card), /f10/);
});

test('renderRelations: what autoindex inferred — both kinds, and an honest none', () => {
  const st = parseKnowledge(PAYLOAD());
  const rows = renderRelations(st);
  const text = textOf(rows);
  assert.match(text, /ax-mail\.email_from\s+⟷\s+ax-pdfs\.title/);
  assert.match(text, /7 shared values/);
  assert.match(text, /likely/);
  assert.match(text, /7\/12 confirmed by query/);
  assert.match(text, /e\.g\. sam@acme\.example/);
  assert.match(text, /ranges overlap 62%/);
  assert.match(text, /activity r=0\.71 · correlated/);
  // none inferred is stated, not papered over
  assert.match(textOf(renderRelations({ relations: [] })), /No cross-dataset relations were inferred/);
});

test('renderCapabilities: every entry names its real surface; hrefs are in-app routes', () => {
  const st = parseKnowledge(PAYLOAD());
  const strip = renderCapabilities(st);
  const cards = findAll(strip, (n) => (n.attrs.class || '').split(' ').includes('cp-cap'));
  assert.equal(cards.length, 4);
  const text = textOf(strip);
  assert.match(text, /Search it/);
  assert.match(text, /xerj autoindex map/); // the CLI command is shown verbatim
  for (const c of cards) {
    const a = findAll(c, (n) => n.tag === 'a')[0];
    if (a) assert.match(a.attrs.href, /^#\//, 'a capability link is an in-app hash route');
  }
  // nothing to show → nothing claimed
  assert.equal(renderCapabilities({ capabilities: [] }), null);
});

test('the CORPUS scene answers "how large" in its meta line and adds the two panels', () => {
  const st = parseKnowledge(PAYLOAD());
  const view = corpus.render({ data: st });
  assert.ok(view.meta.some((m) => /105 RECORDS/.test(m)), view.meta.join(' | '));
  assert.ok(view.meta.some((m) => /3\.6 MB|3\.4 MB/.test(m)), view.meta.join(' | '));
  assert.deepEqual(view.panels.map((p) => p.id), ['datasets', 'relations', 'capabilities']);
  // the legacy path (no totals) keeps the one panel it always had
  const legacy = corpus.render({ data: { status: 'ok', datasets: st.datasets, summaries: [], totals: null, relations: [], capabilities: [] } });
  assert.deepEqual(legacy.panels.map((p) => p.id), ['datasets']);
  assert.ok(legacy.meta.some((m) => /105 RECORDS/.test(m)), `the cards' own records sum: ${legacy.meta.join(' | ')}`);
});

test('liveCorpus reads ONE endpoint and keeps the never-mock contract', async () => {
  const { query, NEVER_MOCK } = await import('../src/data/query.js');
  assert.ok(NEVER_MOCK.has('corpus'));
  calls.length = 0;
  respond = (url) => (url.endsWith('/api/v1/knowledge') ? json(200, PAYLOAD()) : json(404, {}));
  const r = await query({ dashId: 'corpus' });
  assert.equal(calls.length, 1, 'exactly one request: the knowledge surface');
  assert.ok(calls[0].endsWith('/_xerj-console/api/v1/knowledge'));
  assert.equal(r.data.datasets.length, 2);
  assert.equal(r.data.relations.length, 2);
  assert.equal(r.data.capabilities.length, 4);
  // engine unreachable → an error and empty lists, never a sample
  calls.length = 0;
  respond = () => { throw new TypeError('Failed to fetch'); };
  const err = await query({ dashId: 'corpus' });
  assert.equal(err.meta.sourceKind, 'live-error');
  assert.equal(err.data.status, 'error');
  assert.deepEqual(err.data.datasets, []);
  assert.deepEqual(err.data.relations, []);
});
