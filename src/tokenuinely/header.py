from __future__ import annotations

from anthropic import AsyncAnthropic

from .config import HEADER_INPUT_CHAR_LIMIT, HEADER_MODEL

SYSTEM_PROMPT = """You write ultra-compact semantic headers for source files.
Your output is (a) embedded for retrieval, (b) read by a coding agent that pays per token.
Optimize for human INTENT, not symbol enumeration.

Rules:
- Mine intent from: module/file docstrings, top-of-file comments, class & function docstrings,
  route/CLI/decorator strings, type names, and architectural cues (layering, naming).
- Prefer the author's own words when a docstring exists; paraphrase only to compress.
- Use the vocabulary a developer would actually search for ("auth callback", "rate limiter",
  "websocket fan-out") over restating syntax ("defines a class with three methods").
- Be specific. No filler ("this file", "various", "utilities", "helpers" alone). No marketing.
- Never invent behavior not visible in the file. If a field has nothing real to say, write "none"."""

USER_TEMPLATE = """Emit EXACTLY these five lines, in order, nothing else. No prose, no code fences.

WHY: <one sentence, <=18 words, the human purpose/role of this file in the system; lift from docstrings when present>
SUMMARY: <one sentence, <=22 words, what it actually does mechanically; complements WHY without repeating it>
KEY SYMBOLS: <up to 8 comma-separated public names (functions/classes/exports/CLI commands/routes); omit private/dunder; no parens, no types>
TOUCHES: <up to 8 comma-separated external dependencies actually used: db tables, HTTP endpoints, env vars (prefix "env:"), package imports, files written, side effects (prefix "fx:")>
NOT HERE: <up to 4 comma-separated redirects of the form "<concept> -> <path>" for behavior a reader might expect here but that lives elsewhere; "none" if truly nothing>

File path: {path}
Language: {language}
{truncation_note}--- FILE CONTENTS ---
{contents}
--- END FILE ---"""


class HeaderGenerator:
    def __init__(self, api_key: str) -> None:
        self._client = AsyncAnthropic(api_key=api_key)

    async def generate(
        self, path: str, language: str | None, contents: str, truncated: bool
    ) -> str:
        if len(contents) > HEADER_INPUT_CHAR_LIMIT:
            contents = contents[:HEADER_INPUT_CHAR_LIMIT]
            truncated = True
        note = ""
        if truncated:
            note = "(NOTE: file was truncated for header generation; describe what is visible)\n"
        prompt = USER_TEMPLATE.format(
            path=path,
            language=language or "unknown",
            truncation_note=note,
            contents=contents,
        )
        resp = await self._client.messages.create(
            model=HEADER_MODEL,
            max_tokens=260,
            system=SYSTEM_PROMPT,
            messages=[{"role": "user", "content": prompt}],
        )
        parts = [b.text for b in resp.content if getattr(b, "type", None) == "text"]
        text = "".join(parts).strip()
        return "\n".join(line.rstrip() for line in text.splitlines() if line.strip())
