/**
 * Quick chat (WI-1097): persistent text chats with an agent (Klaudia), kept
 * forever and found again by search, with a model picker, approval cards for
 * anything that would change something, Stop, and "Continue in a session".
 *
 * The engine owns the conversation; this renders it. One chat is followed at
 * a time over its own event stream, and a dropped stream is answered by
 * re-reading the chat — never by trusting that nothing happened while it was
 * down. The list is re-read on a timer while the panel is open: chats change
 * rarely, and a second always-open stream for the list would cost more than
 * it saves.
 */
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  type Component,
} from "solid-js";
import {
  chatApi,
  subscribeChat,
  type ChatApproval,
  type ChatConfigInfo,
  type ChatDetail,
  type ChatEntry,
  type ChatEvent,
  type ChatSummary,
} from "./api";
import { renderMarkdown } from "./markdown";
import { createNarrow } from "./narrow";

interface Props {
  config: ChatConfigInfo;
  /** The chat the URL names, or null for the list / a new chat. */
  chatId: string | null;
  onSelect: (id: string | null) => void;
  onOpenSession: (sessionId: string, label: string) => void;
  onError: (message: string) => void;
}

const LIST_REFRESH_MS = 15_000;
const RECONNECT_MS = 2_000;

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function when(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const today = new Date();
  return at.toDateString() === today.toDateString()
    ? at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : at.toLocaleDateString([], { day: "numeric", month: "short" });
}

function stateLabel(chat: ChatSummary): string {
  if (chat.promoted_session) return "continued in a session";
  if (chat.state === "awaiting-approval") return "needs approval";
  if (chat.state === "running") return "working";
  return "";
}

