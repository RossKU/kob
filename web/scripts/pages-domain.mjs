#!/usr/bin/env node
// The custom domain of a Cloudflare Pages site (.github/workflows/deploy-pages.yml, docs/ops/web-hosting.md, section 5): makes sure the host of
// the public URL is attached to the Pages project, and that its DNS record exists, with the API token of the deployment.
//
//   node scripts/pages-domain.mjs --project kob-web --public-url https://kob-tn10.kaspa.ag
//
// Environment: CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID (the secrets of the workflow; never printed).
//
// * attached to the project already: nothing to do.
// * not attached: `POST /accounts/{id}/pages/projects/{project}/domains`. A domain that is attached to another project is NOT moved (a move
//   is an outage until the other project releases it): the step says to remove it there first, in the dashboard, and exits 1.
// * DNS: when the zone is found (the token needs Zone > Read and DNS > Edit; a token limited to Pages > Edit cannot), a missing proxied
//   `CNAME <host> -> <project>.pages.dev` is created. Otherwise the step prints the record to add in the dashboard (exit 0: the site
//   itself is deployed, only the name does not point at it yet).
// Prints markdown (the workflow appends it to the job summary).
import { fileURLToPath } from 'node:url';

const API = 'https://api.cloudflare.com/client/v4';

/** The host of a public URL (`https://kob-tn10.kaspa.ag/` -> `kob-tn10.kaspa.ag`); null when it is not an https origin. */
export function hostOf(publicUrl) {
  try {
    const u = new URL(String(publicUrl).trim());
    return u.protocol === 'https:' && u.hostname ? u.hostname.toLowerCase() : null;
  } catch {
    return null;
  }
}

/** The zone names a host may belong to, longest first (`a.b.kaspa.ag` -> `a.b.kaspa.ag`, `b.kaspa.ag`, `kaspa.ag`). */
export function zoneCandidates(host) {
  const parts = host.split('.');
  const out = [];
  for (let i = 0; i <= parts.length - 2; i++) out.push(parts.slice(i).join('.'));
  return out;
}

async function call(fetchFn, token, method, path, body) {
  const res = await fetchFn(`${API}${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let json = null;
  try {
    json = await res.json();
  } catch {
    /* not JSON */
  }
  return { ok: res.ok && json?.success !== false, status: res.status, json, errors: (json?.errors ?? []).map((e) => `${e.code}: ${e.message}`).join('; ') };
}

/**
 * Attaches `host` to `project` and creates its DNS record when possible. Returns `{ ok, lines }` (`lines`: markdown for the job summary).
 * `fetchFn` is injectable (tests).
 */
export async function ensureDomain({ token, account, project, host, fetchFn = fetch }) {
  const lines = [`### Custom domain \`${host}\` (project \`${project}\`)`, ''];
  const target = `${project}.pages.dev`;
  const base = `/accounts/${account}/pages/projects/${project}/domains`;
  let ok = true;

  const have = await call(fetchFn, token, 'GET', base);
  if (!have.ok) {
    lines.push(`- cannot read the domains of the project (HTTP ${have.status}${have.errors ? `, ${have.errors}` : ''}): the token needs Account > Cloudflare Pages > Edit.`);
    lines.push(`- Dashboard: Workers & Pages > ${project} > Custom domains > Set up a custom domain > \`${host}\`.`);
    return { ok: false, lines };
  }
  const attached = (have.json?.result ?? []).find((d) => String(d.name).toLowerCase() === host);
  if (attached) {
    lines.push(`- attached already (status: ${attached.status ?? 'unknown'}).`);
  } else {
    const add = await call(fetchFn, token, 'POST', base, { name: host });
    if (add.ok) {
      lines.push(`- attached to the project (status: ${add.json?.result?.status ?? 'pending'}).`);
    } else {
      ok = false;
      lines.push(`- could NOT attach it (HTTP ${add.status}${add.errors ? `, ${add.errors}` : ''}).`);
      lines.push(`- If \`${host}\` is attached to another Pages project, remove it there first (Workers & Pages > that project > Custom domains > \`${host}\` > Remove; the site on it is down from then until this run attaches it), then run *Actions* > *deploy-pages* > *Run workflow* again.`);
      return { ok, lines };
    }
  }

  // the DNS record: needs a token that may edit DNS; without it say what to add
  const dashboardDns = `- DNS (dashboard, kaspa.ag zone > DNS > Records): \`CNAME ${host} -> ${target}\`, proxied (orange cloud), unless the record exists already.`;
  let zone = null;
  for (const name of zoneCandidates(host)) {
    const z = await call(fetchFn, token, 'GET', `/zones?name=${encodeURIComponent(name)}&account.id=${account}`);
    if (!z.ok) {
      lines.push(`- DNS: the token cannot look up the zone (HTTP ${z.status}): it is limited to Pages.`, dashboardDns);
      return { ok, lines };
    }
    if (z.json?.result?.length) {
      zone = z.json.result[0];
      break;
    }
  }
  if (!zone) {
    lines.push('- DNS: no zone of this host is in the account.', dashboardDns);
    return { ok, lines };
  }
  const recs = await call(fetchFn, token, 'GET', `/zones/${zone.id}/dns_records?name=${encodeURIComponent(host)}`);
  if (!recs.ok) {
    lines.push(`- DNS: the token cannot read the records of \`${zone.name}\` (HTTP ${recs.status}).`, dashboardDns);
    return { ok, lines };
  }
  const existing = recs.json?.result ?? [];
  if (existing.length) {
    const r = existing[0];
    lines.push(r.type === 'CNAME' && String(r.content).toLowerCase() === target ? `- DNS: \`CNAME ${host} -> ${target}\` exists.` : `- DNS: \`${host}\` already has a ${r.type} record (${r.content}), not ${target}: change it in the dashboard.`);
    if (!(r.type === 'CNAME' && String(r.content).toLowerCase() === target)) ok = false;
    return { ok, lines };
  }
  const made = await call(fetchFn, token, 'POST', `/zones/${zone.id}/dns_records`, { type: 'CNAME', name: host, content: target, proxied: true, ttl: 1 });
  if (made.ok) lines.push(`- DNS: created \`CNAME ${host} -> ${target}\` (proxied).`);
  else lines.push(`- DNS: the token cannot create the record (HTTP ${made.status}${made.errors ? `, ${made.errors}` : ''}).`, dashboardDns);
  return { ok, lines };
}

function arg(name) {
  const i = process.argv.indexOf(name);
  return i >= 0 ? process.argv[i + 1] : undefined;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const project = arg('--project');
  const host = hostOf(arg('--public-url') ?? '');
  const token = process.env.CLOUDFLARE_API_TOKEN;
  const account = process.env.CLOUDFLARE_ACCOUNT_ID;
  if (!project || !host || !token || !account) {
    console.error('usage: CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=... node scripts/pages-domain.mjs --project <name> --public-url https://<host>');
    process.exit(2);
  }
  const { ok, lines } = await ensureDomain({ token, account, project, host });
  console.log(lines.join('\n'));
  process.exit(ok ? 0 : 1);
}
