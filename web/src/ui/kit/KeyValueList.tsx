import type { ComponentChildren } from 'preact';

export interface KeyValueItem {
  label: ComponentChildren;
  value: ComponentChildren;
  /** put on the value element */
  'data-testid'?: string;
  /** skip the row when false */
  show?: boolean;
}

/**
 * `<dl>` of label / value rows (definition list semantics, two columns on wide screens, stacked on a phone). `flow`: on a wide card the rows run in two or
 * three label / value groups side by side (a container query on the card), so a short list does not leave the right of the card empty.
 */
export function KeyValueList(props: { items: KeyValueItem[]; compact?: boolean; flow?: boolean; 'data-testid'?: string }) {
  const list = (
    <dl class={`kv${props.compact ? ' kv-compact' : ''}${props.flow ? ' kv-flow' : ''}`} data-testid={props['data-testid']}>
      {props.items
        .filter((i) => i.show !== false)
        .map((i, n) => (
          <>
            <dt key={`t${n}`}>{i.label}</dt>
            <dd key={`d${n}`} data-testid={i['data-testid']}>{i.value}</dd>
          </>
        ))}
    </dl>
  );
  return props.flow ? <div class="kv-box">{list}</div> : list;
}
