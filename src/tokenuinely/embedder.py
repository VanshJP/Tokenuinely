from __future__ import annotations

import voyageai

from .config import EMBED_BATCH_MAX, EMBED_MODEL, EMBED_TOKEN_BUDGET


class Embedder:
    def __init__(self, api_key: str) -> None:
        self._client = voyageai.AsyncClient(api_key=api_key)

    async def embed_query(self, text: str) -> list[float]:
        result = await self._client.embed(
            texts=[text], model=EMBED_MODEL, input_type="query"
        )
        return result.embeddings[0]

    async def embed_documents(self, texts: list[str]) -> list[list[float]]:
        """Embed many headers in as few API calls as possible.

        Chunks `texts` so each chunk has at most EMBED_BATCH_MAX items AND at most
        EMBED_TOKEN_BUDGET tokens (the Voyage per-request token cap for voyage-3 is
        320K; we keep a safety margin).
        """
        if not texts:
            return []
        out: list[list[float]] = []
        for chunk in self._chunks(texts):
            resp = await self._client.embed(
                texts=chunk, model=EMBED_MODEL, input_type="document"
            )
            out.extend(resp.embeddings)
        return out

    def _chunks(self, texts: list[str]) -> list[list[str]]:
        chunks: list[list[str]] = []
        cur: list[str] = []
        cur_tokens = 0
        for t in texts:
            est = max(1, len(t) // 4)
            if cur and (len(cur) >= EMBED_BATCH_MAX or cur_tokens + est > EMBED_TOKEN_BUDGET):
                chunks.append(cur)
                cur, cur_tokens = [], 0
            cur.append(t)
            cur_tokens += est
        if cur:
            chunks.append(cur)
        return chunks
