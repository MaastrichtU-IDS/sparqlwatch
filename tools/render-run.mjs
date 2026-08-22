// Render a prober run (.nq) as a standalone local page, using the design's
// verdict encoding. This is NOT the web tier (stage 3): it is a read-only
// viewer so a real run can be looked at before that exists.
//
//   node tools/render-run.mjs run.nq out.html
//
// The N-Quads the prober emits are line-based and already validated, so a
// line regex is enough here; the real web tier queries Oxigraph instead.
import { readFileSync, writeFileSync } from 'node:fs';

const DQV = 'http://www.w3.org/ns/dqv#';
const PROV = 'http://www.w3.org/ns/prov#';
const SW = 'urn:sparqlwatch:';

const QUAD = /^(\S+)\s+<([^>]+)>\s+(.+?)\s+<([^>]+)>\s*\.$/;

function parse(nq) {
  const quads = [];
  for (const line of nq.split('\n')) {
    const m = QUAD.exec(line.trim());
    if (m) quads.push({ s: m[1], p: m[2], o: m[3], g: m[4] });
  }
  return quads;
}

const iri = (t) => t.replace(/^<|>$/g, '');
const lit = (t) => {
  const m = /^"((?:[^"\\]|\\.)*)"/.exec(t);
  return m ? m[1] : iri(t);
};

// Verdict presentation. Mirrors the design: colour never carries the meaning
// alone. Dashed borders mark "works but undeclared" and "indeterminate", and
// `absent` has no border at all so it reads as empty rather than as another grey.
// Verdict presentation, on THREE channels that are all independent of colour:
// border style, whether the chip is filled, and border weight. Colour still
// carries meaning for a reader who can see it, but never carries it alone.
//
// This replaced an encoding with two collisions. Border style alone had solid
// covering `verified`, `declared-only` and `declared-but-wrong`, and dashed
// covering `undeclared-but-verified` and `indeterminate`. So a reader who cannot
// separate green from amber could not tell "works, just undeclared" from "we
// never found out", and, worse, a reader who cannot separate grey from red could
// not tell `declared-only` (neutral) from `declared-but-wrong` (the worst verdict
// in the vocabulary). Seven states need more than four border styles.
//
// The channels carry meaning rather than being arbitrary:
//   filled        = we have positive evidence the capability works
//   dashed        = no declaration was seen for it
//   2px           = something is actively wrong, not merely missing
//   no border     = nothing was there
//   dotted        = we did not look
const VERDICT = {
  'verified':                { label: 'verified',            token: 'good',  style: 'solid',  fill: true,  weight: 1 },
  'undeclared-but-verified': { label: 'works, not declared', token: 'good',  style: 'dashed', fill: true,  weight: 1 },
  'declared-only':           { label: 'declared only',       token: 'muted', style: 'solid',  fill: false, weight: 1 },
  'declared-but-wrong':      { label: 'declared but wrong',  token: 'crit',  style: 'solid',  fill: true,  weight: 2 },
  'indeterminate':           { label: 'indeterminate',       token: 'warn',  style: 'dashed', fill: false, weight: 1 },
  'absent':                  { label: 'absent',              token: 'dim',   style: 'none',   fill: false, weight: 1 },
  // Not a verdict. A declined metric was never measured, so it has no verdict at
  // all; this exists so the viewer shows that state rather than an empty gap
  // that reads as "this metric does not exist".
  'not-measured':            { label: 'not measured',        token: 'dim',   style: 'dotted', fill: false, weight: 1 },
};

// One place builds a chip's border and fill, so the table and the legend cannot
// drift apart. They did not share this before, which is how a `dotted` style
// silently rendered as solid in one of them.
function chipStyle(v) {
  const border = v.style === 'none'
    ? `${v.weight}px solid transparent`
    : `${v.weight}px ${v.style} var(--${v.token})`;
  const fill = v.fill ? 'background: var(--chip-fill);' : '';
  return `border: ${border}; ${fill}`;
}

const ABBR = {
  availability: 'A', cors: 'C', 'service-description': 'S',
  'geo-functions': 'G', 'geo-data': 'D', classes: 'K', 'cors-preflight': 'P',
  'has-classes': 'T',
};

// The metrics this viewer knows about, in a fixed display order. A run can
// carry a metric not in this list (an older or newer prober version, or a
// metrics.toml this script has not been told about); `metricsIn` below still
// renders it, with a fallback abbreviation and label, rather than dropping it
// from the page. A viewer that hides measurements is worse than one that
// looks untidy.
const KNOWN_METRICS = [
  'availability', 'cors', 'cors-preflight', 'service-description', 'geo-functions', 'geo-data',
  'has-classes', 'classes',
];

