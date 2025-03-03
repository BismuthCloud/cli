from typing import List
from collections import OrderedDict
import tree_sitter

from .scopes import Scope
    
class CallArgument(object):
    node: tree_sitter.Node
    
    def __init__(self, node: tree_sitter.Node):
        self.node = node

class NamedCallArgument(CallArgument):
    name: str
    
    def __init__(self, node: tree_sitter.Node, name: str):
        super().__init__(node)
        self.name = name

class CallSite(object):
    node: tree_sitter.Node
    target: tree_sitter.Node
    args: List[CallArgument]
    
    def __init__(self, node: tree_sitter.Node, target: tree_sitter.Node, args: List[CallArgument]):
        self.node = node
        self.target = target
        self.args = args

class Parameter(object):
    name: str
    required: bool
    
    def __init__(self, name: str, required: bool):
        self.name = name
        self.required = required

class Function(object):
    node: tree_sitter.Node
    name: str
    args: OrderedDict[str, Parameter]
    body: tree_sitter.Node
    
    def __init__(self, node: tree_sitter.Node, name: str, args: OrderedDict[str, Parameter], body: tree_sitter.Node):
        self.node = node
        self.name = name
        self.args = args
        self.body = body