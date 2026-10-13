// vite asset imports of the web app's browser loaders (never reached in node)
declare module '*?url' {
  const url: string;
  export default url;
}

// vite's `import.meta.env` of the web app's composition root (web/src/app/services.ts; never reached in node, the bundle drops it)
interface ImportMetaEnv {
  readonly DEV?: boolean;
  readonly [key: string]: unknown;
}
interface ImportMeta {
  readonly env: ImportMetaEnv;
  // vite's `import.meta.glob` of the web app's i18n dictionary (web/src/i18n/index.ts, reached through the order labels of My orders'
  // model): the bundle expands it (rolldown's import-glob plugin, rolldown.config.mjs)
  glob(pattern: string, options: { eager: true }): Record<string, unknown>;
}