// Every metric actually present in `rows`: the known ones first, in the fixed
// order above, then anything unrecognised, alphabetically, so a run is never
// silently under-reported just because this script predates its metric.
function metricsIn(rows, declined = []) {
  // Declined metrics count as present. A metric declined on every endpoint has
  // no measurement row anywhere, so taking the column set from `rows` alone
  // would drop it from the page entirely, which reads as "this metric does not
  // exist" rather than "we chose not to run it".
  const present = new Set([...rows.map((r) => r.metric), ...declined.map((d) => d.metric)]);
  const known = KNOWN_METRICS.filter((m) => present.has(m));
  const unknown = [...present].filter((m) => !KNOWN_METRICS.includes(m)).sort();
  return [...known, ...unknown];
}

function collect(quads) {
  const m = new Map();
  const row = (s) => { if (!m.has(s)) m.set(s, {}); return m.get(s); };
  let run = { at: '', version: '', revision: '' };
  for (const q of quads) {
    if (q.p === `${DQV}computedOn`) row(q.s).endpoint = iri(q.o);
    else if (q.p === `${DQV}isMeasurementOf`) row(q.s).metric = iri(q.o).replace(`${SW}metric:`, '');
    else if (q.p === `${DQV}value`) row(q.s).verdict = lit(q.o);
    else if (q.p === `${SW}elapsedMs`) row(q.s).ms = Number(lit(q.o));
    else if (q.p === `${PROV}generatedAtTime`) run.at = lit(q.o);
    else if (q.p === `${SW}proberVersion`) run.version = lit(q.o);
    else if (q.p === `${SW}metricDefinitionRevision`) run.revision = lit(q.o);
    // A not-measured fact carries sparqlwatch-owned predicates only: reusing
    // `dqv:computedOn` would entail that it IS a quality measurement, which is
    // exactly what it is not. So it needs its own arms here.
    else if (q.p === `${SW}notMeasuredOn`) row(q.s).nmEndpoint = iri(q.o);
    else if (q.p === `${SW}notMeasuredMetric`) row(q.s).nmMetric = iri(q.o).replace(`${SW}metric:`, '');
    else if (q.p === `${SW}notMeasuredReason`) row(q.s).nmReason = lit(q.o);
  }
  const all = [...m.values()];
  const rows = all.filter((r) => r.endpoint && r.metric && r.verdict);
  const declined = all
    .filter((r) => r.nmEndpoint && r.nmMetric)
    .map((r) => ({ endpoint: r.nmEndpoint, metric: r.nmMetric, reason: r.nmReason }));
  return { run, rows, declined };
}

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

function chip(metric, verdict) {
  const v = VERDICT[verdict] ?? { label: verdict, token: 'dim', style: 'none', fill: false, weight: 1 };
  return `<span class="chip" title="${esc(metric)}: ${esc(v.label)}"
    style="${chipStyle(v)} color: var(--${v.token})">${esc(ABBR[metric] ?? metric[0].toUpperCase())}</span>`;
}

