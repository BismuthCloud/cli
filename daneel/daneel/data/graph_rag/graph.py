from enum import Enum
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import rustworkx as rx
from asimov.asimov_base import AsimovBase
from pydantic import Field


class KGNodeType(Enum):
    FILE = 0
    CLASS = 1
    FUNCTION = 2


class WalkDirection(Enum):
    UP = 0
    Down = 1


class KGNode(AsimovBase):
    type: KGNodeType
    symbol: str
    file_name: str
    # 0-indexed line number
    line: int
    # 0-indexed, exclusive
    end_line: Optional[int] = None
    db_id: Optional[int] = None
    id: Optional[int] = None

    def __hash__(self):
        return self.id

    def str_dict(self):
        return {
            "type": self.type.name,
            "symbol": self.symbol,
            "file_name": self.file_name,
            "line": str(self.line),
            "end_line": str(self.end_line),
            "db_id": str(self.db_id),
            "id": str(self.id),
        }

    @staticmethod
    def from_str_dict(d):
        return KGNode(
            type=KGNodeType[d["type"]],
            symbol=d["symbol"],
            file_name=d["file_name"],
            line=int(d.get("line", 0)),  # TODO: remove .get
            end_line=(
                int(d["end_line"]) if d.get("end_line", "None") != "None" else None
            ),
            db_id=int(d["db_id"]) if d["db_id"] != "None" else None,
            id=int(d["id"]) if d["id"] != "None" else None,
        )


def init_graph() -> rx.PyDiGraph:
    return rx.PyDiGraph(multigraph=True)


class CodeKnowledgeGraph(AsimovBase):
    digraph: rx.PyDiGraph = Field(default_factory=init_graph)
    feature_id: int

    def save(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        rx.node_link_json(
            self.digraph,
            path=str(path),
            node_attrs=lambda n: n.str_dict(),
            edge_attrs=lambda e: {  # type: ignore
                "type": e["type"],
                "reverse": "1" if e.get("reverse", False) else "0",
            },
        )

    @classmethod
    def load(cls, path: Path, feature_id: int):
        graph = rx.from_node_link_json_file(
            str(path),
            node_attrs=lambda d: KGNode.from_str_dict(d),
            edge_attrs=lambda d: {
                "type": d["type"],
                "reverse": d["reverse"] == "1",
            },
        )
        assert isinstance(graph, rx.PyDiGraph)

        edge_index_map = graph.edge_index_map()
        for idx in graph.edge_indices():
            src_node_id, target_node_id, _ = edge_index_map[idx]
            src_file = graph.get_node_data(src_node_id).file_name
            target_file = graph.get_node_data(target_node_id).file_name
            graph.update_edge_by_index(
                idx,
                graph.get_edge_data_by_index(idx)
                | {"idx": idx, "src_file": src_file, "target_file": target_file},
            )

        return cls(
            digraph=graph,
            feature_id=feature_id,
        )

    @property
    def graphid(self):
        return f"{self.feature_id}"

    def add_node(
        self,
        type: KGNodeType,
        symbol: str,
        file_name: str,
        line: int,
        end_line: Optional[int] = None,
        db_id: Optional[int] = None,
        node_id: Optional[int] = None,
    ) -> KGNode:
        node = KGNode(
            type=type,
            symbol=symbol,
            db_id=db_id,
            file_name=file_name,
            line=line,
            end_line=end_line,
        )
        if node_id:
            node.id = node_id
        else:
            node_index = self.digraph.add_node(node)
            node.id = node_index

        self.digraph[node.id] = node

        return node

    def get_nodes(self, ids: List[int]) -> List[KGNode]:
        return [self.digraph.get_node_data(id) for id in ids]

    def add_edges(self, additions: List[Tuple[int, int, Dict[str, Any]]]):
        self.digraph.add_edges_from(additions)
