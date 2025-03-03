from typing import List

from .source_file import SourceFile
from .location import Range
from .tree_helpers import walk_depth_first


def symbol_ranges(sf: SourceFile, rt=None) -> List[Range]:
    ranges = []

    if rt is None:
        rt = sf.tree.root_node

    for node in walk_depth_first(rt):
        if node.type in sf.lang.identifier_type_names:
            ranges.append(Range.from_ts_node(node))

    return ranges
