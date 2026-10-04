"""What an agent did, reduced to something safe to keep.

Agent transcripts (Claude Code's and Codex's JSONL session logs) record every
tool call an agent made and every result it saw. That is the answer to
"which session did X" and "when did this start failing", and it is also a
pile of credentials: environment dumps, kubeconfigs, tokens echoed by a
command, private keys a tool printed. This module is the rule for turning one
tool call into an index row without carrying any of that across.

Three decisions, made here once so every reader inherits them.

**Redaction happens before anything is kept.** `redact` runs on the command
summary and on the result excerpt before either leaves the collector, so the
store never holds an unredacted value it would then have to be trusted not to
show. Redaction is pattern-based and deliberately greedy: a long random-looking
string is replaced even when it is only a hash, because over-redacting an
index row costs a little search recall and under-redacting costs a credential.

**Raw output is never stored.** A result contributes an error flag and, only
when it is an error, a short redacted excerpt. Output from a command that
dumps configuration or environment (`.config.environment`, `printenv`,
kubeconfig reads) or that *looks* like such a dump is withheld entirely; a
redacted environment listing still names every secret it held.

**Service tags and the error flag are heuristics and say so.** They are
regular expressions over the call and its result, good enough to answer
"which sessions touched Komodo" and "what failed", not proof that a call
succeeded or that a service was the one affected.

Pure: no I/O, no clock. The collector reads files and hands text in.
"""

from __future__ import annotations

import json
import re
from collections.abc import Mapping
from dataclasses import dataclass

#: How long a stored command summary may be, after redaction.
SUMMARY_LIMIT = 300

#: How much of an error result is kept, after redaction: the head and tail of
#: the output, because a failure usually says what it was doing at the top and
#: why it stopped at the bottom.
EXCERPT_HEAD = 200
EXCERPT_TAIL = 200

#: How much raw text is ever handed to the redactor. Tool outputs run to
#: megabytes; only the windows an excerpt is cut from need reading, and the
#: excerpt is cut well inside them so a secret straddling a window edge never
#: reaches the kept text half-matched.
SCAN_WINDOW = 8_192

REDACTED = "[REDACTED]"

#: What stands in for the output of a call that dumps configuration.
WITHHELD = "[withheld: configuration or environment output]"

# -- redaction -------------------------------------------------------------

_PEM = re.compile(
    r"-----BEGIN [A-Z0-9 ]+-----.*?(?:-----END [A-Z0-9 ]+-----|\Z)", re.DOTALL
)
_JWT = re.compile(r"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}")

#: Credentials recognisable by their own shape, independent of context.
_TOKEN_SHAPES = re.compile(
    r"(?:"
    r"\bgh[pousr]_[A-Za-z0-9]{20,}"  # GitHub classic/app/user tokens
    r"|\bgithub_pat_[A-Za-z0-9_]{20,}"  # GitHub fine-grained tokens
    r"|\bglpat-[A-Za-z0-9_-]{16,}"  # GitLab personal tokens
    r"|\bsk-[A-Za-z0-9_-]{16,}"  # OpenAI / Anthropic style API keys
    r"|\bxox[abposr]-[A-Za-z0-9-]{10,}"  # Slack tokens
    r"|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"  # AWS access key ids
    r"|\bAIza[0-9A-Za-z_-]{30,}"  # Google API keys
    r"|\bya29\.[0-9A-Za-z_-]{20,}"  # Google OAuth access tokens
    r"|\bnpm_[A-Za-z0-9]{30,}"  # npm tokens
    r"|\bhf_[A-Za-z0-9]{30,}"  # Hugging Face tokens
    r"|\bst\.[A-Za-z0-9-]{8,}\.[A-Fa-f0-9]{16,}\.[A-Fa-f0-9]{16,}"  # Infisical
    r"|\b(?:pk|rk|sk)_(?:live|test)_[A-Za-z0-9]{16,}"  # Stripe keys
    r"|\bAGE-SECRET-KEY-1[0-9A-Z]{20,}"  # age secret keys
    r")"
)

