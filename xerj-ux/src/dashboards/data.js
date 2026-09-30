// ============================================================
// Section — DATA
//
// Index / field inventory of the built-in engine. Every row on this
// page comes from a real endpoint the console serves:
//   GET /_xerj-console/api/v1/data-sources/connections
//   GET …/connections/built-in/indices          (names, docs, store bytes)
//   GET …/connections/built-in/indices/:i/fields (names, types)
// An unreachable engine yields empty tables and a line saying so — never
// sample clusters or seeded index names (data/data-sources.js is
// real-only). Sizes, per-dataset fields with coverage/examples and the
// capability strip live on CORPUS, the knowledge surface; this page is
// the flat inventory under it.
// ============================================================

import { esc }                 from '../ux/text.js';
import { Markdown }            from '../ux/tables.js';
import { defaultClusterId }    from '../data/data-sources.js';

const humanCount = (n) => {
  if (n == null) return '—';
  if (n >= 1e9) return (n / 1e9).toFixed(1) + 'B';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(0) + 'K';
  return String(n);
};
const humanBytes = (b) => {
  if (b == null) return '—';
  const u = ['B','KB','MB','GB','TB','PB'];
  let v = b, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return v.toFixed(v < 10 ? 1 : 0) + ' ' + u[i];
};

export const dataSection = {
  id: 'data',
  name: 'Data',
  section: 'data',
  render: ({ data, time }) => {
    const clusters = data.clusters || [];
    const indicesByCluster = data.indicesByCluster || {};
    const fieldsByIndex = data.fieldsByIndex || {};
    const active = data.activeCluster || 'local';
    const activeIndices = indicesByCluster[active] || [];
    const focusIndex = data.focusIndex || activeIndices[0]?.name;
    const fields = fieldsByIndex[focusIndex] || [];
    const unreachable = !clusters.length;

    return {
      title: 'DATA',
      kicker: 'CONNECTIONS · INDICES · FIELDS',
      meta: [time, 'SOURCES'],
      caption: unreachable
        ? 'No data source could be read. This page lists only what the engine reports — sign in, or start the node, and reload.'
        : 'What the engine actually has: one row per index (documents, on-disk store bytes) and each index\'s mapping. Per-dataset structure, coverage and examples live on CORPUS.',
      panels: [

        { id: 'clusters', eyebrow: 'CONNECTIONS · CLICK TO SET DEFAULT', cols: 12, type: 'clusters',
          render: () => renderClusters(clusters, active),
        },

        { id: 'indices', eyebrow: `INDICES · ${active.toUpperCase()} · CLICK AN INDEX TO INSPECT FIELDS`, cols: 6, type: 'indices',
          render: () => renderIndices(activeIndices, focusIndex),
        },

        { id: 'fields', eyebrow: `FIELDS · ${focusIndex || '—'} · FROM THE INDEX MAPPING`, cols: 6, type: 'fields',
          render: () => renderFields(fields),
        },

        { id: 'howTo', eyebrow: 'WHERE THESE NUMBERS COME FROM', cols: 12, type: 'markdown',
          render: () => Markdown(
`## One connection, read live

This node has one data source — its built-in engine — listed by
\`GET /_xerj-console/api/v1/data-sources/connections\`. Indices and fields
come from the same facade. Doc counts and store bytes are the engine's own
(\`Index::stats\` and the index's data directory), not estimates.

The default connection is stored in \`localStorage.xerj.cluster\`, visible
under SETTINGS. The knowledge surface — sizes, per-dataset fields,
relations, capabilities — is CORPUS, the section this inventory sits
under.`
          ),
        },

      ],
    };
  },
};

// ---------- renderers -----------------------------------

function renderClusters(clusters, active) {
  if (!clusters.length) return '<div class="mono faint">No data source readable. Sign in and reload — this page never shows a sample.</div>';
  const rows = clusters.map((c) => {
    const isActive = c.id === active;
    const status = {
      green:  `<span class="mono accent">●</span>`,
      yellow: `<span class="mono faint">◐</span>`,
      red:    `<span class="mono">○</span>`,
    }[c.status] || '—';
    return `
      <button type="button" class="mg-cluster${isActive ? ' mg-cluster-active' : ''}" data-mg-cluster="${esc(c.id)}">
        <span class="mg-cluster-status">${status}</span>
        <span class="mg-cluster-name mono${isActive ? ' accent' : ''}">${esc(c.name)}</span>
        <span class="mg-cluster-url mono faint">${esc(c.url)}</span>
        <span class="mg-cluster-stat mono">${humanCount(c.indices)}&nbsp;idx</span>
        <span class="mg-cluster-stat mono">${humanCount(c.docs)}&nbsp;docs</span>
        <span class="mg-cluster-ver mono faint">${esc(c.version || '')}</span>
      </button>`;
  }).join('');
  return `<div class="mg-clusters">${rows}</div>`;
}

function renderIndices(indices, focusIndex) {
  if (!indices.length) return '<div class="mono faint">No indices on this engine.</div>';
  const cols = ['NAME', 'DOCS', 'SIZE', 'SHARDS'];
  const headRow = `<div class="mg-idx-row mg-idx-head">${cols.map((c) => `<span>${esc(c)}</span>`).join('')}</div>`;
  const body = indices.map((i) => {
    const cells = [
      `<button type="button" class="mg-idx-btn${i.name === focusIndex ? ' active' : ''}" data-mg-index="${esc(i.name)}">${esc(i.name)}</button>`,
      humanCount(i.docs),
      humanBytes(i.bytes),
      String(i.shards),
    ];
    return `<div class="mg-idx-row">${cells.map((c) => `<span>${c}</span>`).join('')}</div>`;
  }).join('');
  return `<div class="mg-idx-table">${headRow}${body}</div>`;
}

function renderFields(fields) {
  if (!fields.length) return '<div class="mono faint">No mapping for this index.</div>';
  // FIELD and TYPE only: cardinality, encoding and compression ratio are
  // not on this endpoint, and a column that guesses from the type is a
  // claim about the on-disk format this page cannot make. The measured
  // per-field facts (coverage, null%, examples) live on CORPUS, from the
  // autoindex catalog.
  const cols = ['FIELD', 'TYPE'];
  const headRow = `<div class="mg-fld-row mg-fld-head">${cols.map((c) => `<span>${esc(c)}</span>`).join('')}</div>`;
  const body = fields.map((f) => `
    <div class="mg-fld-row">
      <span class="mono">${esc(f.name)}</span>
      <span class="mono faint">${esc(f.type)}${f.semantic ? ' · semantic' : ''}</span>
    </div>`).join('');
  return `<div class="mg-fld-table">${headRow}${body}</div>`;
}
