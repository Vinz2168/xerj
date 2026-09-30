// ============================================================
// Section — CORPUS  (the home page after `xerj brain <folder>`)
//
// The knowledge surface. One read — GET /_xerj-console/api/v1/knowledge —
// answers the three questions a person has the moment indexing finishes:
//
//   HOW LARGE   the scene meta line: datasets, records, source bytes
//               (whole-corpus totals, computed server-side)
//   WHICH DATA  one card per dataset: live doc count and store bytes,
//               formats, time span, the FULL field list with types,
//               coverage, null% and example values, and the sample
//               queries autoindex wrote — plus any other index the
//               engine holds; then the cross-dataset relations
//               autoindex actually inferred (key overlaps, time
//               alignments — an empty list is shown as none, never
//               guessed)
//   WHAT NOW    a capability strip grounded in real surfaces: console
//               routes, CLI commands, HTTP endpoints the node really
//               serves (the server computes availability from facts;
//               this module adds nothing)
//
// Data comes from `backends/xerj.js#liveCorpus` and is listed in
// data/query.js#NEVER_MOCK: this page shows what the engine reports, or an
// error — never a sample.
//
// The panels below are MOUNT POINTS, not markup. Everything on a card can
// be document-derived, so the shell (app.js#mountSafePanels) fills them
// with nodes from ux/corpus-render.js through ux/safe-dom.js. No document
// string is ever interpolated into the HTML this module returns.
// ============================================================

import { fmtCount, fmtBytes } from '../data/catalog.js';

export const CORPUS_MOUNT = '<div data-safe-mount="corpus" class="cp-mount"></div>';
export const RELATIONS_MOUNT = '<div data-safe-mount="corpus-relations" class="cp-mount"></div>';
export const CAPABILITIES_MOUNT = '<div data-safe-mount="corpus-capabilities" class="cp-mount"></div>';

export const corpus = {
  id:   'corpus',
  name: 'Corpus',
  section: 'corpus',
  render: ({ data }) => {
    const datasets = data?.datasets || [];
    const summaries = data?.summaries || [];
    const n = datasets.length + summaries.length;
    const totals = data?.totals || null;
    const hasKnowledge = !!totals;
    // Gate 1 — how large. The catalog's own whole-corpus numbers when the
    // knowledge endpoint served them; the cards' sums otherwise.
    const total = totals
      ? totals.records
      : datasets.reduce((acc, c) => acc + c.records, 0) + summaries.reduce((acc, c) => acc + c.records, 0);
    const meta = data?.status === 'error'
      ? ['CATALOG UNREADABLE']
      : (n ? [
        `${n} DATASET${n === 1 ? '' : 'S'}`,
        `${fmtCount(total)} RECORDS`,
        ...(totals && totals.bytes ? [fmtBytes(totals.bytes)] : []),
      ] : ['EMPTY']);
    const relations = data?.relations;
    const capabilities = data?.capabilities || [];
    return {
      title:  'CORPUS',
      kicker: 'WHAT IS INDEXED',
      meta,
      caption: n
        ? 'Everything xerj brain / xerj autoindex wrote to this engine — size, structure, relations and what you can run against it — read live, plus any other index it holds. Open a dataset in the Reader, or run one of the sample queries its catalog entry carries.'
        : '',
      panels: [
        { id: 'datasets', eyebrow: n ? 'ONE CARD PER DATASET · LIVE FROM autoindex-catalog' : 'WHAT IS INDEXED', cols: 12, type: 'corpus',
          render: () => CORPUS_MOUNT,
        },
        ...(hasKnowledge ? [{ id: 'relations', eyebrow: 'RELATIONS AUTOINDEX INFERRED BETWEEN DATASETS · NOTHING GUESSED', cols: 12, type: 'corpus',
          render: () => RELATIONS_MOUNT }] : []),
        ...(capabilities.length ? [{ id: 'capabilities', eyebrow: 'WHAT YOU CAN DO WITH THIS CORPUS · EVERY ENTRY IS A REAL ROUTE, COMMAND OR ENDPOINT', cols: 12, type: 'corpus',
          render: () => CAPABILITIES_MOUNT }] : []),
      ],
    };
  },
};
