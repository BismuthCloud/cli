import asyncio
import collections
import os
import pathlib
from typing import Any, Callable, Iterable, List, Optional

import aiohttp
import opentelemetry.trace
import rustworkx as rx
from asimov.asimov_base import AsimovBase
from pydantic import Field, PrivateAttr, model_validator

from daneel.data.graph_rag.code_indexing import ast_parse

from .graph import CodeKnowledgeGraph, KGNode
from .hybrid_search import Code, SearchIndexAction, SearchIndexActionType

tracer = opentelemetry.trace.get_tracer(__name__)


def init_graph() -> rx.PyDiGraph:
    return rx.PyDiGraph(multigraph=True)


class RerankResult(AsimovBase):
    id: int
    score: float


class GraphRag(AsimovBase):
    feature_id: int
    default_path: str = Field(
        default=str(
            pathlib.Path(
                os.environ.get("BISMUTH_GRAPH", "~/.bismuthGraph")
            ).expanduser()
        )
    )
    graph_top: int = 50
    search_top: int = 150
    rerank_top: int = 100
    bm25_weight: float = 0.5
    vector_weight: float = 0.5
    _graph: CodeKnowledgeGraph = PrivateAttr()

    @model_validator(mode="after")
    def initialize_graph(self):
        path = f"{self.default_path}/{self.feature_id}/graph.json"

        if os.path.isfile(path):
            self._graph = CodeKnowledgeGraph.load(
                path,
                feature_id=self.feature_id,
            )
        else:
            self._graph = CodeKnowledgeGraph(
                feature_id=self.feature_id,
                digraph=init_graph(),
            )
        return self

    def save(self):
        path = f"{self.default_path}/{self.feature_id}/graph.json"
        self._graph.save(pathlib.Path(path))

    @tracer.start_as_current_span("bulk_insert")
    async def bulk_insert(
        self,
        nodes: list[KGNode],
        contents: list[str],
        progress_cb: Optional[Callable[[float], None]] = None,
        cursor=None,
    ) -> list[KGNode]:
        for i, node in enumerate(nodes):
            if i % 1000 == 0:
                await asyncio.sleep(0)
            nodes[i] = self._graph.add_node(
                type=node.type,
                symbol=node.symbol,
                file_name=node.file_name,
                line=node.line,
                end_line=node.end_line,
            )
        print("added nodes to graph")
        await Code.bulk_action(
            self._graph.graphid,
            [
                SearchIndexAction(
                    type=SearchIndexActionType.CREATE,
                    file=node.file_name,
                    content=content,
                    node_id=node.id,
                )
                for node, content in zip(nodes, contents)
            ],
            progress_cb=progress_cb,
            cursor=cursor,
        )
        print("added nodes to db")
        return nodes

    async def invalidate(self, file_names: Iterable[str]):
        nodes = self._graph.digraph.nodes()
        ids_by_file = collections.defaultdict(list)
        for node in nodes:
            ids_by_file[node.file_name].append(node.id)

        for file_name in file_names:
            node_ids = ids_by_file[file_name]
            for node_id in node_ids:
                self._graph.digraph.remove_node(node_id)
            await Code.bulk_action(
                self._graph.graphid,
                [
                    SearchIndexAction(
                        type=SearchIndexActionType.DELETE, node_id=node_id
                    )
                    for node_id in node_ids
                ],
            )

    async def _rerank_docs(
        self, query: str, docs: List[Code], top_n: int
    ) -> List[RerankResult]:
        async with aiohttp.ClientSession() as session:
            async with session.post(
                "http://localhost:7373/api/rerank",
                json={
                    "query": query,
                    "top_n": top_n,
                    "docs": [
                        {
                            "id": doc.id,
                            "text": doc.text,
                            "meta": {
                                "file": doc.file,
                            },
                        }
                        for doc in docs
                    ],
                },
            ) as response:
                result = await response.json()
                return [
                    RerankResult(id=item["id"], score=item["score"])
                    for item in result["reranked_docs"]
                ]

    @tracer.start_as_current_span("delete")
    async def delete(self):
        """Delete all graph data for this feature, including database nodes and graph.json file."""
        # Get all node IDs from the graph
        nodes = self._graph.digraph.nodes()
        node_ids = [node.id for node in nodes]

        # Delete all nodes from database
        await Code.bulk_action(
            self._graph.graphid,
            [
                SearchIndexAction(type=SearchIndexActionType.DELETE, node_id=node_id)
                for node_id in node_ids
            ],
        )

        # Delete graph.json file if it exists
        path = f"{self.default_path}/{self.feature_id}/graph.json"
        if os.path.isfile(path):
            os.remove(path)

    @tracer.start_as_current_span("search")
    async def search(
        self,
        query: str,
        seed_nodes: list[KGNode] | None = None,
        overlay_files: dict[str, Optional[str]] = {},
        only_tests: bool = False,
    ) -> list[tuple[KGNode, float]]:
        """
        Search the graph for nodes that match the query.
        Results are returned in order of relevance.
        """
        with Code.db_manager().get_cursor(commit=False) as cursor:
            overlay_nodes, overlay_contents, overlay_deferred_edges = ast_parse(
                {
                    file_name: file_contents
                    for file_name, file_contents in overlay_files.items()
                    if file_contents is not None
                }
            )

            if overlay_nodes:
                overlay_nodes = await self.bulk_insert(
                    overlay_nodes,
                    overlay_contents,
                    cursor=cursor,
                )

                overlay_node_by_sym = {node.symbol: node for node in overlay_nodes}
                overlay_edges: list[tuple[int, int, dict[str, Any]]] = []
                for deferred_edge in overlay_deferred_edges:
                    overlay_edges.append(
                        (
                            overlay_node_by_sym[deferred_edge[0]].id,  # type: ignore
                            overlay_node_by_sym[deferred_edge[1]].id,
                            {
                                "type": deferred_edge[2],
                                "src_file": overlay_node_by_sym[
                                    deferred_edge[0]
                                ].file_name,
                                "target_file": overlay_node_by_sym[
                                    deferred_edge[1]
                                ].file_name,
                                "reverse": False,
                            },
                        )
                    )
                self._graph.add_edges(overlay_edges)
                self._graph.add_edges(
                    [
                        (
                            b,
                            a,
                            {
                                "type": d["type"],
                                "src_file": d["target_file"],
                                "target_file": d["src_file"],
                                "reverse": True,
                            },
                        )
                        for a, b, d in overlay_edges
                    ]
                )

            search_hits = await Code.search(
                self._graph.graphid,
                query,
                top=self.search_top,
                bm25_weight=self.bm25_weight,
                vector_weight=self.vector_weight,
                cursor=cursor,
            )

        personalization: dict[int, float] = {
            hit.node_id: score for hit, score in search_hits  # type: ignore
        }

        if seed_nodes:
            # Weight the seed nodes as high as the highest we got from BM25
            personalization.update(
                {node.id: max(personalization.values()) for node in seed_nodes}  # type: ignore
            )

        merged: dict[int, float] = collections.defaultdict(float)

        def weight_fn_wrapper(only_tests: bool = False, reverse: bool = False):
            def weight_fn(e):
                target_file = e["target_file"]
                if "test" in target_file.split("_")[0]:
                    if only_tests:
                        return 1.0
                    else:
                        return 0.10

                if only_tests:
                    return 0.01
                elif (reverse ^ e["reverse"]) and e["type"] in (
                    "call",
                    "class_ref",
                    "test_coverage",
                ):
                    return 1.0
                else:
                    return 0.01

            return weight_fn

        # "Forward" pagerank (x calls y)
        try:
            pr = rx.pagerank(
                self._graph.digraph,
                weight_fn=weight_fn_wrapper(only_tests=only_tests),
                personalization=personalization,
            )
            for node, weight in pr.items():
                merged[node] += weight

            # Reverse pagerank (y is called by x)
            pr = rx.pagerank(
                self._graph.digraph,
                weight_fn=weight_fn_wrapper(only_tests=only_tests, reverse=True),
                personalization=personalization,
            )
            for node, weight in pr.items():
                merged[node] += weight
        except rx.FailedToConverge:
            print("Failed to converge in graph traversal, returning raw search results")
            merged = personalization

        out_ids = sorted(merged.items(), key=lambda x: x[1], reverse=True)[
            : self.graph_top
        ]

        # Stip out results that are now marked as deleted in overlay (content=None)
        out = [
            (n, weight)
            for n, weight in zip(
                self._graph.get_nodes([item for item, _weight in out_ids]),
                [weight for _item, weight in out_ids],
            )
            if not (n.file_name in overlay_files and overlay_files[n.file_name] is None)
        ]

        # Graph must be saved explicitly for things to persist, so no need to remove nodes from graph

        return out
