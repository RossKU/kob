// Trusted Types policy. The CSP (csp.mjs) sets `require-trusted-types-for 'script'` and allows
// only the policy name `default`, once, so a later script cannot create another policy.
//
// The app builds its DOM through Preact and createElement and never hands markup to a DOM sink.
// The one exception is lightweight-charts, which writes its TradingView attribution logo with
// `innerHTML`. The chart license asks the app to show it (PriceChart.tsx `attributionLogo`).
// The `default` policy lets exactly that constant markup through and refuses every other string,
// script and script URL.

/** The logo markup of lightweight-charts 5.2.1. A test compares it with the installed package. */
export const CHART_ATTRIBUTION_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="35" height="19" fill="none"><g fill-rule="evenodd" clip-path="url(#a)" clip-rule="evenodd"><path fill="var(--stroke)" d="M2 0H0v10h6v9h21.4l.5-1.3 6-15 1-2.7H23.7l-.5 1.3-.2.6a5 5 0 0 0-7-.9V0H2Zm20 17h4l5.2-13 .8-2h-7l-1 2.5-.2.5-1.5 3.8-.3.7V17Zm-.8-10a3 3 0 0 0 .7-2.7A3 3 0 1 0 16.8 7h4.4ZM14 7V2H2v6h6v9h4V7h2Z"/><path fill="var(--fill)" d="M14 2H2v6h6v9h6V2Zm12 15h-7l6-15h7l-6 15Zm-7-9a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z"/></g><defs><clipPath id="a"><path fill="var(--stroke)" d="M0 0h35v19H0z"/></clipPath></defs></svg>';

/** What the `default` policy turns into TrustedHTML: the chart logo only. Null refuses the sink. */
export const allowHtml = (s: string): string | null => (s === CHART_ATTRIBUTION_SVG ? s : null);

interface TrustedTypesLike {
  createPolicy(name: string, rules: { createHTML?: (s: string) => string | null }): unknown;
}

/** Installs the `default` policy. Returns false where Trusted Types is missing or already set. */
export function installTrustedTypesPolicy(host: { trustedTypes?: TrustedTypesLike } = globalThis as { trustedTypes?: TrustedTypesLike }): boolean {
  const tt = host.trustedTypes;
  if (!tt?.createPolicy) return false;
  try {
    tt.createPolicy('default', { createHTML: allowHtml });
    return true;
  } catch {
    return false; // already installed (a second boot in the same page)
  }
}
