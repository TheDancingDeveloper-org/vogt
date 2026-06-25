import { Component, For, Show, createSignal } from "solid-js";
import type { SessionTemplate } from "./api";

interface Props {
  open: boolean;
  onClose: () => void;
  onSelect: (template: SessionTemplate, name: string) => void;
  templates: SessionTemplate[];
}

const TemplateSelector: Component<Props> = (props) => {
  const [selectedTemplate, setSelectedTemplate] = createSignal<SessionTemplate | null>(null);
  const [sessionName, setSessionName] = createSignal("");
  let nameInputRef: HTMLInputElement | undefined;

  const handleSelect = (template: SessionTemplate) => {
    setSelectedTemplate(template);
    // Auto-generate name based on template
    const timestamp = Date.now() % 1000;
    const defaultName = template.name === "Shell"
      ? `shell-${timestamp}`
      : `${template.name.toLowerCase().replace(/\s+/g, "-")}-${timestamp}`;
    setSessionName(defaultName);
    setTimeout(() => nameInputRef?.select(), 50);
  };

  const handleSubmit = (e: Event) => {
    e.preventDefault();
    const template = selectedTemplate();
    const name = sessionName().trim();
    if (template && name) {
      props.onSelect(template, name);
      setSelectedTemplate(null);
      setSessionName("");
    }
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
    }
  };

  return (
    <Show when={props.open}>
      <div
        class="modal-backdrop"
        onPointerDown={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div class="modal template-selector" onPointerDown={(e) => e.stopPropagation()}>
          <h2>Create New Session</h2>

          <Show
            when={!selectedTemplate()}
            fallback={
              <form onSubmit={handleSubmit}>
                <div class="template-selected">
                  <div class="template-selected-header">
                    <span class="template-selected-name">{selectedTemplate()?.name}</span>
                    <button
                      type="button"
                      class="template-back-btn"
                      onClick={() => setSelectedTemplate(null)}
                    >
                      ← Back
                    </button>
                  </div>
                  <p class="template-selected-desc">{selectedTemplate()?.description}</p>

                  <Show when={selectedTemplate()?.env && selectedTemplate()!.env.length > 0}>
                    <div class="template-env">
                      <strong>Environment:</strong>
                      <ul>
                        <For each={selectedTemplate()!.env}>
                          {([key, value]) => <li><code>{key}={value}</code></li>}
                        </For>
                      </ul>
                    </div>
                  </Show>
                </div>

                <div class="form-group">
                  <label for="session-name">Session Name:</label>
                  <input
                    ref={nameInputRef}
                    id="session-name"
                    type="text"
                    value={sessionName()}
                    onInput={(e) => setSessionName(e.currentTarget.value)}
                    onKeyDown={handleKeyDown}
                    placeholder="Enter session name"
                    autofocus
                    required
                  />
                </div>

                <div class="modal-actions">
                  <button type="button" onClick={() => props.onClose()}>
                    Cancel
                  </button>
                  <button type="submit" disabled={!sessionName().trim()}>
                    Create Session
                  </button>
                </div>
              </form>
            }
          >
            <div class="template-list">
              <For each={props.templates}>
                {(template) => (
                  <button
                    class="template-item"
                    onClick={() => handleSelect(template)}
                  >
                    <div class="template-item-name">{template.name}</div>
                    <div class="template-item-desc">{template.description}</div>
                  </button>
                )}
              </For>
            </div>
          </Show>
        </div>
      </div>
    </Show>
  );
};

export default TemplateSelector;
