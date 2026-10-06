// vite asset imports of the web app's browser loaders (never reached in node)
declare module '*?url' {
  const url: string;
  export default url;
}