#: `Authorization: Bearer …`, `--header "Authorization: token …"`, and a bare
#: `Bearer …` anywhere.
_AUTH_HEADER = re.compile(
    r"(?i)\b((?:authorization|x-api-key|api-key|x-auth-token|private-token)"
    r"\s*[:=]\s*(?:bearer\s+|basic\s+|token\s+)?)[^\s\"',;]+"
)
_BEARER = re.compile(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}")

#: Credentials embedded in a URL: `https://user:secret@host`.
_URL_USERINFO = re.compile(r"(://)[^/\s:@]+:[^/\s@]+@")

#: The name half of a `NAME=value` / `"name": "value"` / `--name value` pair
#: whose value is a credential by its name.
_SECRET_NAME = (
    r"[A-Za-z0-9_.-]*(?:token|secret|passw(?:or)?d|passwd|pwd|api[_-]?key|apikey"
    r"|private[_-]?key|access[_-]?key|client[_-]?secret|credential|auth[_-]?key"
    r"|session[_-]?key|signing[_-]?key|webhook[_-]?url|dsn)[A-Za-z0-9_.-]*"
)
_ASSIGNMENT = re.compile(
    r"(?i)\b(" + _SECRET_NAME + r")(\s*[=:]\s*)(\"[^\"\n]*\"|'[^'\n]*'|[^\s\"',;}&]+)"
)
_JSON_PAIR = re.compile(
    r"(?i)(\"" + _SECRET_NAME + r"\"\s*:\s*)(\"(?:[^\"\\\n]|\\.)*\"|[^\s,}\]]+)"
)
_FLAG_VALUE = re.compile(
    r"(?i)(--?" + _SECRET_NAME + r"[ =])(\"[^\"\n]*\"|'[^'\n]*'|[^\s\"']+)"
)

#: Long hex is redacted past SHA-1 length (a commit id is worth keeping, a
#: 256-bit key is not); long mixed-case alphanumerics with digits are random
#: by construction.
_LONG_HEX = re.compile(r"\b[A-Fa-f0-9]{48,}\b")
_LONG_BLOB = re.compile(r"[A-Za-z0-9+_=-]{32,}")

#: Output shapes that are a credential store as a whole.
_KUBECONFIG = re.compile(
    r"(?i)client-(?:key|certificate)-data\s*:|certificate-authority-data\s*:"
    r"|\bkind\s*:\s*Config\b[\s\S]*\busers\s*:"
)
_ENV_LINE = re.compile(r"(?m)^\s*(?:export\s+)?[A-Z][A-Z0-9_]{2,}=\S")

#: Commands whose output is a configuration or environment dump. Their output
#: is withheld whole, however it is shaped.
_DUMP_COMMAND = re.compile(
    r"(?i)\.config\.environment|\bconfig\.environment\b|\bprintenv\b"
    r"|(?:^|[;&|(]\s*|\bsudo\s+)env\s*(?:$|[;&|)])|\bdeclare\s+-x\b|\bexport\s+-p\b"
    r"|\bkubectl\s+config\s+view|kube/?config\b|\bk3s\.yaml\b"
    r"|\bcat\s+[^\s|;]*\.env\b|/proc/[^\s]*/environ\b"
    r"|\binfisical\s+(?:secrets|export|run)\b|\bsecrets?\s+(?:get|export|list|show)\b"
    r"|\bGetStack\b|\bGetVariable\b|\bListVariables\b|\bvault\s+(?:kv\s+)?read\b"
)


def _blob(match: re.Match[str]) -> str:
    text = match.group(0)
    has_digit = any(c.isdigit() for c in text)
    has_upper = any(c.isupper() for c in text)
    has_lower = any(c.islower() for c in text)
    if has_digit and has_upper and has_lower:
        return REDACTED
    return text


def redact(text: str) -> str:
    """Replace every credential-shaped substring of `text`.

    Order matters: whole blocks (PEM) first so their bodies are not
    half-redacted by the blob rule, then named shapes, then pairs whose name
    marks the value, then the shape-only fallbacks.
    """
    if not text:
        return text
    out = _PEM.sub("[REDACTED:pem]", text)
    out = _JWT.sub("[REDACTED:jwt]", out)
    out = _TOKEN_SHAPES.sub("[REDACTED:token]", out)
    out = _AUTH_HEADER.sub(lambda m: m.group(1) + REDACTED, out)
    out = _BEARER.sub(lambda m: m.group(1) + REDACTED, out)
    out = _URL_USERINFO.sub(r"\1[REDACTED]@", out)
    out = _JSON_PAIR.sub(lambda m: m.group(1) + '"' + REDACTED + '"', out)
    out = _FLAG_VALUE.sub(lambda m: m.group(1) + REDACTED, out)
    out = _ASSIGNMENT.sub(lambda m: m.group(1) + m.group(2) + REDACTED, out)
    out = _LONG_HEX.sub(REDACTED, out)
    return _LONG_BLOB.sub(_blob, out)


