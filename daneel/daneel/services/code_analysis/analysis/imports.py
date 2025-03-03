from typing import List
import tree_sitter


class ImportRef(object):
    node: tree_sitter.Node
    source: str
    exposes: List[str]
    
    def __init__(self, node: tree_sitter.Node, source: str, exposes: List[str]):
        self.node = node
        self.source = source
        self.exposes = exposes
    
    def to_dict(self):
        return {
            'source': self.source,
            'exposes': self.exposes,
        }