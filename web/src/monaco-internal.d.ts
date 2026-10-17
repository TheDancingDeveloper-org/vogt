declare module "monaco-editor/esm/vs/editor/standalone/browser/standaloneServices.js" {
  export const StandaloneServices: {
    get<T>(serviceId: unknown): T;
  };
}

declare module "monaco-editor/esm/vs/platform/commands/common/commands.js" {
  export const ICommandService: unknown;
}

// Monaco exposes these files through its wildcard package export, but they do
// not carry declarations that TypeScript's bundler resolver can discover from
// every installed version. Keep the runtime imports deep and lazy while using
// the package's public declaration for the editor namespace.
declare module "monaco-editor/esm/vs/editor/editor.api" {
  export * from "monaco-editor";
}

declare module "monaco-editor/esm/vs/language/*/monaco.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/rust/rust.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/python/python.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/go/go.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/ini/ini.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/shell/shell.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/sql/sql.contribution" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/esm/vs/basic-languages/dockerfile/dockerfile.contribution" {
  const contribution: unknown;
  export default contribution;
}
