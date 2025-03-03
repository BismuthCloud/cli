from typing import List, Iterator, Dict, Any, Optional, TYPE_CHECKING
from enum import Enum
import tree_sitter

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .source_file import SourceFile

from .location import Range
from .tree_helpers import walk_depth_first, get_depth

if TYPE_CHECKING:
    from .source_file import SourceFile


class ScopeType(Enum):
    UNKNOWN = 0
    FILE = 1
    NAMESPACE = 2
    CLASS = 3
    FUNCTION = 4
    # if, while, for, etc.
    BLOCK_STATEMENT = 5
    # semi-arbitrary spans including things like multi-line string literals, dicts, etc.
    SPAN = 6


class Scope(object):
    typ: ScopeType
    node: tree_sitter.Node
    sf: "SourceFile"
    # The range covering the entire scope
    rng: Range
    # The range covering the docstring of the scope (if applicable)
    doc_rng: Optional[Range]
    # The range covering the body of the scope (if applicable)
    body_rng: Optional[Range]
    # Number of ancestor AST elements to get to the node owning this scope
    # Basically only meaningful to sort Scopes in order of specificity
    depth: int

    def __init__(
        self,
        typ: ScopeType,
        node: tree_sitter.Node,
        sf: "SourceFile",
        rng: Range,
        depth: int,
    ):
        self.typ = typ
        self.node = node
        self.sf = sf
        self.rng = rng
        self.doc_rng = self.sf.lang.scope_comment_range(self)
        if (body := self.sf.lang.body_node(self)) is not None:
            self.body_rng = Range.from_ts_node(body)
        else:
            self.body_rng = None
        self.depth = depth

    @staticmethod
    def from_ts_node(
        typ: ScopeType, node: tree_sitter.Node, sf: "SourceFile"
    ) -> "Scope":
        return Scope(typ, node, sf, Range.from_ts_node(node), get_depth(node))

    def __repr__(self):
        return f"<Scope type {self.typ.name} at {self.rng} depth {self.depth}>"

    def to_dict(self, extra=True):
        d = {
            "type": self.typ.name,
            "range": self.rng.to_dict(),
            "depth": self.depth,
        }

        if extra:
            d["name"] = self.sf.lang.scope_name(self)
            if self.body_rng:
                d["range"]["body_start"] = self.body_rng.start.to_dict()
            comment_range = self.sf.lang.scope_comment_range(self)
            if comment_range is not None:
                comment_formatter = self.sf.lang.docstring_formatter(self)
                d["comment"] = {
                    "range": comment_range.to_dict(),
                    "text": comment_formatter.unformat_comment(
                        self.sf.content_at(comment_range).decode("utf-8")
                    ),
                }

        return d


def scopes(sf: "SourceFile") -> List[Scope]:
    scopes = [
        Scope(
            ScopeType.FILE,
            sf.tree.root_node,
            sf,
            Range.from_ts_node(sf.tree.root_node),
            0,
        ),
    ]

    for node in walk_depth_first(sf.tree.root_node):
        st = sf.lang.scope_type(node)
        if st is not None:
            scopes.append(Scope.from_ts_node(st, node, sf))

    return scopes


def _nested_scopes(
    sf: "SourceFile", node: tree_sitter.Node
) -> Iterator[Dict[str, Any]]:
    if sf.lang.scope_type(node) is not None:
        scope = Scope.from_ts_node(sf.lang.scope_type(node), node, sf)
        d = scope.to_dict()

        d["name"] = sf.lang.scope_name(scope)
        d["children"] = []
        for child in node.children:
            d["children"].extend(_nested_scopes(sf, child))
        yield d
    else:
        for child in node.children:
            yield from _nested_scopes(sf, child)


def filter_children_types(children, cb):
    new_children = []
    for child in children:
        filtered = filter_children_types(child["children"], cb)
        child["children"] = filtered
        if cb(child):
            new_children.append(child)
        else:
            new_children.extend(filtered)
    return new_children


def nested_scopes(sf: "SourceFile", filter_cb=lambda _: True):
    scope = Scope(
        ScopeType.FILE, sf.tree.root_node, sf, Range.from_ts_node(sf.tree.root_node), 0
    )
    d = scope.to_dict()
    d["name"] = sf.lang.scope_name(scope)
    d["children"] = []
    for child in sf.tree.root_node.children:
        d["children"].extend(_nested_scopes(sf, child))

    d["children"] = filter_children_types(d["children"], filter_cb)
    return d


def enclosing_scopes(sf: "SourceFile", node: tree_sitter.Node) -> List[Scope]:
    scopes = []
    node = node.parent

    while node is not None:
        st = sf.lang.scope_type(node)
        if st is not None:
            scopes.append(Scope.from_ts_node(st, node, sf))

        if node.parent is None:
            scopes.append(Scope(ScopeType.FILE, node, sf, Range.from_ts_node(node), 0))
            break
        else:
            node = node.parent

    return scopes


def collapse_nested_scopes(nested):
    while len(nested["children"]) == 1 and nested["children"][0]["type"] in (
        "NAMESPACE",
        "CLASS",
    ):
        nested["children"] = nested["children"][0]["children"]
    return nested
