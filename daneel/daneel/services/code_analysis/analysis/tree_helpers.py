from typing import Iterable
import tree_sitter

from .location import Range


def walk_depth_first(node: tree_sitter.Node) -> Iterable[tree_sitter.Node]:
    yield node
    for child in node.children:
        yield from walk_depth_first(child)


def walk_breadth_first(node: tree_sitter.Node, is_start=True) -> Iterable[tree_sitter.Node]:
    if is_start:
        yield node

    for child in node.children:
        yield child
    for child in node.children:
        yield from walk_breadth_first(child, False)


def get_depth(node: tree_sitter.Node) -> int:
    depth = 0
    while node is not None:
        node = node.parent
        depth += 1

    return depth


class HashNode(object):
    """
    Simple proxy around a `tree_sitter.Node` that is hashable (and also caches the Node's Range)
    """
    node: tree_sitter.Node
    rng: Range

    def __init__(self, node: tree_sitter.Node):
        self.node = node
        self.rng = Range.from_ts_node(self.node)

    def __repr__(self):
        return f"<HashNode {repr(self.node)}>"

    def __eq__(self, other):
        return isinstance(other, HashNode) and self.node == other.node

    def __hash__(self):
        return hash((self.node.type, self.node.start_byte, self.node.end_byte))

    def __getattr__(self, attr):
        return getattr(self.node, attr)