def dumps_secrets(command: str) -> bool:
    """Whether a call's output is a configuration or environment dump."""
    return bool(_DUMP_COMMAND.search(command))


def looks_like_dump(output: str) -> bool:
    """Whether output is shaped like a credential store, whatever produced it.

    A kubeconfig, or three or more `NAME=value` lines — the shape of `env`,
    a `.env` file, or a stack's environment block.
    """
    if _KUBECONFIG.search(output):
        return True
    return len(_ENV_LINE.findall(output, 0, SCAN_WINDOW)) >= 3


def _one_line(text: str) -> str:
    return " ".join(text.split())


def _cut(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[: limit - 1] + "…"


def excerpt(output: str) -> str:
    """A short, redacted head-and-tail of a result's output."""
    if len(output) <= 2 * SCAN_WINDOW:
        cleaned = _one_line(redact(output))
        if len(cleaned) <= EXCERPT_HEAD + EXCERPT_TAIL:
            return cleaned
        return f"{cleaned[:EXCERPT_HEAD]} … {cleaned[-EXCERPT_TAIL:]}"
    head = _one_line(redact(output[:SCAN_WINDOW]))[:EXCERPT_HEAD]
    tail = _one_line(redact(output[-SCAN_WINDOW:]))[-EXCERPT_TAIL:]
    return f"{head} … {tail}"


# -- summaries -------------------------------------------------------------

#: Input fields that name what a call was about, in preference order.
_SUMMARY_FIELDS = (
    "command",
    "cmd",
    "file_path",
    "path",
    "notebook_path",
    "pattern",
    "query",
    "url",
    "description",
    "prompt",
    "skill",
)


def summarize_input(tool: str, call_input: object) -> str:
    """One redacted line saying what a call did."""
    text: str
    if isinstance(call_input, str):
        text = call_input
    elif isinstance(call_input, Mapping):
        picked = [
            str(call_input[key])
            for key in _SUMMARY_FIELDS
            if key in call_input and call_input[key] not in (None, "")
        ]
        if picked:
            text = " ".join(picked[:2])
        else:
            text = json.dumps(call_input, sort_keys=True, default=str)
    elif isinstance(call_input, list):
        text = " ".join(str(part) for part in call_input)
    else:
        text = "" if call_input is None else str(call_input)
    del tool
    return _cut(_one_line(redact(text[: 4 * SCAN_WINDOW])), SUMMARY_LIMIT)


# -- service tags ----------------------------------------------------------

#: Default service heuristics: a name and a case-insensitive pattern over the
#: tool name and its (unredacted, never stored) input. Generic product and
#: protocol names only; an operator's own hosts belong in configuration
#: (`agent_activity_services`), which extends and overrides this table.
DEFAULT_SERVICES: Mapping[str, str] = {
    "github": (
        r"\bgh\s+(?:pr|run|api|workflow|release|repo|issue|secret|variable|auth)\b"
        r"|api\.github\.com|github\.com/|mcp__github"
    ),
    "git": r"\bgit\s+(?:push|pull|fetch|clone|rebase|merge|commit|checkout|switch)\b",
    "docker": r"\bdocker(?:\s+compose|-compose)?\s+\w+|\bghcr\.io\b|\bpodman\b",
    "kubernetes": r"\bkubectl\b|\bhelm\s|\bk3s\b|\bkubeconfig\b",
    "komodo": r"\bkomodo\b|\bDeployStack\b|\bGetStack\b|\bWriteStackFile\b",
    "infisical": r"\binfisical\b",
    "cloudflare": r"\bcloudflare|\bwrangler\b|\bcloudflared\b",
    "caddy": r"\bcaddy\b|\bCaddyfile\b",
    "tailscale": r"\btailscale\b|\btailnet\b",
    "dns": r"\bnslookup\b|\bdig\s+\S|\bresolvectl\b",
    "forgejo": r"\bforgejo\b|\bgitea\b",
    "woodpecker": r"\bwoodpecker\b",
    "firebase": r"\bfirebase\b|\bfcm\b",
    "play": r"\bandroidpublisher\b|\bfastlane\b|\bgoogle play\b|\bplay console\b",
    "ssh": r"""(?:^|[\s;&|("'])(?:ssh|scp|rsync)\s""",
    "http": r"""(?:^|[\s;&|("'])(?:curl|wget|http)\s|\bWebFetch\b""",
    "vogt": r"\bmcp__vogt__|\bvogt\s+\w+",
    "cadastre": r"\bmcp__cadastre__",
}


@dataclass(frozen=True)
class ServiceMatcher:
    """Compiled service heuristics."""

    patterns: tuple[tuple[str, re.Pattern[str]], ...]

    @classmethod
    def build(cls, overrides: Mapping[str, str] | None = None) -> ServiceMatcher:
        merged = dict(DEFAULT_SERVICES)
        for name, pattern in (overrides or {}).items():
            if pattern:
                merged[name] = pattern
            else:
                merged.pop(name, None)
        return cls(
            patterns=tuple(
                (name, re.compile(pattern, re.IGNORECASE))
                for name, pattern in sorted(merged.items())
            )
        )

    def tags(self, tool: str, call_input: object) -> tuple[str, ...]:
        if isinstance(call_input, str):
            text = call_input
        else:
            text = json.dumps(call_input, sort_keys=True, default=str)
        haystack = f"{tool} {text[: 4 * SCAN_WINDOW]}"
        return tuple(name for name, rx in self.patterns if rx.search(haystack))


# -- errors ----------------------------------------------------------------

#: Exit statuses the agents' own wrappers report.
_EXIT_STATUS = re.compile(
    r"(?im)^\s*(?:Script failed\b|Process exited with code [1-9]"
    r"|Exit code:?\s*[1-9]|exit status [1-9]|Command failed with exit code [1-9])"
)
#: Failure lines strong enough to call an error on their own. Matched near the
#: start of the output only, so a file or grep result that merely *mentions*
#: an error is not one.
_FAILURE = re.compile(
    r"(?im)^\s*(?:error\b|fatal:|ERROR\b|Error:|panic:"
    r"|Traceback \(most recent call last\)"
    r"|.*\bcommand not found\b|.*\bpermission denied\b|.*\bNo such file or directory\b"
    r"|.*\b(?:401 Unauthorized|403 Forbidden|HTTP (?:401|403|404|500|502|503|504))\b"
    r"|.*\bconnection refused\b|.*\b(?:timed out|deadline exceeded)\b"
    r"|<tool_use_error>)"
)
_FAILURE_WINDOW = 1_000


#: Tools whose output is a process's own: a shell command, or Codex's script
#: cell. Only for these is the text of the output read for failure lines; any
#: other tool's output is somebody's file, a search hit, or a structured reply
#: that may *mention* an error ("timed out" in an issue title) without being
#: one, so for those only the agent's flag and a wrapper exit status count.
SHELL_TOOLS = frozenset(
    {"Bash", "BashOutput", "exec", "exec_command", "shell", "write_stdin"}
)


def is_error(output: str, *, flagged: bool | None, tool: str = "") -> bool:
    """Whether a result reads as a failure.

    An explicit `is_error` from the agent is believed when true. Otherwise the
    agent wrapper's exit-status line counts for any tool, and the opening
    lines of the output are read for a failure only for shell tools.
    """
    if flagged:
        return True
    if _EXIT_STATUS.search(output[:_FAILURE_WINDOW]):
        return True
    if tool not in SHELL_TOOLS:
        return False
    return bool(_FAILURE.search(output[:_FAILURE_WINDOW]))


def result_excerpt(output: str, *, error: bool, withheld: bool) -> str | None:
    """What of a result is kept: nothing unless it failed, and never a dump."""
    if not error:
        return None
    if withheld or looks_like_dump(output[: 2 * SCAN_WINDOW]):
        return WITHHELD
    return excerpt(output)
