import { type Component } from "solid-js";
import FileTree from "./FileTree";
import SurfaceHeader from "./SurfaceHeader";

interface Props {
  promptPath: (
    title: string,
    defaultValue?: string,
    placeholder?: string,
  ) => Promise<string | null>;
  confirmAction: (title: string, body?: string) => Promise<boolean>;
  onCreatePresetHere?: (path: string) => void;
  onError: (message: string) => void;
}

/**
 * The workspace file tree as a place of its own (`#/files`).
 *
 * On a desk the tree lives in the Places rail; a phone has no rail, so before
 * this the only way to put a file on the workspace from the app was a desk.
 * The tree here is the same component with the same per-folder actions —
 * expand a folder, open its ⋯ menu, "Upload here" — so choosing where an
 * upload lands is navigating the tree, not typing a path.
 */
const FilesPlace: Component<Props> = (props) => (
  <section class="files-place" aria-label="Files">
    <SurfaceHeader
      label="Files header"
      title={(
        <>
          <p class="place-kicker">Machine</p>
          <h1>Files</h1>
        </>
      )}
      honesty={(
        <p class="files-place-honesty">
          Workspace files. Open a folder's ⋯ menu to upload into it.
        </p>
      )}
    />
    <div class="files-place-body">
      <FileTree
        alwaysExpanded
        promptPath={props.promptPath}
        confirmAction={props.confirmAction}
        onCreatePresetHere={props.onCreatePresetHere}
        onError={props.onError}
      />
    </div>
  </section>
);

export default FilesPlace;
