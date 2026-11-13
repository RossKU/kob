// Machine translation of the page, such as Chrome's built-in Google Translate, replaces each text
// node it translates with `<font>` elements of its own and keeps the original node detached.
// Preact keeps updating the original, so a text that changes in place stays on screen as the
// translation of the value it had when the page was translated. Examples are a status bar stuck
// at "checking..." beside a green dot, and a send status that never says "accepted". A new text
// inserted before a replaced one even throws (`insertBefore`: NotFoundError).
//
// While the page is translated, the translator marks <html> with `translated-ltr` or
// `translated-rtl`. Then every non-interactive element whose children are only text is keyed by
// that text. A change re-creates the element, so the translator's <font> leaves with the old one
// and the new one is translated afresh. An untranslated page is not touched.
//
// A text that changes must therefore be the only content of its element, as in
// `<span>{text}</span>`, never `{label}: {value}<b>...</b>` mixed with elements.
import { options } from 'preact';

/** Elements that are never re-created because they hold focus, selection or form state. */
const KEEP = new Set(['button', 'a', 'input', 'select', 'option', 'optgroup', 'textarea', 'label', 'summary', 'details', 'iframe', 'script', 'style', 'title']);

const isText = (c: unknown): boolean => c == null || typeof c === 'boolean' || typeof c === 'string' || typeof c === 'number' || typeof c === 'bigint';

/**
 * The joined text of children that render as text only, with null and boolean holes skipped.
 * Null when a child is not text or no piece is left.
 */
export function textOnly(children: unknown): string | null {
  const parts: unknown[] = Array.isArray(children) ? children.flat(Infinity) : [children];
  if (!parts.every(isText)) return null;
  const s = parts.filter((c) => c != null && typeof c !== 'boolean').join('');
  return s === '' ? null : s;
}

/** The key that re-creates a text-only element when its text changes, or null to leave it. */
export function translationKey(v: { type: unknown; key?: unknown; ref?: unknown; props: Record<string, unknown> }): string | null {
  if (typeof v.type !== 'string' || v.key != null || v.ref != null || KEEP.has(v.type)) return null;
  const p = v.props;
  if (p.translate === false || p.translate === 'no' || p.tabIndex != null || p.contentEditable != null || p.dangerouslySetInnerHTML != null) return null;
  const text = textOnly(p.children);
  return text === null ? null : `\u0000t:${text}`;
}

const TRANSLATED = /\btranslated-(ltr|rtl)\b/;

/** Watches <html> for the translator's mark and keys text-only elements by their text meanwhile. */
export function installTranslationSafety(doc: Document = document): void {
  const root = doc.documentElement;
  let translated = TRANSLATED.test(root.className);
  new MutationObserver(() => {
    translated = TRANSLATED.test(root.className);
  }).observe(root, { attributes: true, attributeFilter: ['class'] });
  const prev = options.vnode;
  options.vnode = (v) => {
    if (translated) {
      const k = translationKey(v as Parameters<typeof translationKey>[0]);
      if (k !== null) (v as { key?: unknown }).key = k;
    }
    prev?.(v);
  };
}
