import { Component, For, Show, createMemo, createSignal } from "solid-js";
import type { SessionTemplate } from "./api";
import Dialog from "./Dialog";
import {
  buildDefaultSessionName,
  fillTemplateString,
  templateMatchesContext,
  type TemplateContext,
} from "./customTemplates";

interface Props {
  open: boolean;
  onClose: () => void;
  onSelect: (template: SessionTemplate, name: string) => void;
  templates: SessionTemplate[];
  context: TemplateContext | null;
}

const TemplateSelector: Component<Props> = (props) => {
  const [selectedTemplate, setSelectedTemplate] =
    createSignal<SessionTemplate | null>(null);
  const [sessionName, setSessionName] = createSignal("");
  let nameInputRef: HTMLInputElement | undefined;

  const matchingTemplates = createMemo(() =>
    props.templates.filter((template) => templateMatchesContext(template, props.context)),
  );
  const otherTemplates = createMemo(() =>
    props.templates.filter((template) => !templateMatchesContext(template, props.context)),
  );
  const secondaryTemplates = createMemo(() =>
    props.context?.repoName
      ? matchingTemplates().length > 0
        ? otherTemplates()
        : props.templates
      : props.templates,
  );

  const closeSelector = () => {
    setSelectedTemplate(null);
    setSessionName("");
    props.onClose();
  };

  const handleSelect = (template: SessionTemplate) => {
    setSelectedTemplate(template);
    setSessionName(buildDefaultSessionName(template, props.context));
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
      closeSelector();
    }
  };

  return (
    <Show when={props.open}>
      <Dialog
        title="Create New Session"
        onClose={closeSelector}
        dialogClass="modal template-selector"
        dismissOnBackdrop
      >

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

                  <Show when={props.context?.repoName}>
                    <div class="template-context-note">
                      Repo: <strong>{props.context?.repoName}</strong>
                    </div>
                  </Show>

                  <Show when={selectedTemplate()?.cwd}>
                    <div class="template-context-note">
                      Launch cwd:{" "}
                      <code>
                        {fillTemplateString(
                          selectedTemplate()!.cwd!,
                          selectedTemplate()!,
                          props.context,
                        )}
                      </code>
                    </div>
                  </Show>

                  <Show
                    when={selectedTemplate()?.match_repo_names && selectedTemplate()!.match_repo_names!.length > 0}
                  >
                    <div class="template-context-note">
                      Matches repos: {selectedTemplate()!.match_repo_names!.join(", ")}
                    </div>
                  </Show>

                  <Show when={selectedTemplate()?.env && selectedTemplate()!.env.length > 0}>
                    <div class="template-env">
                      <strong>Environment:</strong>
                      <ul>
                        <For each={selectedTemplate()!.env}>
                          {([key, value]) => (
                            <li>
                              <code>{key}={value}</code>
                            </li>
                          )}
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
                  <button type="button" onClick={closeSelector}>
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
              <Show when={props.context?.repoName && matchingTemplates().length > 0}>
                <div class="template-list-section-label">Recommended for this workspace</div>
                <For each={matchingTemplates()}>
                  {(template) => (
                    <button
                      class="template-item"
                      onClick={() => handleSelect(template)}
                    >
                      <div class="template-item-name">{template.name}</div>
                      <div class="template-item-desc">{template.description}</div>
                      <Show when={(template.tags?.length ?? 0) > 0}>
                        <div class="template-tags">
                          <For each={template.tags ?? []}>
                            {(tag) => <span class="template-tag">{tag}</span>}
                          </For>
                        </div>
                      </Show>
                    </button>
                  )}
                </For>
              </Show>

              <Show
                when={
                  !props.context?.repoName ||
                  otherTemplates().length > 0 ||
                  matchingTemplates().length === 0
                }
              >
                <Show when={matchingTemplates().length > 0}>
                  <div class="template-list-section-label">Other presets</div>
                </Show>
                <For each={secondaryTemplates()}>
                  {(template) => (
                    <button
                      class="template-item"
                      onClick={() => handleSelect(template)}
                    >
                      <div class="template-item-name">{template.name}</div>
                      <div class="template-item-desc">{template.description}</div>
                      <Show when={(template.tags?.length ?? 0) > 0}>
                        <div class="template-tags">
                          <For each={template.tags ?? []}>
                            {(tag) => <span class="template-tag">{tag}</span>}
                          </For>
                        </div>
                      </Show>
                    </button>
                  )}
                </For>
              </Show>
            </div>
            {/* Choosing a preset is not the only way out. Before this the list
                step had no Cancel, and on a phone — no Escape key, no visible
                backdrop to tap — a reader who opened it by mistake had to
                create a session to leave. */}
            <div class="modal-actions">
              <button type="button" onClick={closeSelector}>
                Cancel
              </button>
            </div>
          </Show>
      </Dialog>
    </Show>
  );
};

export default TemplateSelector;
