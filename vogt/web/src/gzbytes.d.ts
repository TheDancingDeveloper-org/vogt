// A `?gzbytes` import (see the `gz-fixture-bytes` plugin in vitest.config.ts)
// resolves to the decoded bytes of a gzip fixture, for tests only.
declare module "*?gzbytes" {
  const bytes: Uint8Array;
  export default bytes;
}
