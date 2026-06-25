import { Component, For, Show, createSignal } from "solid-js";
import type { SessionTemplate } from "./api";
import {
  getCustomTemplates,
  addCustomTemplate,
  updateCustomTemplate,
  deleteCustomTemplate,
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
  const [envPairs, setEnvPairs] = createSignal<EnvPair[]>([]);

  const refresh = () => setTemplates(getCustomTemplates());

  const resetForm = () => {
    setName("");
    setDescription("");
    setCommand("");
    setCwd("");
    setEnvPairs([]);
    setEditingIndex(null);
  };

  const startNew = () => {
    resetForm();
    setEditingIndex(-1); // -1 means "new"
  };

  const startEdit = (index: number) => {
    const t = templates()[index];
    if (!t) return;
    setName(t.name);
    setDescription(t.description);
    setCommand(t.command ? t.command.join(" ") : "");
    setCwd(t.cwd ?? "");
    setEnvPairs(t.env.map(([key, value]) => ({ key, value })));
    setEditingIndex(index);
  };

  const buildTemplate = (): SessionTemplate => ({
    name: name().trim(),
    description: description().trim(),
    command: command().trim() ? command().trim().split(/\s+/) : null,
    cwd: cwd().trim() || null,
    env: envPairs()
      .filter((p) => p.key.trim())
      .map((p) => [p.key.trim(), p.value] as [string, string]),
  });

  const save = () => {
    if (!name().trim()) return;
    const template = buildTemplate();
    const idx = editingIndex();
    if (idx === -1) {
      addCustomTemplate(template);
    } else if (idx !== null) {
      updateCustomTemplate(idx, template);
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

  const updateEnvPair = (index: number, field: "key" | "value", val: string) => {
    const pairs = [...envPairs()];
    if (pairs[index]) {
      pairs[index] = { ...pairs[index], [field]: val };
      setEnvPairs(pairs);
    }
  };

  const removeEnvPair = (index: number) => {
    setEnvPairs(envPairs().filter((_, i) => i !== index));
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
            <h2>Custom Session Templates</h2>
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
                    + New Template
                  </button>
                  <Show
                    when={templates().length > 0}
                    fallback={
                      <div class="template-editor-empty">
                        No custom templates yet. Click "New Template" to create one.
                      </div>
                    }
                  >
                    <For each={templates()}>
                      {(t, index) => (
                        <div class="template-editor-item">
                          <div class="template-editor-item-info">
                            <div class="template-editor-item-name">{t.name}</div>
                            <div class="template-editor-item-desc">
                              {t.description}
                            </div>
                          </div>
                          <div class="template-editor-item-actions">
                            <button onClick={() => startEdit(index())}>Edit</button>
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
                    placeholder="My Custom Environment"
                    autofocus
                  />
                </div>

                <div class="form-group">
                  <label>Description</label>
                  <input
                    type="text"
                    value={description()}
                    onInput={(e) => setDescription(e.currentTarget.value)}
                    placeholder="What this template is for"
                  />
                </div>

                <div class="form-group">
                  <label>Command (optional, space-separated)</label>
                  <input
                    type="text"
                    value={command()}
                    onInput={(e) => setCommand(e.currentTarget.value)}
                    placeholder="bash"
                  />
                </div>

                <div class="form-group">
                  <label>Working Directory (optional)</label>
                  <input
                    type="text"
                    value={cwd()}
                    onInput={(e) => setCwd(e.currentTarget.value)}
                    placeholder="~/projects/myapp"
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
                    {editingIndex() === -1 ? "Create" : "Save"}
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
