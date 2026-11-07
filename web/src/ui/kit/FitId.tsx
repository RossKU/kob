import { shortId } from './format';

/**
 * How an id gives way, longest first, each with its middle taken out:
 *  - `covenant`: 12 + 12 hex is what the market list shows (96 bits, audit 2026-10-29 F4), 8 + 8 the floor of a token's id (`longId`), for a line too narrow for that;
 *  - `log`: a transaction or an address in a column of the activity log; the link and the title have it whole, so 4 + 4 serves a very narrow column.
 */
export const FIT_FORMS = { covenant: [12, 8], log: [8, 4] } as const;

/** The forms of `value`, in the order `FitId` puts them. */
export const fitForms = (value: string, kind: keyof typeof FIT_FORMS): [string, string, string] => {
  const [long, short] = FIT_FORMS[kind];
  return [value, shortId(value, long, long), shortId(value, short, short)];
};

/** A selection inside the id copies the whole id, whichever form is on screen; a selection that goes past it is left alone. */
function copyWhole(e: ClipboardEvent, value: string): void {
  const box = e.currentTarget as HTMLElement;
  const sel = getSelection();
  if (!sel || !box.contains(sel.anchorNode) || !box.contains(sel.focusNode)) return;
  e.clipboardData?.setData('text/plain', value);
  e.preventDefault();
}

/**
 * An id as long as the room allows: whole where it fits, else with its middle taken out (`abcdef01…12345678`). All the forms are in the page and the stylesheet shows the one
 * that fits (styles.css `.fit-id`): nothing is measured, and nothing moves when the room changes. `room` decides what is asked: `box` (default) the id's own box, which takes its
 * width from where it stands (a column), `line` the `.fit-line` around it, for an id that takes only the width of its text. The whole id is the title and `data-value`.
 */
export function FitId(props: { value: string; kind: keyof typeof FIT_FORMS; room?: 'box' | 'line'; as?: 'span' | 'code'; class?: string; 'data-testid'?: string }) {
  const Tag = props.as ?? 'span';
  const room = props.room ?? 'box';
  return (
    <Tag
      class={`fit-id fit-${props.kind} fit-in-${room}${props.class ? ` ${props.class}` : ''}`}
      title={props.value}
      data-value={props.value}
      data-testid={props['data-testid']}
      onCopy={(e) => copyWhole(e, props.value)}
    >
      {fitForms(props.value, props.kind).map((form, i) => <span key={i}>{form}</span>)}
    </Tag>
  );
}
