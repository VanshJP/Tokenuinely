from __future__ import annotations

from anthropic import AsyncAnthropic

from .config import HEADER_INPUT_CHAR_LIMIT, HEADER_MODEL

SYSTEM_PROMPT = """You write compact semantic headers for source files. \
Your output will be embedded for retrieval, so be specific and use the vocabulary \
a developer would search for. Do not pad. Do not editorialize. Do not invent details \
not visible in the file."""

USER_TEMPLATE = """Write a header for this file in EXACTLY this format, no extra text:

SUMMARY: <one sentence, <=25 words, what this file does>
KEY SYMBOLS: <comma-separated names of important functions/classes/exports, max 10>
TOUCHES: <comma-separated external things this file uses: DB tables, APIs, env vars, modules it depends on, side effects>
NOT HERE: <comma-separated pointers to related things that live elsewhere, e.g. "auth flows → src/auth/oauth.ts". If unsure, write "none">

File path: {path}
Language: {language}
{truncation_note}
--- FILE CONTENTS ---
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
            max_tokens=400,
            system=SYSTEM_PROMPT,
            messages=[{"role": "user", "content": prompt}],
        )
        parts = [b.text for b in resp.content if getattr(b, "type", None) == "text"]
        return "".join(parts).strip()
