import type { ComponentChildren } from 'preact';
import { useState } from 'preact/hooks';

export interface SectionProps {
  title?: ComponentChildren;
  /**
   * A small muted line under the title. The header stays one row, with the title and its subtitle
   * at the left and the actions at the right.
   */
  subtitle?: ComponentChildren;
  /** The right side of the header (buttons, badges). */
  actions?: ComponentChildren;
  /** A "?" right after the title (kit/HelpTip.tsx). */
  help?: ComponentChildren;
  children?: ComponentChildren;
  /** The body can be folded, and the header text becomes the toggle. */
  collapsible?: boolean;
  /** The initial state of a collapsible section (default open). */
  defaultOpen?: boolean;
  /**
   * Shown after the title while a collapsible section is folded. It is one line, so a folded
   * section stays small.
   */
  summary?: ComponentChildren;
  'data-testid'?: string;
  class?: string;
}

/**
 * A titled card. Collapsible sections keep the body in the DOM (hidden), so tests and screen-reader
 * search still find it.
 */
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
          ) : props.help ? (
            <div class="help-title">
              <h2>{props.title}</h2>
              {props.help}
            </div>
          ) : props.subtitle ? (
            <div class="section-titles">
              <h2>{props.title}</h2>
              <p class="section-sub small muted">{props.subtitle}</p>
            </div>
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
