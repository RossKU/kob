import type { ComponentChildren } from 'preact';

export interface TabItem<Id extends string = string> {
  id: Id;
  label: ComponentChildren;
  'data-testid'?: string;
}

/** Tab strip (`role=tablist`). The caller renders the panel of the active tab (give it `role="tabpanel"`). Arrow keys move between tabs. */
export function Tabs<Id extends string = string>(props: { tabs: TabItem<Id>[]; active: Id; onChange: (id: Id) => void; 'aria-label': string }) {
  const move = (dir: 1 | -1) => {
    const i = props.tabs.findIndex((x) => x.id === props.active);
    props.onChange(props.tabs[(i + dir + props.tabs.length) % props.tabs.length].id);
  };
  return (
    <div
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
          onClick={() => props.onChange(tab.id)}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}
