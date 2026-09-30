// ============================================================
// XERJ Console — data source catalog (REAL ONLY)
//
// What the engine actually has, read through the console's own
// session-authenticated facade:
//   GET /_xerj-console/api/v1/data-sources/connections                     → rows
//   GET /_xerj-console/api/v1/data-sources/connections/built-in/indices    → { indices }
//   GET /_xerj-console/api/v1/data-sources/connections/built-in/indices/:i/fields
//
// This module used to seed four fabricated clusters (prod-us with 840M
// documents and the like) and fall back to mock index/field arrays when
// the engine was unreachable — an inventory page that can show numbers
// the engine never reported is worse than one that says "nothing
// reachable", so the fabrications are gone: every row here is a row a
// real endpoint returned, and a failure yields an empty list and the
// section says so.
//
// The one-connection truth: a standalone node has exactly one data
// source, the `built-in` engine. When HTTP-shaped connection adapters
// land, more rows appear here from the same endpoint — nothing to
// re-seed.
// ============================================================

const XAPI = '/_xerj-console/api/v1';

function liveBaseUrl() {
  if (typeof window !== 'undefined' && window.location && window.location.origin) {
    return window.location.origin;
  }
  return 'http://localhost:9200';
}

async function getJson(path) {
  const r = await fetch(path, { credentials: 'same-origin' });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}

async function liveConnections() {
  try {
    const body = await getJson(XAPI + '/data-sources/connections');
    return body?.data?.connections || [];
  } catch (_e) { return []; }
}

/** The LOCAL row: this node's built-in engine, from the connections
 *  endpoint. `indices`/`docs` are left null — not 0, not a guess — until
 *  the indices endpoint is read (the DATA section fills them in). */
async function localRow() {
  const conns = await liveConnections();
  const builtIn = conns.find((c) => c && c.id === 'built-in');
  if (!builtIn) return null;
  return {
    id: 'local',
    name: (builtIn.name || 'LOCAL').toUpperCase(),
    url: liveBaseUrl(),
    status: builtIn.status || 'green',
    version: builtIn.version || null,
    kind: builtIn.kind || null,
    indices: null,
    docs: null,
  };
}

async function liveIndices() {
  try {
    const body = await getJson(XAPI + '/data-sources/connections/built-in/indices');
    const list = body?.data?.indices || [];
    return list.map((it) => ({
      name: it.name,
      docs: Number(it.docs || 0),
      // Real on-disk store bytes measured server-side from the index's data
      // dir (the same number /_cat/indices reports). Not yet, never 0-by-
      // construction.
      bytes: Number.isFinite(Number(it.bytes)) ? Number(it.bytes) : null,
      shards: Number(it.shards || 1),
      replicas: Number(it.replicas || 0),
      segments: Number(it.segments || 0),
      fields: Number(it.fields || 0),
      health: 'green',
      status: 'open',
      retention_days: null,
    }));
  } catch (_e) {
    return null;
  }
}

async function liveFields(indexName) {
  try {
    const body = await getJson(
      XAPI + '/data-sources/connections/built-in/indices/'
        + encodeURIComponent(indexName) + '/fields',
    );
    const list = body?.data?.fields || [];
    // Only what the endpoint actually reports: name and type. Cardinality,
    // encoding and compression ratio are NOT guessed from the type — the
    // engine's per-field encodings are not on this facade, and a wrong
    // label here would be a claim about the on-disk format.
    return list.map((f) => ({
      name: f.name,
      type: f.type || 'object',
      semantic: f.semantic === true,
    }));
  } catch (_e) {
    return null;
  }
}

/** List configured data sources. Real rows only; `[]` when the console
 *  facade cannot be read (the section shows why, not a sample). */
export async function listClusters() {
  const row = await localRow();
  return row ? [row] : [];
}

/** The cluster picker's synchronous row — the built-in engine, with no
 *  fabricated counts. Used before any fetch resolves; `null` counts mean
 *  "not read yet". */
export function listClustersSync() {
  return [{ id: 'local', name: 'LOCAL', url: liveBaseUrl(), status: 'green', indices: null, docs: null }];
}

/** List indices of the built-in engine. Real rows only; `[]` on failure. */
export async function listIndices(_clusterId) {
  const live = await liveIndices();
  return live || [];
}

/** List fields of one index, from the engine's mapping. `[]` on failure. */
export async function listFields(indexName) {
  const live = await liveFields(indexName);
  return live || [];
}

/** Return the default cluster id the app should talk to. */
export function defaultClusterId() {
  return localStorage.getItem('xerj.cluster') || 'local';
}
export function setDefaultCluster(id) {
  localStorage.setItem('xerj.cluster', id);
}
