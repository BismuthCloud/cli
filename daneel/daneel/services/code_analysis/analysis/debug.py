import tree_sitter

from . import SourceFile, Range


def node_repr(node: tree_sitter.Node, sf: SourceFile):
    contents = sf.content_at(Range.from_ts_node(node)).decode('utf-8')
    return f"{node.type} ({node.start_point}:{node.end_point}) {repr(contents)}"


def _pprint_tree(node: tree_sitter.Node, sf: SourceFile, depth: int):
    print('|  '*depth + node_repr(node, sf))
    for child in node.children:
        _pprint_tree(child, sf, depth+1)


def pprint_tree(sf: SourceFile):
    _pprint_tree(sf.tree.root_node, sf, 0)
