// Content-Security-Policy of the production build.
//
// The app is a blind-signing context: wallet popups show no token semantics, so an injected script or a framing page has outsized impact.
// The policy below is embedded as a <meta http-equiv> by the build (vite.config.ts `kobCsp`) and is the SAME policy that `public/_headers` sends
// as a real header, where `frame-ancestors` (which a <meta> cannot express) is added. A test (src/hardening.test.ts) keeps the two in sync.
//
//   * script-src 'self' 'wasm-unsafe-eval': only the bundle; WebAssembly (kob-wasm, the kaspa SDK) may compile, `eval` may not.
//   * require-trusted-types-for 'script' + trusted-types default: no DOM sink (innerHTML, script.src = ...) accepts a plain string. The only
//     policy that may exist is `default`, installed once at boot (src/app/trusted-types.ts): it passes the chart library's constant attribution
//     logo and refuses everything else.
//   * style-src 'self' 'unsafe-inline': the UI sets inline style attributes (Preact `style=`); scripts stay locked down.
//   * connect-src: the node (wRPC over ws:// / wss://, a mock or REST base over http(s)://) and the indexer are USER-CONFIGURABLE (Settings,
//     config.json; the testnet node of the project is a plain ws:// address), so hosts cannot be pinned in the shipped build. The directive
//     therefore only names the schemes. A PRODUCTION DEPLOYMENT SHOULD REPLACE IT with the exact node and indexer origins (README, "Deployment
//     headers"): that is what closes the exfiltration channel, the other directives already stop script injection and framing.
//   * frame-src 'self': the only frame is the app's own private-key signer (signer.html, sandboxed without allow-same-origin).
//   * everything else is denied: no plugins, no base-uri, no form posts, no workers.
//
// The signer page (signer.html) has its own, stricter policy (SIGNER_*): no network at all beyond its own bundle and wasm, no inline styles, no
// frames, and (header only) `sandbox allow-scripts` (an opaque origin even when framed without the sandbox attribute) and `frame-ancestors 'self'`:
// only the app may frame it.
export const CONNECT_SRC = ["'self'", 'https:', 'wss:', 'http:', 'ws:'];

/** Directives that also work in a <meta> element (no `frame-ancestors`, `report-uri`, `sandbox`). */
export const META_DIRECTIVES = {
  'default-src': ["'none'"],
  'script-src': ["'self'", "'wasm-unsafe-eval'"],
  'style-src': ["'self'", "'unsafe-inline'"],
  'img-src': ["'self'", 'data:'],
  'font-src': ["'self'"],
  'connect-src': CONNECT_SRC,
  'object-src': ["'none'"],
  'base-uri': ["'none'"],
  'form-action': ["'none'"],
  'frame-src': ["'self'"],
  'worker-src': ["'none'"],
  'manifest-src': ["'self'"],
  'require-trusted-types-for': ["'script'"],
  'trusted-types': ['default'],
};

/** The private-key signer page: its bundle and wasm from its own origin, nothing else. */
export const SIGNER_META_DIRECTIVES = {
  'default-src': ["'none'"],
  'script-src': ["'self'", "'wasm-unsafe-eval'"],
  'style-src': ["'self'"],
  'connect-src': ["'self'"],
  'img-src': ["'none'"],
  'object-src': ["'none'"],
  'base-uri': ["'none'"],
  'form-action': ["'none'"],
  'frame-src': ["'none'"],
  'worker-src': ["'none'"],
  'require-trusted-types-for': ["'script'"],
  'trusted-types': ["'none'"],
};

const render = (d) => Object.entries(d).map(([k, v]) => `${k} ${v.join(' ')}`).join('; ');

/** Policy for the <meta http-equiv="Content-Security-Policy"> tag. */
export const CSP_META = render(META_DIRECTIVES);
/** Policy for the real HTTP header (`public/_headers`): the same plus `frame-ancestors 'none'`. */
export const CSP_HEADER = `${CSP_META}; frame-ancestors 'none'`;

/** The signer page's <meta> policy. */
export const SIGNER_CSP_META = render(SIGNER_META_DIRECTIVES);
/**
 * The signer page's header policy: only the app's own pages may frame it, and it runs sandboxed (opaque origin, scripts only) however it is
 * loaded: a same-origin page that frames it without the iframe's sandbox attribute still cannot read its DOM (the key field) or its memory.
 */
export const SIGNER_CSP_HEADER = `${SIGNER_CSP_META}; sandbox allow-scripts; frame-ancestors 'self'`;
