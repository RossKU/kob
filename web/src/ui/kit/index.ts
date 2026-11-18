// UI KIT: the small, dependency-free component set every view uses (Preact 10, styles in
// src/styles.css). Import from here:
//   import { Button, Field, Banner, Modal, Amount, ... } from '../kit';
//
// Every component forwards `data-testid`. Text comes from the caller, translated with `t()` from
// src/i18n.
//
// Components, with their main props:
// - `Button`: `variant` ('primary' | 'secondary' | 'danger' | 'ghost'), `loading`, `small`,
//   `block`, `disabled` and the button attributes.
// - `Field`: label, input / textarea / select, hint and error. Props are `label`, `hint`, `error`,
// `as` ('input' | 'textarea' | 'select'), `options`, `value`, `onValue(string)`, `onInput(event)`,
//   `type`, `inputMode`, `placeholder`, `disabled`, `readOnly`, `suffix` and `data-testid`, which
//   goes on the CONTROL.
// - `Select`: `Field` as="select" with `options: SelectOption[]`.
// - `Dropdown`: the drop-down behind `Select`, for a control with its own label. Props are
//   `value`, `options` (options and `{ group, options }`), `onChange`, and `labelledBy` or `label`.
// - `Checkbox`: `checked`, `onChange(boolean)`, `label`, `hint`.
// - `FieldShell`: a render-prop wrapper for custom controls, `children({ id, labelId,
//   describedBy, invalid })`.
// - `Segmented`: a radio-group toggle with `options` (`value`, `label`, `tone`, `data-testid`),
//   `value`, `onChange` and `aria-label`.
// - `Table`: a scrollable `<table>` wrapper with `caption`, `dense` and `children`. `TableMessage`
//   with `colSpan` is for empty and loading rows.
// - `Badge`: `tone` ('ok' | 'warn' | 'bad' | 'info' | 'neutral') and `title`.
// - `Banner`: `tone` ('info' | 'ok' | 'warn' | 'error'), `title`, `actions` and `onDismiss`.
//   `ErrorBanner` takes `error` and `onRetry`.
// - `Modal` and `Dialog`: `title`, `onClose`, `footer`, `large`, `dismissable` and `hideClose`.
//   Render them conditionally, as mounted means open.
// - `Spinner` with `large`, and `Loading` with `text`.
// - `CurveGraph`: a stepped sell curve, price against the share sold. Props are `steps`
//   (kob/curve.ts `curveSteps`), `label` and `axis`.
// - `Stepper`: the signing steps of a chained flow. Props are `steps` (`StepperItem[]` with
//   `label`, `state` done | current | waiting | failed, `note` and `busy`), `label` and `layout`.
// - `Amount`: `kind` ('kas' | 'token' | 'price'), `value` (bigint or null), `decimals`, `scale`,
//   `maxFraction`, `unit`, `group` and `signed`.
// - `CopyText`: `value`, `short`, `head` and `tail`. It is a shortened id or address with a copy
//   button. `copyToClipboard(text)` is exported too.
// - `ToastHost`: mount it once (App does). Show messages with `showToast(message, tone,
//   timeoutMs?)`.
// - `Section`: a titled card with `title`, `actions`, `collapsible` and `defaultOpen`.
// - `KeyValueList`: a `<dl>` with `items` (`label`, `value`, `data-testid`, `show`) and `compact`.
// - `Tabs`: a tab strip with `tabs`, `active`, `onChange` and `aria-label`.
// - `EmptyState`: the one empty / disconnected state, with `icon`, `children` (one sentence),
//   `action` and `compact`.
// - `HelpTip`: a "?" beside a label or heading that opens its explanation by click, tap or
//   keyboard, and Escape closes it. Props are `label` (the topic) and `children`.
//
// Helpers:
// - download.ts: `downloadText`, `readFileText`, `exportFileName`, `jsonText`.
// - format.ts: `shortId`, `shortAddress`, `kasText`, `tokenText`, `pricePerTokenText`,
//   `amountText`, `formatRowTime`, `displayPriceDp`, `columnPriceDp`.
// - hooks.ts: `useAsync`, `useInterval`, `useNow`.
// - toast-store.ts: `showToast`, `ToastStore`.
export * from './download';
export * from './format';
export * from './hooks';
export { Amount, type AmountProps } from './Amount';
export { Badge, type BadgeProps, type Tone } from './Badge';
export { IssueLine, RawDetails } from './RawDetails';
export { Banner, ErrorBanner, type BannerProps, type BannerTone } from './Banner';
export { Button, type ButtonProps, type ButtonVariant } from './Button';
export { CopyText, copyToClipboard, type CopyTextProps } from './CopyText';
export { EmptyState, type EmptyIcon, type EmptyStateProps } from './EmptyState';
export { friendlyErrorText } from './friendly-error';
export { Dropdown, usePickerPanel, type DropdownProps } from './Dropdown';
export type { DropdownEntry, DropdownGroup, DropdownOption } from './dropdown-model';
export { Checkbox, Field, FieldShell, Select, type FieldProps, type FieldShellProps, type SelectOption, type SelectProps } from './Field';
export { HelpTip, type HelpTipProps } from './HelpTip';
export { KeyValueList, type KeyValueItem } from './KeyValueList';
export { Dialog, Modal, type ModalProps } from './Modal';
export { Section, type SectionProps } from './Section';
export { useSlideIndicator } from './slide-indicator';
export { Segmented, type SegmentedOption, type SegmentedProps } from './Segmented';
export { Loading, Spinner, type SpinnerProps } from './Spinner';
export { Stepper, type StepperProps } from './Stepper';
export { CurveGraph, type CurveGraphProps } from './CurveGraph';
export type { StepperItem, StepperState } from './stepper-model';
export { SortButton, SortControl, sortKeyOf, useSortState } from './SortHeader';
export { ariaSortOf, chooseSort, nextSort, scaledUnits, sortBy, type SortColumn, type SortDir, type SortState, type SortValue } from './sort';
export { UnitText, unitParts } from './UnitText';
export { Table, TableMessage, type TableProps } from './Table';
export { Tabs, type TabItem } from './Tabs';
export { ToastHost, useToasts } from './Toast';
export { showToast, toasts, ToastStore, type ToastItem, type ToastTone } from './toast-store';
