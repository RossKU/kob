import type { ComponentChildren } from 'preact';

export interface KeyValueItem {
  label: ComponentChildren;
  value: ComponentChildren;
  /** put on the value element */
  'data-testid'?: string;
  /** skip the row when false */
  show?: boolean;
}

/** `<dl>` of label / value rows (definition list semantics, two columns on wide screens, stacked on a phone). */
export function KeyValueList(props: { items: KeyValueItem[]; compact?: boolean; 'data-testid'?: string }) {
  return (
    <dl class={`kv${props.compact ? ' kv-compact' : ''}`} data-testid={props['data-testid']}>
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
}
