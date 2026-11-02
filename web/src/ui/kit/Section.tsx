import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';

export interface SectionProps {
  title?: ComponentChildren;
  /** right side of the header (buttons, badges) */
  actions?: ComponentChildren;
  children?: ComponentChildren;
  /** the body can be folded; the header text becomes the toggle */
  collapsible?: boolean;
  /** initial state of a collapsible section (default open) */
  defaultOpen?: boolean;
  /** shown after the title while a collapsible section is folded (one line, so a folded section stays small) */
  summary?: ComponentChildren;
  'data-testid'?: string;
  class?: string;
}

/** A titled card. Collapsible sections keep the body in the DOM (hidden), so tests and screen-reader search still find it. */
export function Section(props: SectionProps) {
  const [open, setOpen] = useState(props.defaultOpen ?? true);
  const folded = props.collapsible && !open;
  return (
    <section class={`section${folded ? ' section-collapsed' : ''}${props.class ? ` ${props.class}` : ''}`} data-testid={props['data-testid']}>
      {props.title || props.actions ? (
        <div class="section-head">
          {props.collapsible ? (
            <button type="button" class="disclosure" aria-expanded={open ? 'true' : 'false'} onClick={() => setOpen(!open)}>
              <h2>{props.title}</h2>
            </button>
          ) : (
            <h2>{props.title}</h2>
          )}
          {folded && props.summary ? <span class="section-summary">{props.summary}</span> : null}
          {props.actions ? <div class="row">{props.actions}</div> : null}
        </div>
      ) : null}
      <div class="section-body">{props.children}</div>
    </section>
  );
}
