import type { ComponentChildren } from 'preact';
import { useEffect, useRef } from 'preact/hooks';

export interface TabItem<Id extends string = string> {
  id: Id;
  label: ComponentChildren;
  'data-testid'?: string;
  /** the accessible name where the label is shortened */
  'aria-label'?: string;
}

/** Tab strip (`role=tablist`). The caller renders the panel of the active tab (give it `role="tabpanel"`). Arrow keys move between tabs. */
export function Tabs<Id extends string = string>(props: { tabs: TabItem<Id>[]; active: Id; onChange: (id: Id) => void; 'aria-label': string }) {
  const strip = useRef<HTMLDivElement>(null);
  // a strip that scrolls sideways (the tabs do not fit one line): the active tab is brought into view, inside the strip only (the page does not move)
  useEffect(() => {
    const el = strip.current;
    const b = el?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (!el || !b || el.scrollWidth <= el.clientWidth) return;
    el.scrollLeft = b.offsetLeft - (el.clientWidth - b.offsetWidth) / 2;
  }, [props.active]);
  const move = (dir: 1 | -1) => {
    const i = props.tabs.findIndex((x) => x.id === props.active);
    props.onChange(props.tabs[(i + dir + props.tabs.length) % props.tabs.length].id);
  };
  return (
    <div
      ref={strip}
      class="tabs"
      role="tablist"
      aria-label={props['aria-label']}
      onKeyDown={(e) => {
        if (e.key === 'ArrowRight') {
          e.preventDefault();
          move(1);
        } else if (e.key === 'ArrowLeft') {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {props.tabs.map((tab) => (
        <button
          key={tab.id}
          type="button"
          role="tab"
          id={`tab-${tab.id}`}
          aria-selected={tab.id === props.active ? 'true' : 'false'}
          tabIndex={tab.id === props.active ? 0 : -1}
          data-testid={tab['data-testid']}
          aria-label={tab['aria-label']}
          onClick={() => props.onChange(tab.id)}
        >
          {/* a label with a count changes in place: in an element of its own, keyed by its text (a page translator rewrites text nodes) */}
          {typeof tab.label === 'string' ? <span key={tab.label}>{tab.label}</span> : tab.label}
        </button>
      ))}
    </div>
  );
}
