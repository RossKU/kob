// A QR code as one SVG path (pure, tested in qr.test.ts). The modules come from qrcode-generator; the SVG is drawn by SupportPage.tsx as elements, so no
// markup string is ever parsed (CSP and Trusted Types as everywhere else in the app).
import qrcode from 'qrcode-generator';

/** The light margin the standard asks for, in modules: wallets need it to find the code on a dark page. */
export const QR_QUIET_ZONE = 4;

export interface QrPath {
  /** width and height in modules, the quiet zone included */
  size: number;
  /** the dark modules, in module units */
  path: string;
}

/** `text` as a QR code of error correction level M, the version as small as it fits. */
export function qrPath(text: string): QrPath {
  const qr = qrcode(0, 'M');
  qr.addData(text, 'Byte');
  qr.make();
  const n = qr.getModuleCount();
  let path = '';
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      if (!qr.isDark(y, x)) continue;
      const start = x;
      while (x + 1 < n && qr.isDark(y, x + 1)) x++;
      path += `M${start + QR_QUIET_ZONE} ${y + QR_QUIET_ZONE}h${x - start + 1}v1h-${x - start + 1}z`;
    }
  }
  return { size: n + 2 * QR_QUIET_ZONE, path };
}
