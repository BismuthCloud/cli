import asyncio
import random
from typing import AsyncIterator, Awaitable, Callable, List, TypeVar, Optional, Dict
from enum import Enum
import re
import google.api_core.exceptions
from vertexai.language_models import TextEmbeddingModel, TextEmbeddingInput  # type: ignore
import os
import opentelemetry.trace
import logging

from asimov.data.postgres.manager import DatabaseManager
from asimov.asimov_base import AsimovBase

from daneel.data.postgres.models import DBModel, Column

tracer = opentelemetry.trace.get_tracer(__name__)

T = TypeVar("T")

ENABLE_EMBEDDING = os.environ.get("GOOGLE_APPLICATION_CREDENTIALS") and os.path.exists(
    os.environ["GOOGLE_APPLICATION_CREDENTIALS"]
)

if not ENABLE_EMBEDDING:
    logging.warning("Embeddings disabled")
else:
    embedding_model = TextEmbeddingModel.from_pretrained("text-embedding-004")


async def async_chunk(
    iterator: AsyncIterator[T], chunk_size: int
) -> AsyncIterator[List[T]]:
    buffer = []
    async for item in iterator:
        buffer.append(item)
        if len(buffer) == chunk_size:
            yield buffer
            buffer = []
    if buffer:
        yield buffer


async def ordered_as_completed(awaitables: list[Awaitable[T]]) -> AsyncIterator[T]:
    futures = [asyncio.ensure_future(a) for a in awaitables]
    pending = set(futures)

    while futures:
        done, pending = await asyncio.wait(pending, return_when=asyncio.FIRST_COMPLETED)
        # We only want to yield the result if it's from the next expected future
        if futures[0] in done:
            yield await futures.pop(0)
            # If we have any previously completed futures that are next in order, yield those too
            while futures and futures[0].done():
                yield await futures.pop(0)


class SearchIndexActionType(Enum):
    CREATE = 0
    DELETE = 2


class SearchIndexAction(AsimovBase):
    type: SearchIndexActionType = SearchIndexActionType.CREATE
    file: Optional[str] = None
    content: Optional[str] = None
    embedding: Optional[List[float]] = None
    id: Optional[int] = None
    node_id: Optional[int] = None