function render({ run, rows, declined = [] }) {
  const endpoints = [...new Set([...rows.map((r) => r.endpoint), ...declined.map((d) => d.endpoint)])].sort();
  const metrics = metricsIn(rows, declined);
  const wasDeclined = (ep, m) => declined.some((d) => d.endpoint === ep && d.metric === m);

  const counts = {};
  for (const r of rows) counts[r.verdict] = (counts[r.verdict] ?? 0) + 1;
  counts['not-measured'] = declined.length;

  const body = endpoints.map((ep) => {
    const mine = metrics.map((m) => rows.find((r) => r.endpoint === ep && r.metric === m));
    const times = mine.filter((r) => r && r.ms !== undefined).map((r) => r.ms);
    const slowest = times.length ? Math.max(...times) : null;
    // Named for what it counts. "not measured" now has a specific published
    // meaning (a metric declined by the cost ceiling), so it cannot also mean
    // "measured, but reported no elapsed time".
    const untimed = mine.filter((r) => r && r.ms === undefined).length;
    return `<tr>
      <td><div class="name">${esc(ep.replace(/^https?:\/\//, ''))}</div></td>
      <td><div class="chips">${mine.map((r, i) => r ? chip(metrics[i], r.verdict)
        : wasDeclined(ep, metrics[i]) ? chip(metrics[i], 'not-measured')
        : '<span class="chip" style="border:1px solid transparent"></span>').join('')}</div></td>
      <td class="num">${slowest === null ? '&mdash;' : slowest + ' ms'}</td>
      <td class="num dim">${untimed ? untimed + ' untimed' : ''}</td>
    </tr>`;
  }).join('\n');

  // Built through the same `chipStyle` the chips use, so a swatch always looks
  // like the thing it explains. It did not before: the legend gave `absent` a
  // fill the chips never had, so the one verdict that claims a negative was
  // explained by a swatch that did not match it.
  const legend = Object.entries(VERDICT).map(([k, v]) =>
    `<span class="leg"><i style="${chipStyle(v)}"></i>${esc(v.label)} <b>${counts[k] ?? 0}</b></span>`
  ).join('');

  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>sparqlwatch run ${esc(run.at)}</title>
<style>
  /* blue dark (blueprint), the default theme, from ontoexplorer's tokens */
  :root {
    --bg:#0a1929; --bg2:#112a3f; --border:#2a5580; --text:#d0e4f5; --bright:#fff;
    --muted:#7fa5c8; --dim:#5c7c9c; --accent:#4fc3f7; --good:#66bb6a; --warn:#ffb74d;
    --crit:#ef5350; --overlay:rgba(255,255,255,.05); --mono:ui-monospace,'Cascadia Code',monospace;
    /* A chip is 26x22px, where --overlay's 5% white is invisible. Desaturating
       the page showed filled and empty chips reading identically, which defeats
       the one channel separating "works" from "we never found out". 16% is the
       point at which the difference survives greyscale without the fill
       competing with the border for attention. */
    --chip-fill:rgba(255,255,255,.16);
    color-scheme: dark;
  }
  * { box-sizing: border-box; }
  body { margin:0; background:var(--bg); color:var(--text); font:14px/1.5 system-ui,-apple-system,sans-serif; }
  header { border-bottom:1px solid var(--border); background:var(--bg2); padding:14px 24px; display:flex; align-items:center; gap:12px; }
  .logo { display:flex; align-items:center; gap:9px; font-weight:600; color:var(--bright); }
  .meta { margin-left:auto; font-size:12px; color:var(--dim); font-family:var(--mono); }
  main { padding:22px 24px; max-width:1120px; }
  h1 { font-size:19px; font-weight:600; color:var(--bright); margin:0 0 4px; letter-spacing:-.01em; }
  .sub { color:var(--muted); font-size:13px; margin-bottom:20px; }
  table { width:100%; border-collapse:collapse; }
  th { text-align:left; font-size:11px; font-weight:600; color:var(--dim); text-transform:uppercase;
       letter-spacing:.05em; padding:0 12px 8px; border-bottom:1px solid var(--border); }
  td { padding:13px 12px; border-bottom:1px solid var(--border); vertical-align:middle; }
  .name { font-family:var(--mono); font-size:12.5px; color:var(--bright); }
  .chips { display:flex; gap:5px; }
  .chip { display:inline-flex; align-items:center; justify-content:center; width:26px; height:22px;
          border-radius:4px; font:700 10px var(--mono); }
  .num { text-align:right; font-variant-numeric:tabular-nums; color:var(--muted); white-space:nowrap; }
  .dim { color:var(--dim); font-size:12px; }
  .panel { margin-top:18px; border:1px solid var(--border); border-radius:6px; background:var(--bg2); padding:14px 16px; }
  .panel h2 { font-size:11px; font-weight:600; color:var(--dim); text-transform:uppercase; letter-spacing:.05em; margin:0 0 11px; }
  .leg { display:inline-flex; align-items:center; gap:7px; margin:0 16px 6px 0; font-size:12px; color:var(--muted); }
  .leg i { width:20px; height:15px; border-radius:3px; display:inline-block; }
  .leg b { color:var(--bright); font-variant-numeric:tabular-nums; }
  .note { margin-top:11px; font-size:12px; color:var(--muted); max-width:780px; }
</style></head><body>
<header>
  <div class="logo">
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="var(--accent)" stroke-width="2" stroke-linecap="round">
      <circle cx="11" cy="11" r="6"/><path d="M15.5 15.5 21 21"/><path d="M8 11h6"/></svg>
    sparqlwatch
  </div>
  <div class="meta">run ${esc(run.at)} &middot; prober ${esc(run.version)} &middot; ${esc(run.revision)}</div>
</header>
<main>
  <h1>Endpoint conformance</h1>
  <div class="sub">${rows.length} measurements across ${endpoints.length} endpoints, from one probe sweep. No score and no ranking: each attribute carries its own verdict.</div>
  <table>
    <thead><tr><th>Endpoint</th><th>Conformance</th><th style="text-align:right">Slowest probe</th><th></th></tr></thead>
    <tbody>${body}</tbody>
  </table>
  <div class="panel">
    <h2>Verdicts in this run</h2>
    ${legend}
    <div class="note">Chips are A availability, C CORS, P CORS preflight, S service description, G GeoSPARQL functions, D geometry data, K classes.
    A metric this page does not recognise still renders, labelled by its own id with its first letter as the chip.
    The encoding never rests on colour. A filled chip means we have positive evidence the capability works; a dashed border means no
    declaration was seen for it; a heavier border means something is actively wrong rather than merely missing; a dotted border means we did not look;
    and no border at all means nothing was there. Absent is the only verdict that claims a negative, and it is claimed only where a parsed answer
    established one.</div>
  </div>
</main></body></html>`;
}

const [, , inPath, outPath = 'run.html'] = process.argv;
if (!inPath) { console.error('usage: node tools/render-run.mjs <run.nq> [out.html]'); process.exit(2); }
const data = collect(parse(readFileSync(inPath, 'utf8')));
if (!data.rows.length) { console.error(`no measurements found in ${inPath}`); process.exit(1); }
writeFileSync(outPath, render(data));
console.log(`wrote ${outPath}, ${data.rows.length} measurements, ${new Set(data.rows.map(r => r.endpoint)).size} endpoints`);
