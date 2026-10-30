import { type Component, createSignal, Show } from "solid-js";
import { useNavigate } from "@solidjs/router";
import FilePreview from "./FilePreview";
import FileTree from "./FileTree";
import SurfaceHeader from "./SurfaceHeader";
import { openEditorTab } from "./tabs";

interface Props {
  promptPath: (
    title: string,
    defaultValue?: string,
    placeholder?: string,
  ) => Promise<string | null>;
  confirmAction: (title: string, body?: string) => Promise<boolean>;
  onCreatePresetHere?: (path: string) => void;
  onError: (message: string) => void;
  onNotify?: (message: string) => void;
}

/**
 * The workspace file browser as a place of its own (`#/files`): the tree
 * beside a preview pane. Clicking a file previews it here — `.html` rendered
 * in a sandboxed iframe with a source toggle, images as images, everything
 * else as text — and "Edit" leaves for the editor. On a phone the preview
 * takes the pane and "Back to files" returns to the tree.
 */
const FilesPlace: Component<Props> = (props) => {
  const navigate = useNavigate();
  const [previewPath, setPreviewPath] = createSignal<string | null>(null);

  const edit = (path: string) => {
    openEditorTab(path);
    navigate(`/e/${encodeURIComponent(path)}`);
  };

  return (
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
            Workspace files. + uploads or creates; right-click or long-press a
            file or folder for its actions, including Copy path for an agent.
            Click a file to preview it; HTML renders in a sandbox with no
            scripts.
          </p>
        )}
      />
      <div class="files-place-body" classList={{ "files-place-body--previewing": previewPath() !== null }}>
        <div class="files-place-tree">
          <FileTree
            alwaysExpanded
            promptPath={props.promptPath}
            confirmAction={props.confirmAction}
            onCreatePresetHere={props.onCreatePresetHere}
            onError={props.onError}
            onNotify={props.onNotify}
            onPreviewFile={setPreviewPath}
          />
        </div>
        <Show
          when={previewPath()}
          fallback={(
            <p class="files-place-empty">
              Select a file to preview it. HTML files render here, with their
              source one toggle away.
            </p>
          )}
        >
          {(path) => (
            <div class="files-place-preview">
              <button
                type="button"
                class="files-place-back"
                onClick={() => setPreviewPath(null)}
              >Back to files</button>
              <FilePreview
                path={path()}
                onEdit={edit}
                onError={props.onError}
              />
            </div>
          )}
        </Show>
      </div>
    </section>
  );
};

export default FilesPlace;
