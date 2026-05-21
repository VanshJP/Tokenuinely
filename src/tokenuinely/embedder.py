from __future__ import annotations

import asyncio

import voyageai

from .config import EMBED_MODEL


class Embedder:
    def __init__(self, api_key: str) -> None:
        self._client = voyageai.Client(api_key=api_key)

    async def embed_document(self, text: str) -> list[float]:
        return await asyncio.to_thread(self._embed_one, text, "document")

    async def embed_query(self, text: str) -> list[float]:
        return await asyncio.to_thread(self._embed_one, text, "query")

    def _embed_one(self, text: str, input_type: str) -> list[float]:
        result = self._client.embed(
            texts=[text],
            model=EMBED_MODEL,
            input_type=input_type,
        )
        return result.embeddings[0]
