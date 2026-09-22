declare module "monaco-editor/editor/standalone/browser/standaloneServices.js" {
  export const StandaloneServices: {
    get<T>(serviceId: unknown): T;
  };
}

declare module "monaco-editor/platform/commands/common/commands.js" {
  export const ICommandService: unknown;
}

// Monaco exposes these files through its wildcard package export, but they do
// not carry declarations that TypeScript's bundler resolver can discover from
// every installed version. Keep the runtime imports deep and lazy while using
// the package's public declaration for the editor namespace.
declare module "monaco-editor/editor/editor.api.js" {
  export * from "monaco-editor";
}

declare module "monaco-editor/language/*/monaco.contribution.js" {
  const contribution: unknown;
  export default contribution;
}

declare module "monaco-editor/languages/definitions/rust/register.js" {}

declare module "monaco-editor/languages/definitions/python/register.js" {}

declare module "monaco-editor/languages/definitions/go/register.js" {}

declare module "monaco-editor/languages/definitions/markdown/register.js" {}

declare module "monaco-editor/languages/definitions/yaml/register.js" {}

declare module "monaco-editor/languages/definitions/ini/register.js" {}

declare module "monaco-editor/languages/definitions/shell/register.js" {}

declare module "monaco-editor/languages/definitions/sql/register.js" {}

declare module "monaco-editor/languages/definitions/dockerfile/register.js" {}
