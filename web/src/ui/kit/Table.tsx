import type { ComponentChildren } from 'preact';

export interface TableProps {
  /** put `<thead>` / `<tbody>` (or plain `<tr>`s) inside */
  children?: ComponentChildren;
  /** visible caption above the table (also the accessible name) */
  caption?: ComponentChildren;
  dense?: boolean;
  'data-testid'?: string;
  class?: string;
}

/** Table inside a horizontally scrollable wrapper (the page itself never scrolls sideways on a phone). Numeric cells: `class="right num"`. */
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

/** A full-width row for an empty / loading / error state inside a `<tbody>`. */
export function TableMessage(props: { colSpan: number; children?: ComponentChildren; 'data-testid'?: string }) {
  return (
    <tr>
      <td colSpan={props.colSpan} class="table-empty" data-testid={props['data-testid']}>
        {props.children}
      </td>
    </tr>
  );
}
