// Build-time additions to the head of index.html (vite.config.ts `kobHead`), driven by two environment variables:
//
//   KOB_NETWORK      `mainnet` (default) or `testnet-10`: a testnet build prefixes its title (and og:title) with `Testnet · `, so a testnet page is
//                    never mistaken for the real one in a tab, a bookmark or a link preview.
//   KOB_PUBLIC_URL   the public origin the build is served from (`https://kob.kaspa.ag`): og:url and og:image (and twitter:image) need ABSOLUTE urls,
//                    so they are emitted only when it is given. Unset: both are omitted, and the output does not depend on any host.
//
//   landing-only     (an option of applyHead, from KOB_LANDING_ONLY): the description and the link-preview text say the app is in preparation.
//
// The rest of the head (title, description, og:title, og:description, twitter:card, icons, manifest) is static in index.html.
export const TESTNET_TITLE_PREFIX = 'Testnet · ';

/** The configured network name; throws on anything but the two the app knows. */
export function parseBuildNetwork(raw) {
  const v = (raw ?? '').trim();
  if (v === '' || v === 'mainnet') return 'mainnet';
  if (v === 'testnet-10') return v;
  throw new Error(`KOB_NETWORK must be mainnet or testnet-10, got "${v}"`);
}

/** The origin of `KOB_PUBLIC_URL` without a trailing slash, or null when it is unset; throws when it is not an http(s) origin. */
export function parsePublicOrigin(raw) {
  const v = (raw ?? '').trim();
  if (v === '') return null;
  let u;
  try {
    u = new URL(v);
  } catch {
    throw new Error(`KOB_PUBLIC_URL must be an absolute http(s) URL, got "${v}"`);
  }
  if ((u.protocol !== 'https:' && u.protocol !== 'http:') || u.username || u.password || u.search || u.hash || (u.pathname !== '/' && u.pathname !== '')) {
    throw new Error(`KOB_PUBLIC_URL must be an http(s) origin without path, query or credentials, got "${v}"`);
  }
  return u.origin;
}

const esc = (s) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;');

/** The description of a landing-only build (the mainnet front page): it does not offer an app that is not there. */
export const LANDING_ONLY_DESCRIPTION = 'Kaspa Order Book: decentralized limit orders for Kaspa tokens. No admin keys, no oracle. Mainnet is in preparation; the testnet app is open. Open source.';

/** `index.html` with the build's network and public origin applied (and, landing-only, the "in preparation" description). */
export function applyHead(html, { network, publicUrl, landingOnly = false }) {
  const net = parseBuildNetwork(network);
  const origin = parsePublicOrigin(publicUrl);
  let out = html;
  if (landingOnly) {
    out = out.replace(/(<meta (?:name|property)="(?:description|og:description|twitter:description)" content=")[^"]*(")/g, (_m, a, b) => `${a}${esc(LANDING_ONLY_DESCRIPTION)}${b}`);
  }
  if (net !== 'mainnet') {
    out = out
      .replace(/<title>([^<]*)<\/title>/, (_m, t) => `<title>${TESTNET_TITLE_PREFIX}${t}</title>`)
      .replace(/(<meta property="og:title" content=")([^"]*)(")/, (_m, a, t, b) => `${a}${TESTNET_TITLE_PREFIX}${t}${b}`)
      .replace(/(<meta name="twitter:title" content=")([^"]*)(")/, (_m, a, t, b) => `${a}${TESTNET_TITLE_PREFIX}${t}${b}`);
  }
  if (origin) {
    const image = `${origin}/og.png`;
    const tags = [
      `<meta property="og:url" content="${esc(origin + '/')}" />`,
      `<meta property="og:image" content="${esc(image)}" />`,
      `<meta property="og:image:width" content="1200" />`,
      `<meta property="og:image:height" content="630" />`,
      `<meta property="og:image:alt" content="KOB: bars of a Kaspa limit order book, asks above and bids below" />`,
      `<meta name="twitter:image" content="${esc(image)}" />`,
    ];
    out = out.replace(/[ \t]*<\/head>/, `${tags.map((t) => `    ${t}\n`).join('')}  </head>`);
  }
  return out;
}
