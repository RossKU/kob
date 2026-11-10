// The Kaspa mark, for KAS as an asset (the Portfolio's Balances row): inline SVG, nothing to fetch (CSP: no external images).
// Source: kaspa.org's own glyph, https://kaspa.org/icon.svg (the site's icon; "Kaspa logo / View logo assets" in its logo menu), fetched 2026-11-10; the same mark
// is in kaspanet/rusty-kaspa (ISC licence) at wasm/examples/web/resources/kaspa.svg. kaspa.org states no licence for the mark itself: it is used here only
// to name the KAS asset, as wallets and explorers do. Paths and colours (#6fc7ba disc, white K) are the file's; the viewBox is cropped to the disc itself (the file's has a margin of about 18% on every side), so the mark fills its box like a token's round avatar; the white disc behind shows through the cut-out K,
// so it reads the same on a dark and a light page.

/** The Kaspa mark at `size` px (default 26, the height of a token's icon in a table line). */
export function KasIcon(props: { size?: number }) {
  const size = props.size ?? 26;
  return (
    <svg class="kas-icon" viewBox="42.4 42.4 112.2 112.2" width={size} height={size} role="img" aria-label="KAS" data-testid="kas-icon">
      <title>KAS</title>
      <circle cx="98.52" cy="98.43" r="41.03" fill="#fff" />
      <path
        fill="#6fc7ba"
        d="m149.42,76.34c-2.8-6.76-7.22-13.03-12.32-18.12-5.08-5.1-11.61-8.91-18.38-11.71-6.51-2.71-13.78-4.07-21.28-4.07s-15.21.27-21.72,2.98c-6.76,2.8-12.42,8.12-17.51,13.22-5.1,5.1-11.02,10.51-13.82,17.26-2.71,6.53-1.83,14.48-1.83,21.99s.55,14.85,3.26,21.38c2.8,6.76,8.52,11.54,13.6,16.64,5.1,5.1,9.71,11.18,16.47,13.98,6.53,2.71,14.04,4.55,21.54,4.55s14.87-2.22,21.4-4.94c6.75-2.8,12.86-7.18,17.96-12.27,5.1-5.1,9.19-11.32,12.01-18.08,2.7-6.51,5.79-13.76,5.79-21.26s-2.48-15-5.19-21.53Zm-36.26,53.49l-11.88-1.75,3.39-23-24.87,19.16-7.32-9.56,21.82-16.8-21.82-16.81,7.32-9.56,24.87,19.16-3.39-22.99,11.88-1.77,4.73,31.98-4.73,31.96Z"
      />
    </svg>
  );
}