class Code(DBModel):
    @classmethod
    def db_manager(cls) -> DatabaseManager:
        db_manager = DatabaseManager(
            dsn=os.environ.get(
                "CODESEARCH_DSN",
                "postgresql://postgres:postgres@localhost:5435/codesearch",
            )
        )

        return db_manager

    TABLE_NAME = "code"
    COLUMNS = {
        "id": Column("id", "id", int),
        "file": Column("file", "file", str),
        "text": Column("text", "text", str),
        "node_id": Column("node_id", "nodeid", int),
        "graph_id": Column("graph_id", "graphid", str),
        "embedding": Column("embedding", "embedding", list),
    }

    CREATE_TABLE_SQL = f"""
    CREATE TABLE IF NOT EXISTS code (
        id SERIAL PRIMARY KEY,
        file TEXT,
        text TEXT,
        nodeid BIGINT,
        graphid TEXT,
        embedding vector(768)
    );

    CREATE SEQUENCE IF NOT EXISTS code_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;
    """

    FTS_IDX_SQL = """
    CALL paradedb.create_bm25(
        index_name => 'code_search_idx',
        table_name => 'code',
        key_field => 'id',
        text_fields => paradedb.field('text', tokenizer => paradedb.tokenizer('source_code')) ||
                       paradedb.field('file', tokenizer => paradedb.tokenizer('source_code')) ||
                       paradedb.field('graphid', tokenizer => paradedb.tokenizer('raw'))
    );
    """

    VECTOR_IDX_SQL = """
    CREATE INDEX ON public.code
    USING hnsw (embedding vector_cosine_ops)
    """

    @classmethod
    def create_table(cls) -> None:
        cls.db_manager().execute_query(cls.CREATE_TABLE_SQL)
        cls.db_manager().execute_query(cls.FTS_IDX_SQL)
        cls.db_manager().execute_query(cls.VECTOR_IDX_SQL)

    def __init__(
        self,
        graph_id: str,
        file: Optional[str] = None,
        text: Optional[str] = None,
        id: Optional[int] = None,
        node_id: Optional[int] = None,
        embedding: Optional[List[float]] = None,
    ):
        self.graph_id = graph_id
        self.file = file
        self.text = text
        self.id = id
        self.node_id = node_id
        self.embedding = embedding

    @classmethod
    async def _embed(
        cls, batch: List[str], input_type: str, sem: asyncio.Semaphore
    ) -> list[list[float] | None]:

        nonempty_idxs = [i for i in range(len(batch)) if batch[i]]
        if not nonempty_idxs:
            return [None] * len(batch)

        for retry in range(1, 4):
            try:
                async with sem:
                    with tracer.start_as_current_span("_embed"):
                        embeddings = [
                            x.values
                            # for x in await embedding_model.get_embeddings_async(
                            #    [batch[i] for i in nonempty_idxs]
                            # )
                            for x in await asyncio.to_thread(
                                embedding_model.get_embeddings,
                                [
                                    TextEmbeddingInput(
                                        text=batch[i], task_type=input_type
                                    )
                                    for i in nonempty_idxs
                                ],
                            )
                        ]
                        out = []
                        for i in range(len(batch)):
                            if i in nonempty_idxs:
                                out.append(embeddings.pop(0))
                            else:
                                out.append(None)
                        return out

            except google.api_core.exceptions.ResourceExhausted:
                print("429 backing off")
                await asyncio.sleep(5**retry + random.randint(0, 30))
            except google.api_core.exceptions.InternalServerError:
                print("500 retrying")
                await asyncio.sleep(5**retry + random.randint(0, 30))
        raise Exception("retries exceeded")

    @classmethod
    async def embed(
        cls, texts: List[str], input_type: str
    ) -> AsyncIterator[list[float] | None]:
        batches: list[list[str]] = [[]]
        for t in texts:
            batches[-1].append(t)
            # Up to 20,000 tokens are supported in a batch, guestimate 2 chars per token worst case
            # TODO: https://medium.com/google-cloud/counting-gemini-text-tokens-locally-with-the-vertex-ai-sdk-78979fea6244
            if sum(len(x) for x in batches[-1]) > 20000:
                batches.append([])

        if ENABLE_EMBEDDING:
            sem = asyncio.Semaphore(6)
            async for o in ordered_as_completed(
                [cls._embed(batch, input_type, sem) for batch in batches]
            ):
                for e in o:
                    yield e
        else:
            for t in texts:
                yield None

    @classmethod
    async def bulk_action(
        cls,
        graph_id: str,
        all_actions: List[SearchIndexAction],
        progress_cb: Optional[Callable[[float], None]] = None,
        cursor=None,
    ) -> None:
        def group_actions(
            actions: List[SearchIndexAction],
        ) -> Dict[SearchIndexActionType, List[SearchIndexAction]]:
            grouped_actions: dict[SearchIndexActionType, list[SearchIndexAction]] = {
                SearchIndexActionType.CREATE: [],
                SearchIndexActionType.DELETE: [],
            }

            for action in actions:
                grouped_actions[action.type].append(action)

            return grouped_actions

        actions: Dict[SearchIndexActionType, List[SearchIndexAction]] = group_actions(
            all_actions
        )

        if actions[SearchIndexActionType.CREATE]:
            i = 0
            async for embed_batch in async_chunk(
                Code.embed(
                    [x.content for x in actions[SearchIndexActionType.CREATE]],  # type: ignore
                    input_type="RETRIEVAL_DOCUMENT",
                ),
                100,
            ):
                items_batch = actions[SearchIndexActionType.CREATE][
                    i : i + len(embed_batch)
                ]
                i += len(embed_batch)
                codes = []
                for item, embed in zip(items_batch, embed_batch):
                    code = Code(
                        graph_id=graph_id,
                        file=item.file,
                        text=item.content,
                        node_id=item.node_id,
                        embedding=embed,
                    )
                    codes.append(code)
                await asyncio.to_thread(Code.insert_many, codes, cursor=cursor)

                if progress_cb:
                    progress_cb(i / len(actions[SearchIndexActionType.CREATE]))

        Code.delete_many([x.id for x in actions[SearchIndexActionType.DELETE]], cursor=cursor)  # type: ignore

    @classmethod
    async def search(
        cls,
        graph_id: str,
        query: str,
        top: int = 20,
        bm25_weight=0.5,
        vector_weight=0.5,
        cursor=None,
    ) -> list[tuple["Code", float]]:
        query_terms = re.split(r'[ \n`.\(\)\[\]\{\}\'"/-]+', query)
        query_terms = [
            term.strip().replace("\\", "\\\\") for term in query_terms if term.strip()
        ]

        embed = await anext(Code.embed([query], "RETRIEVAL_QUERY"))

        search_sql = (
            (
                f"""
        WITH scores AS (
            SELECT id, score_hybrid AS score FROM code_search_idx.score_hybrid(
                bm25_query => 'text:("""
                + " ".join(f'"{w}"' for w in query_terms)
                + """) OR file:("""
                + " ".join(f'"{w}"' for w in query_terms)
                + f""")^2',
                similarity_query => '''{embed}'' <=> embedding',
                bm25_weight => {bm25_weight},
                similarity_weight => {vector_weight},
                bm25_limit_n => 5000,
                similarity_limit_n => 5000
            )
        )
        """
                if embed is not None
                else f"""
        WITH scores AS (
            SELECT id, score_bm25 AS score FROM code_search_idx.score_bm25(
                'text:("""
                + " ".join(f'"{w}"' for w in query_terms)
                + """) OR file:("""
                + " ".join(f'"{w}"' for w in query_terms)
                + f""")^2',
                limit_rows => 5000
            )
        )
"""
            )
            + f"""
        SELECT code.*, score
        FROM code
        JOIN scores
            ON code.id = scores.id
        WHERE code.graphid = '{graph_id}'
        ORDER BY score DESC
        LIMIT {top};
        """
        )

        return [
            (Code.from_db_row(r), r["score"])
            for r in cls.db_manager().execute_query(search_sql, cursor=cursor)
        ]


def create_tables() -> None:
    Code.create_table()


if __name__ == "__main__":
    create_tables()
