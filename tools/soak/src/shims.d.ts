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
}
