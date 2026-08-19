// Render .dc.html artboards to standalone static HTML for local PDF export.
// The published canvas renders artboards through its own runtime, which does not
// resolve from file://, so this reimplements just enough of the template format:
// {{dotted.holes}}, <sc-for list as>, <sc-if value>. Events are stripped.
import { readFileSync, writeFileSync } from 'node:fs';

const readFile = (p) => readFileSync(p, 'utf8');

/** Find the index just past the matching close tag for an opening tag at `from`. */
function matchTag(src, tag, openEnd) {
  const open = new RegExp(`<${tag}\\b`, 'g');
  const close = new RegExp(`</${tag}\\s*>`, 'g');
  let depth = 1, i = openEnd;
  for (;;) {
    open.lastIndex = i; close.lastIndex = i;
    const o = open.exec(src), c = close.exec(src);
    if (!c) throw new Error(`unclosed <${tag}>`);
    if (o && o.index < c.index) { depth++; i = o.index + 1; continue; }
    depth--;
    // NB: the body always starts at openEnd — `i` has advanced past it when the
    // tag was nested, and using it here truncates the body mid-markup.
    if (depth === 0) return { inner: [openEnd, c.index], end: c.index + c[0].length };
    i = c.index + 1;
  }
}

const lookup = (scope, path) => {
  if (path === 'true') return true;
  if (path === 'false') return false;
  return path.split('.').reduce((v, k) => (v == null ? undefined : v[k]), scope);
};

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

function render(src, scope) {
  let out = '';
  let i = 0;
  for (;;) {
    const nextFor = src.indexOf('<sc-for', i);
    const nextIf = src.indexOf('<sc-if', i);
    const next = [nextFor, nextIf].filter((n) => n !== -1).sort((a, b) => a - b)[0];
    if (next === undefined) { out += subst(src.slice(i), scope); break; }
    out += subst(src.slice(i, next), scope);

    const isFor = next === nextFor;
    const tag = isFor ? 'sc-for' : 'sc-if';
    const openEnd = src.indexOf('>', next) + 1;
    const attrs = src.slice(next, openEnd);
    const { inner, end } = matchTag(src, tag, openEnd);
    const body = src.slice(inner[0], inner[1]);

    if (isFor) {
      const listPath = /list="\{\{\s*([^}\s]+)\s*\}\}"/.exec(attrs)?.[1];
      const alias = /\bas="([^"]+)"/.exec(attrs)?.[1] ?? 'item';
      const list = lookup(scope, listPath) ?? [];
      list.forEach((item, idx) => {
        out += render(body, { ...scope, [alias]: item, $index: idx });
      });
    } else {
      const valPath = /value="\{\{\s*([^}\s]+)\s*\}\}"/.exec(attrs)?.[1];
      if (lookup(scope, valPath)) out += render(body, scope);
    }
    i = end;
  }
  return out;
}

/** Substitute holes, drop event handlers and placeholder hints. */
function subst(chunk, scope) {
  return chunk
    .replace(/\s+on[A-Z][A-Za-z]*="\{\{[^}]*\}\}"/g, '')
    .replace(/\s+hint-[a-z-]+="[^"]*"/g, '')
    .replace(/\{\{\s*([^}\s]+)\s*\}\}/g, (_, p) => {
      const v = lookup(scope, p);
      return v === undefined || v === null ? '' : esc(v);
    });
}

/** Build the component instance and get its render values. */
function evaluate(file, propOverrides) {
  const src = readFile(file);
  const tmpl = /<x-dc>([\s\S]*?)<\/x-dc>/.exec(src)?.[1];
  if (!tmpl) throw new Error(`no <x-dc> in ${file}`);
  const style = /<helmet>\s*<style>([\s\S]*?)<\/style>\s*<\/helmet>/.exec(src)?.[1] ?? '';
  const scriptTag = /<script data-dc-script data-props='([\s\S]*?)'>([\s\S]*?)<\/script>/.exec(src);
  if (!scriptTag) throw new Error(`no data-dc-script in ${file}`);
  const propsSpec = JSON.parse(scriptTag[1].replace(/&amp;/g, '&').replace(/&#39;/g, "'"));
  const code = scriptTag[2];

  const defaults = {};
  for (const [k, v] of Object.entries(propsSpec)) if (k !== '$preview') defaults[k] = v.default;
  const props = { ...defaults, ...propOverrides };

  class DCLogic {
    constructor(p) { this.props = p; this.state = {}; }
    setState(patch) { Object.assign(this.state, patch); }
    forceUpdate() {}
  }
  const Component = new Function('DCLogic', `${code}; return Component;`)(DCLogic);
  const inst = new Component(props);
  inst.props = props;
  // strip the helmet block out of the template body
  const body = tmpl.replace(/<helmet>[\s\S]*?<\/helmet>/, '');
  return { style, body, vals: inst.renderVals() };
}

const PAGES = [
  { file: 'Main.dc.html',     label: 'Endpoint directory',        width: 1280 },
  { file: 'Endpoint.dc.html', label: 'Endpoint detail: findings', width: 1120, props: { tab: 'Findings' } },
  { file: 'Endpoint.dc.html', label: 'Endpoint detail: content',  width: 1120, props: { tab: 'Content' } },
  { file: 'Endpoint.dc.html', label: 'Endpoint detail: query',    width: 1120, props: { tab: 'Query' } },
  { file: 'Endpoint.dc.html', label: 'Endpoint detail: history',  width: 1120, props: { tab: 'History' } },
  { file: 'Examples.dc.html', label: 'Example catalog',           width: 1120 },
  { file: 'Fleet.dc.html',    label: 'Fleet charts',              width: 1120 }
];

const sections = PAGES.map(({ file, label, width, props }) => {
  const { style, body, vals } = evaluate(file, props ?? {});
  return { label, width, style, html: render(body, vals) };
});

const PAGE_W = 1376, PAGE_H = 1340;
const doc = `<!doctype html>
<html><head><meta charset="utf-8"><title>sparqlwatch UI</title>
<style>
  @page { size: ${PAGE_W}px ${PAGE_H}px; margin: 0; }
  html, body { margin: 0; padding: 0; background: #ffffff; }
  .page { width: ${PAGE_W}px; height: ${PAGE_H}px; box-sizing: border-box;
          break-after: page; padding: 34px 48px 48px; overflow: hidden; }
  .page:last-child { break-after: auto; }
  .caption { font-family: system-ui, -apple-system, sans-serif; font-size: 13px; font-weight: 600;
             color: #57606a; text-transform: uppercase; letter-spacing: 0.06em; margin-bottom: 14px; }
  .frame { border: 1px solid #d0d7de; border-radius: 8px; overflow: hidden; width: max-content; }
${sections.map((s) => s.style).join('\n')}
</style></head><body>
${sections.map((s) => `<div class="page"><div class="caption">${esc(s.label)}</div><div class="frame">${s.html}</div></div>`).join('\n')}
</body></html>`;

writeFileSync(process.argv[2] ?? 'sparqlwatch-ui-print.html', doc);
console.log(`wrote ${process.argv[2]} — ${sections.length} pages`);
