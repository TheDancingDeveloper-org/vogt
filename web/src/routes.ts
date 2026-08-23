// Every URL this shell answers, in one list (FR-U11).
//
// The table lived inline in `index.tsx`, which mounts the router into the
// document and registers a service worker on import — so nothing but a
// browser could ever read it. FR-U11 is a claim about *these strings*: a
// project, a work item, a session and an audit query are each addressable,
// and a pasted link opens the surface it names. Asserting that means mounting
// `App.tsx` behind the same table the shipped bundle routes with, and a table
// copied into a test would pass for a route that had been deleted from the
// product.
//
// A path added here reaches the app only if `App.tsx`'s URL effect has an arm
// for it; the two are deliberately separate, because the router deciding a
// URL exists and the shell deciding what to show are different failures.
export const APP_ROUTES = [
  "/",
  "/sessions",
  "/t/:id",
  "/e/*path",
  "/g",
  "/g/*path",
  "/gui",
  "/history",
  "/tasks",
  "/board",
  "/backlog",
  "/inbox",
  "/projects",
  "/audit",
  "/setup",
  "/settings",
  "/w/:ref",
  "/assistant",
  "/assistant/*path",
] as const;
