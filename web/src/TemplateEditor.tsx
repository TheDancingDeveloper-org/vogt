import { Component, For, Show, createSignal } from "solid-js";
import type { SessionTemplate } from "./api";
import {
  addCustomTemplate,
  deleteCustomTemplate,
  getCustomTemplates,
  updateCustomTemplate,
} from "./customTemplates";

interface Props {
  open: boolean;
  onClose: () => void;
}

interface EnvPair {
  key: string;
  value: string;
}

const TemplateEditor: Component<Props> = (props) => {
  const [templates, setTemplates] = createSignal<SessionTemplate[]>(
    getCustomTemplates(),
  );
  const [editingIndex, setEditingIndex] = createSignal<number | null>(null);
  const [name, setName] = createSignal("");
  const [description, setDescription] = createSignal("");
  const [command, setCommand] = createSignal("");
  const [cwd, setCwd] = createSignal("");
  const [defaultName, setDefaultName] = createSignal("");
  const [matchRepoNames, setMatchRepoNames] = createSignal("");
  const [matchPathPrefixes, setMatchPathPrefixes] = createSignal("");
  const [tags, setTags] = createSignal("");
  const [envPairs, setEnvPairs] = createSignal<EnvPair[]>([]);

  const refresh = () => setTemplates(getCustomTemplates());

  const resetForm = () => {
    setName("");
    setDescription("");
    setCommand("");
    setCwd("");
    setDefaultName("");
    setMatchRepoNames("");
    setMatchPathPrefixes("");
    setTags("");
    setEnvPairs([]);
    setEditingIndex(null);
  };

  const startNew = () => {
    resetForm();
    setEditingIndex(-1);
  };

  const loadTemplate = (template: SessionTemplate) => {
    setName(template.name);
    setDescription(template.description);
    setCommand(template.command ? template.command.join("\n") : "");
    setCwd(template.cwd ?? "");
    setDefaultName(template.default_name ?? "");
    setMatchRepoNames((template.match_repo_names ?? []).join(", "));
    setMatchPathPrefixes((template.match_path_prefixes ?? []).join("\n"));
    setTags((template.tags ?? []).join(", "));
    setEnvPairs(template.env.map(([key, value]) => ({ key, value })));
  };

  const startEdit = (index: number) => {
    const template = templates()[index];
    if (!template) return;
    loadTemplate(template);
    setEditingIndex(index);
  };

  const startCopy = (index: number) => {
    const template = templates()[index];
    if (!template) return;
    loadTemplate(template);
    setName(`${template.name} Copy`);
    setEditingIndex(-1);
  };

  const buildTemplate = (): SessionTemplate => ({
    name: name().trim(),
    description: description().trim(),
    command: (() => {
      const args = command()
        .split("\n")
        .map((value) => value.trim())
        .filter(Boolean);
      return args.length > 0 ? args : null;
    })(),
    cwd: cwd().trim() || null,
    env: envPairs()
      .filter((pair) => pair.key.trim())
      .map((pair) => [pair.key.trim(), pair.value] as [string, string]),
    default_name: defaultName().trim() || null,
    match_repo_names: matchRepoNames()
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean),
    match_path_prefixes: matchPathPrefixes()
      .split("\n")
      .map((value) => value.trim())
      .filter(Boolean),
    tags: tags()
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean),
  });

  const save = () => {
    if (!name().trim()) return;
    const template = buildTemplate();
    const index = editingIndex();
    if (index === -1) {
      addCustomTemplate(template);
    } else if (index !== null) {
      updateCustomTemplate(index, template);
    }
    refresh();
    resetForm();
  };

  const remove = (index: number) => {
    deleteCustomTemplate(index);
    refresh();
    if (editingIndex() === index) resetForm();
  };

  const addEnvPair = () => {
    setEnvPairs([...envPairs(), { key: "", value: "" }]);
  };

  const updateEnvPair = (
    index: number,
    field: "key" | "value",
    value: string,
  ) => {
    const pairs = [...envPairs()];
    if (!pairs[index]) return;
    pairs[index] = { ...pairs[index], [field]: value };
    setEnvPairs(pairs);
  };

  const removeEnvPair = (index: number) => {
    setEnvPairs(envPairs().filter((_, current) => current !== index));
  };

  return (
    <Show when={props.open}>
      <div
        class="modal-backdrop"
        onClick={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div class="template-editor" onClick={(e) => e.stopPropagation()}>
          <div class="template-editor-header">
            <h2>Workspace Presets</h2>
            <button class="template-editor-close" onClick={props.onClose}>
              ×
            </button>
          </div>

          <div class="template-editor-body">
            <Show
              when={editingIndex() !== null}
              fallback={
                <div class="template-editor-list">
                  <button class="template-add-btn" onClick={startNew}>
                    + New Preset
                  </button>
                  <Show
                    when={templates().length > 0}
                    fallback={
                      <div class="template-editor-empty">
                        No custom presets yet. Click "New Preset" to create one.
                      </div>
                    }
                  >
                    <For each={templates()}>
                      {(template, index) => (
                        <div class="template-editor-item">
                          <div class="template-editor-item-info">
                            <div class="template-editor-item-name">{template.name}</div>
                            <div class="template-editor-item-desc">
                              {template.description}
                            </div>
                            <Show when={(template.tags?.length ?? 0) > 0}>
                              <div class="template-editor-item-meta">
                                {(template.tags ?? []).join(" • ")}
                              </div>
                            </Show>
                          </div>
                          <div class="template-editor-item-actions">
                            <button onClick={() => startEdit(index())}>Edit</button>
                            <button onClick={() => startCopy(index())}>Copy</button>
                            <button
                              class="danger"
                              onClick={() => remove(index())}
                            >
                              Delete
                            </button>
                          </div>
                        </div>
                      )}
                    </For>
                  </Show>
                </div>
              }
            >
              <div class="template-editor-form">
                <div class="form-group">
                  <label>Name</label>
                  <input
                    type="text"
                    value={name()}
                    onInput={(e) => setName(e.currentTarget.value)}
                    placeholder="Rust service shell"
                    autofocus
                  />
                </div>

                <div class="form-group">
                  <label>Description</label>
                  <input
                    type="text"
                    value={description()}
                    onInput={(e) => setDescription(e.currentTarget.value)}
                    placeholder="Preset for this repo or workspace area"
                  />
                </div>

                <div class="form-group">
                  <label>Command Args (optional, one per line)</label>
                  <textarea
                    rows={5}
                    value={command()}
                    onInput={(e) => setCommand(e.currentTarget.value)}
                    placeholder={"/bin/sh\n-lc\ncargo test"}
                  />
                </div>

                <div class="form-group">
                  <label>Working Directory</label>
                  <input
                    type="text"
                    value={cwd()}
                    onInput={(e) => setCwd(e.currentTarget.value)}
                    placeholder="{repo}/server or Active/apps/MyDevEnv2"
                  />
                </div>

                <div class="form-group">
                  <label>Default Session Name</label>
                  <input
                    type="text"
                    value={defaultName()}
                    onInput={(e) => setDefaultName(e.currentTarget.value)}
                    placeholder="{repo_name}-dev-{timestamp}"
                  />
                </div>

                <div class="form-group">
                  <label>Match Repo Names (comma-separated)</label>
                  <input
                    type="text"
                    value={matchRepoNames()}
                    onInput={(e) => setMatchRepoNames(e.currentTarget.value)}
                    placeholder="MyDevEnv2, rustTorrent"
                  />
                </div>

                <div class="form-group">
                  <label>Match Path Prefixes (one per line)</label>
                  <textarea
                    rows={4}
                    value={matchPathPrefixes()}
                    onInput={(e) => setMatchPathPrefixes(e.currentTarget.value)}
                    placeholder={"Active/apps/MyDevEnv2\nsites/my-site"}
                  />
                </div>

                <div class="form-group">
                  <label>Tags (comma-separated)</label>
                  <input
                    type="text"
                    value={tags()}
                    onInput={(e) => setTags(e.currentTarget.value)}
                    placeholder="rust, dev, repo-specific"
                  />
                </div>

                <div class="form-group">
                  <label>Environment Variables</label>
                  <For each={envPairs()}>
                    {(pair, index) => (
                      <div class="env-pair-row">
                        <input
                          type="text"
                          value={pair.key}
                          onInput={(e) =>
                            updateEnvPair(index(), "key", e.currentTarget.value)
                          }
                          placeholder="KEY"
                        />
                        <span>=</span>
                        <input
                          type="text"
                          value={pair.value}
                          onInput={(e) =>
                            updateEnvPair(index(), "value", e.currentTarget.value)
                          }
                          placeholder="value"
                        />
                        <button
                          class="env-remove-btn"
                          onClick={() => removeEnvPair(index())}
                        >
                          ×
                        </button>
                      </div>
                    )}
                  </For>
                  <button class="env-add-btn" onClick={addEnvPair}>
                    + Add Variable
                  </button>
                </div>

                <div class="template-editor-form-actions">
                  <button onClick={resetForm}>Cancel</button>
                  <button
                    class="primary"
                    onClick={save}
                    disabled={!name().trim()}
                  >
                    {editingIndex() === -1 ? "Create Preset" : "Save Preset"}
                  </button>
                </div>
              </div>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
};

export default TemplateEditor;
