declare module "monaco-editor/esm/vs/editor/standalone/browser/standaloneServices.js" {
  export const StandaloneServices: {
    get<T>(serviceId: unknown): T;
  };
}

declare module "monaco-editor/esm/vs/platform/commands/common/commands.js" {
  export const ICommandService: unknown;
}
