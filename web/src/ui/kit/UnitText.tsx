import type { ComponentChildren } from 'preact';

/**
 * Splits a price unit "KAS per TUSD" at its first " per " into the quote ("KAS", full size) and the "per TUSD" part (smaller, `.unit-per`, styles in styles.css). The words stay
 * in one inline run with the same spaces, so a screen reader reads one phrase and a copied selection is the plain text. A text without " per " is returned as it is; a closing
 * parenthesis after the unit ("Price (KAS per TUSD)") stays outside the small part. `single`: for free-running row values, only when one word follows "per" (a ticker), so
 * "one update per 5 min at most" is left alone.
 */
export function unitParts(text: string, single = false): ComponentChildren {
  const at = text.indexOf(' per ');
  if (text.startsWith('per ') && !single) return <span class="unit-per">{text}</span>;
  if (at <= 0) return text;
  const rest = text.slice(at + 1);
  const close = rest.indexOf(')');
  const per = close < 0 ? rest : rest.slice(0, close);
  const tail = close < 0 ? '' : rest.slice(close);
  if (single && /\s.*\S/.test(per.slice(4))) return text;
  return (
    <>
      {text.slice(0, at)}{' '}<span class="unit-per">{per}</span>{tail}
    </>
  );
}

/** A price unit as inline content: `<UnitText text={t('market.stats.unitOf', ...)} />`. */
export function UnitText(props: { text: string }) {
  return <>{unitParts(props.text)}</>;
}
