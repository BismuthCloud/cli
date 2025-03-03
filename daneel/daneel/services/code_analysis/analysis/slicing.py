from typing import List, Tuple, Optional, Set
import logging
import tree_sitter

from .location import Range
from .source_file import SourceFile
from .tree_helpers import walk_depth_first, HashNode
from .scopes import enclosing_scopes, Scope
from .langs import NameRef


logger = logging.getLogger(__name__)

def do_slice(sf: SourceFile, target_range: Range, target_scope: Optional[Scope] = None) -> Tuple[Scope, List[Range]]:
    target = sf.lang.name_at_range(target_range)

    if target_scope is None:
        target_scope = enclosing_scopes(sf, target.node)[0]

    target_names = set([target])

    # TODO: this should ensure targets are popped off when they fall out of scope
    target_names = sf.lang.propagate_targets(target_scope.node, target_names)
    delete_nodes = flatten_unreferenced(sf, target_scope.node, target_names)
    deleted_ranges = coalesece_ranges(delete_nodes)

    return target_scope, deleted_ranges


def _propagate_up_name_usage(sf: SourceFile, node: tree_sitter.Node, target_names: Set[NameRef]) -> List[tree_sitter.Node]:
    """
    Returns a list of all nodes which reference any of the target names, or for which any of their children do.
    """
    if node.type in sf.lang.name_types:
        name = NameRef(node, sf.lang._name_components(node))
        if any(tn.affects(name) for tn in target_names):
            return [node]
        return []
    else:
        descendant_refs = []
        for child in node.children:
            descendant_refs += _propagate_up_name_usage(sf, child, target_names)
        if descendant_refs:
            descendant_refs.append(node)
        return descendant_refs


def flatten_unreferenced(sf: SourceFile, target: tree_sitter.Node, names: Set[NameRef]) -> List[tree_sitter.Node]:
    """
    Return the highest-level nodes that do not reference any of the names.
    We use this "negative" approach because it's easier to subtract out node ranges
    than retain a mixture of nodes which might have context to keep (e.g. conditionals).
    """
    references = _propagate_up_name_usage(sf, target, names)
    delete = []
    for node in walk_depth_first(target):
        if node.type in sf.lang.statement_type_names:
            # if this node is not recorded as having a reference to a target, we can delete it
            if node not in references:
                # but first, check if a parent has already been marked for deletion. if so, we can skip this node
                parent = node
                parent_deleted = False
                while parent is not None:
                    parent = parent.parent
                    if parent in delete:
                        parent_deleted = True
                        break
                if not parent_deleted:
                    delete.append(node)

    return delete

def coalesece_ranges(nodes: List[tree_sitter.Node]) -> List[Range]:
    """
    Create a list of ranges that represent the given nodes.
    Ranges are coalesced together only if they are adjacent in the AST
    """
    ranges = []

    i = 0
    while i < len(nodes):
        node = nodes[i]

        start = Range.from_ts_node(node).start

        #
        while i + 1 < len(nodes):
            cur = node
            nxt = cur.next_sibling

            if nxt is None:
                prev = cur
                cur = cur.parent
                while prev == cur.children[-1]:
                    prev = cur
                    cur = cur.parent
                nxt = cur.children[cur.children.index(prev)+1]

            if nxt == nodes[i+1]:
                node = nodes[i+1]
                i += 1
            else:
                break

        end = Range.from_ts_node(node).end

        # TODO: expand end to include any whitespace lines

        ranges.append(Range(start, end))
        i += 1

    return ranges