export const Chat: Component<Props> = (props) => {
  const narrow = createNarrow();
  const models = () => props.config.drivers[0]?.models ?? [];

  // ── the list ────────────────────────────────────────────────────────
  const [chats, setChats] = createSignal<ChatSummary[]>([]);
  const [query, setQuery] = createSignal("");
  const [showArchived, setShowArchived] = createSignal(false);
  const [listLoaded, setListLoaded] = createSignal(false);
  let listController: AbortController | undefined;
  const loadList = async () => {
    listController?.abort();
    const controller = new AbortController();
    listController = controller;
    try {
      const rows = await chatApi.list(
        { q: query(), archived: showArchived() ? "true" : "false" },
        controller.signal,
      );
      if (!controller.signal.aborted) {
        setChats(rows);
        setListLoaded(true);
      }
    } catch (e) {
      if (!controller.signal.aborted) props.onError(`Could not load chats: ${message(e)}`);
    }
  };
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(on([query, showArchived], () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => void loadList(), 250);
  }));
  const listTimer = setInterval(() => {
    if (document.visibilityState === "visible") void loadList();
  }, LIST_REFRESH_MS);
  onCleanup(() => {
    clearInterval(listTimer);
    clearTimeout(searchTimer);
    listController?.abort();
  });

  // ── the open chat ───────────────────────────────────────────────────
  const [detail, setDetail] = createSignal<ChatDetail | null>(null);
  const [progress, setProgress] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal("");
  const [newModel, setNewModel] = createSignal<string>(models()[0]?.id ?? "default");
  const [busy, setBusy] = createSignal(false);
  let scroller: HTMLDivElement | undefined;
  let composer: HTMLTextAreaElement | undefined;

  const scrollToEnd = () =>
    queueMicrotask(() => {
      if (scroller) scroller.scrollTop = scroller.scrollHeight;
    });

  const refreshChat = async (id: string) => {
    try {
      const fresh = await chatApi.get(id);
      if (props.chatId === id) {
        setDetail(fresh);
        scrollToEnd();
      }
    } catch (e) {
      if (props.chatId === id) props.onError(`Could not load the chat: ${message(e)}`);
    }
  };

  const apply = (id: string, event: ChatEvent) => {
    const current = detail();
    if (!current || current.id !== id) return;
    switch (event.type) {
      case "entry":
        if (current.entries.some((e) => e.seq === event.entry.seq)) return;
        setDetail({ ...current, entries: [...current.entries, event.entry] });
        setProgress(null);
        scrollToEnd();
        return;
      case "progress":
        setProgress(event.text || null);
        return;
      case "approval": {
        const others = current.approvals.filter((a) => a.id !== event.approval.id);
        setDetail({
          ...current,
          approvals: event.approval.status === "pending" ? [...others, event.approval] : others,
        });
        return;
      }
      case "chat":
        setDetail({ ...current, ...event.chat });
        if (event.chat.state === "idle") setProgress(null);
        setChats((rows) => rows.map((r) => (r.id === event.chat.id ? event.chat : r)));
        return;
      case "lagged":
        void refreshChat(id);
        return;
    }
  };

  createEffect(on(() => props.chatId, (id) => {
    setDetail(null);
    setProgress(null);
    if (!id) return;
    let stop: (() => void) | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    const connect = () => {
      stop = subscribeChat(id, (event) => apply(id, event), () => {
        if (closed) return;
        retry = setTimeout(() => {
          void refreshChat(id);
          connect();
        }, RECONNECT_MS);
      });
    };
    void refreshChat(id);
    connect();
    onCleanup(() => {
      closed = true;
      clearTimeout(retry);
      stop?.();
    });
  }));

  const lastUserText = createMemo(() => {
    const entries = detail()?.entries ?? [];
    return [...entries].reverse().find((e) => e.kind === "user")?.text ?? null;
  });

  const send = async (text: string) => {
    const body = text.trim();
    if (!body || busy()) return;
    setBusy(true);
    try {
      const id = props.chatId;
      if (id) {
        await chatApi.send(id, body);
      } else {
        const created = await chatApi.create({
          message: body,
          model: newModel() === "default" ? null : newModel(),
        });
        props.onSelect(created.chat.id);
      }
      setDraft("");
      void loadList();
    } catch (e) {
      props.onError(`Could not send: ${message(e)}`);
    } finally {
      setBusy(false);
      composer?.focus();
    }
  };

  const act = async (what: string, run: () => Promise<unknown>) => {
    try {
      await run();
      void loadList();
    } catch (e) {
      props.onError(`Could not ${what}: ${message(e)}`);
    }
  };

  const decide = (approval: ChatApproval, allow: boolean) =>
    act(allow ? "allow it" : "deny it", () => chatApi.decide(detail()!.id, approval.id, allow));

  const promote = () =>
    act("continue it in a session", async () => {
      const chat = detail()!;
      const result = await chatApi.promote(chat.id, {});
      props.onOpenSession(result.session.id, result.session.name);
    });

  const onComposerKey = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send(draft());
    }
  };

  const [newChat, setNewChat] = createSignal(false);
  // A phone shows one of the two: the list, or the conversation it opened.
  const showList = () => !narrow() || (!props.chatId && !newChat());
  const showConversation = () => !narrow() || Boolean(props.chatId) || newChat();
  createEffect(on(() => props.chatId, (id) => { if (id) setNewChat(false); }));

  const ModelPicker: Component<{ value: string | null; onChange: (id: string) => void; label: string }> = (p) => (
    <select
      class="chat-model"
      aria-label={p.label}
      value={p.value ?? "default"}
      onChange={(e) => p.onChange(e.currentTarget.value)}
    >
      <Show when={models().length === 0}>
        <option value="default">Default model</option>
      </Show>
      <For each={models()}>{(m) => <option value={m.id}>{m.label}</option>}</For>
    </select>
  );

  const Entry: Component<{ entry: ChatEntry }> = (p) => {
    const e = p.entry;
    switch (e.kind) {
      case "user":
        return (
          <div class="chat-row chat-row--user">
            <div class="chat-bubble chat-bubble--user">{e.text}</div>
          </div>
        );
      case "assistant":
        return (
          <div class="chat-row chat-row--assistant">
            <div class="chat-bubble chat-bubble--assistant">{renderMarkdown(e.text)}</div>
          </div>
        );
      case "tool-call":
        return (
          <div class="chat-tool" data-kind="call">
            <span class="chat-tool__name">{e.tool_name ?? "tool"}</span>
            <code class="chat-tool__arg">{e.text}</code>
          </div>
        );
      case "tool-result":
        return (
          <details class={`chat-tool chat-tool--result${e.is_error ? " chat-tool--error" : ""}`}>
            <summary>
              {e.is_error ? "refused / failed" : "result"}
              <Show when={e.tool_name}> · {e.tool_name}</Show>
            </summary>
            <pre>{e.text}</pre>
          </details>
        );
      case "error":
        return (
          <div class="chat-error" role="alert">
            <span>{e.text}</span>
            <Show when={e.retryable && lastUserText()}>
              <button type="button" class="chat-retry" onClick={() => void send(lastUserText()!)}>
                Retry
              </button>
            </Show>
          </div>
        );
      default:
        return <div class={`chat-note chat-note--${e.kind}`}>{e.text}</div>;
    }
  };

  return (
    <section class={`chat${narrow() ? " chat--narrow" : ""}`} aria-label="Chat">
      <Show when={showList()}>
        <aside class="chat-list" aria-label="Chats">
          <div class="chat-list__head">
            <h2>Chat</h2>
            <button
              type="button"
              class="chat-new"
              onClick={() => {
                setNewChat(true);
                props.onSelect(null);
                queueMicrotask(() => composer?.focus());
              }}
            >
              New chat
            </button>
          </div>
          <input
            class="chat-search"
            type="search"
            placeholder="Search chats"
            aria-label="Search chats"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
          />
          <label class="chat-archived-toggle">
            <input
              type="checkbox"
              checked={showArchived()}
              onChange={(e) => setShowArchived(e.currentTarget.checked)}
            />
            Archived
          </label>
          <ul class="chat-list__items">
            <For
              each={chats()}
              fallback={
                <li class="chat-list__empty">
                  {listLoaded()
                    ? query()
                      ? "No chat mentions that."
                      : "No chats yet. Ask a quick question."
                    : "Loading…"}
                </li>
              }
            >
              {(chat) => (
                <li>
                  <button
                    type="button"
                    class={`chat-list__item${props.chatId === chat.id ? " active" : ""}`}
                    aria-current={props.chatId === chat.id ? "page" : undefined}
                    onClick={() => props.onSelect(chat.id)}
                  >
                    <span class="chat-list__title">{chat.title}</span>
                    <span class="chat-list__meta">
                      <Show when={chat.state !== "idle"}>
                        <span class={`chat-state-dot chat-state-dot--${chat.state}`} aria-hidden="true" />
                      </Show>
                      <time datetime={chat.updated_at}>{when(chat.updated_at)}</time>
                    </span>
                    <Show when={stateLabel(chat) || chat.preview}>
                      <span class="chat-list__preview">{stateLabel(chat) || chat.preview}</span>
                    </Show>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </aside>
      </Show>

      <Show when={showConversation()}>
        <div class="chat-main">
          <Show
            when={detail()}
            fallback={
              <Show
                when={!props.chatId}
                fallback={<div class="chat-empty">Loading…</div>}
              >
                <div class="chat-head">
                  <Show when={narrow()}>
                    <button type="button" class="chat-back" aria-label="Back to chats" onClick={() => { setNewChat(false); props.onSelect(null); }}>‹</button>
                  </Show>
                  <h2 class="chat-head__title">New chat</h2>
                  <ModelPicker value={newModel()} onChange={setNewModel} label="Model for the new chat" />
                </div>
                <div class="chat-scroll">
                  <div class="chat-empty">
                    <p>Ask a quick question. The agent can look things up with the same tools a session has.</p>
                    <p class="chat-empty__fine">Reads run at once. Anything that would change something waits for you to allow it.</p>
                  </div>
                </div>
              </Show>
            }
          >
            {(chat) => (
              <>
                <div class="chat-head">
                  <Show when={narrow()}>
                    <button type="button" class="chat-back" aria-label="Back to chats" onClick={() => props.onSelect(null)}>‹</button>
                  </Show>
                  <h2 class="chat-head__title" title={chat().title}>{chat().title}</h2>
                  <ModelPicker
                    value={chat().model}
                    label="Model"
                    onChange={(id) => void act("switch the model", () => chatApi.setModel(chat().id, id))}
                  />
                  <Show when={chat().state !== "idle"}>
                    <button type="button" class="chat-stop" onClick={() => void act("stop it", () => chatApi.interrupt(chat().id))}>
                      Stop
                    </button>
                  </Show>
                  <Show when={!chat().promoted_session && chat().message_count > 0 && chat().state === "idle"}>
                    <button type="button" class="chat-promote" title="Continue this conversation in a terminal session" onClick={() => void promote()}>
                      Continue in session
                    </button>
                  </Show>
                  <button
                    type="button"
                    class="chat-archive"
                    onClick={() => void act("archive it", () => chatApi.archive(chat().id, !chat().archived))}
                  >
                    {chat().archived ? "Unarchive" : "Archive"}
                  </button>
                </div>
                <Show when={chat().promoted_session}>
                  {(session) => (
                    <div class="chat-promoted">
                      This conversation continues in a terminal session.{" "}
                      <a href={`#/t/${session()}`}>Open it ›</a>
                    </div>
                  )}
                </Show>
                <div ref={scroller} class="chat-scroll" aria-live="polite">
                  <For each={chat().entries.filter((e) => e.kind !== "approval")}>
                    {(entry) => <Entry entry={entry} />}
                  </For>
                  <Show when={chat().state === "running"}>
                    <div class="chat-working" role="status">
                      <span class="chat-working__dot" aria-hidden="true" />
                      {progress() ?? "Working…"}
                    </div>
                  </Show>
                </div>
                <For each={chat().approvals}>
                  {(approval) => (
                    <div class="chat-approval" role="region" aria-label={`Approval: ${approval.tool_name}`}>
                      <div class="chat-approval__head">
                        <strong>{approval.source === "driver" ? "The agent asks to change this machine" : "Allow this?"}</strong>
                        <span class="chat-approval__tool">{approval.tool_name}</span>
                      </div>
                      <code class="chat-approval__summary">{approval.summary}</code>
                      <div class="chat-approval__actions">
                        <button type="button" class="chat-allow" onClick={() => void decide(approval, true)}>Allow</button>
                        <button type="button" class="chat-deny" onClick={() => void decide(approval, false)}>Deny</button>
                        <span class="chat-approval__expires">
                          Denied automatically at {new Date(approval.expires_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                        </span>
                      </div>
                    </div>
                  )}
                </For>
              </>
            )}
          </Show>
          <Show when={!detail()?.promoted_session}>
            <form
              class="chat-composer"
              onSubmit={(e) => {
                e.preventDefault();
                void send(draft());
              }}
            >
              <textarea
                ref={composer}
                rows={2}
                placeholder={props.chatId ? "Message" : "Ask anything…"}
                aria-label="Message"
                value={draft()}
                onInput={(e) => setDraft(e.currentTarget.value)}
                onKeyDown={onComposerKey}
              />
              <button type="submit" class="chat-send" disabled={busy() || !draft().trim()}>
                Send
              </button>
            </form>
          </Show>
        </div>
      </Show>
    </section>
  );
};

export default Chat;
