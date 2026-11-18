import type { ComponentChildren } from 'preact';

export interface TableProps {
  /** Put `<thead>` and `<tbody>`, or plain `<tr>`s, inside. */
  children?: ComponentChildren;
  /** A visible caption above the table, which is also the accessible name. */
  caption?: ComponentChildren;
  dense?: boolean;
  'data-testid'?: string;
  class?: string;
}

/**
 * Table inside a horizontally scrollable wrapper, so the page itself never scrolls sideways on a
 * phone. Numeric cells use `class="right num"`.
 */
export function Table(props: TableProps) {
  return (
    <div class={`table-wrap${props.class ? ` ${props.class}` : ''}`}>
      <table class={`table${props.dense ? ' table-dense' : ''}`} data-testid={props['data-testid']}>
        {props.caption ? <caption>{props.caption}</caption> : null}
        {props.children}
      </table>
    </div>
  );
}

/** A full-width row for an empty, loading or error state inside a `<tbody>`. */
export function TableMessage(props: { colSpan: number; children?: ComponentChildren; 'data-testid'?: string }) {
  return (
    <tr>
      <td colSpan={props.colSpan} class="table-empty" data-testid={props['data-testid']}>
        {props.children}
      </td>
    </tr>
  );
}
