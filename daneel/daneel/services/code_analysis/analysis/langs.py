from collections import OrderedDict
from typing import Iterator, Tuple, List, Optional, Set, Dict, TYPE_CHECKING
from enum import Enum
import logging
import tree_sitter

from .calls import CallArgument, CallSite, Function, NamedCallArgument, Parameter
from .comments import (
    CommentFormatter,
    LinePrefixComment,
    BlockComment,
    BlockAndLinePrefixComment,
    CommentFormatterChain,
)
from .imports import ImportRef
from .location import Block, Location, Range
from .tree_helpers import walk_depth_first

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .source_file import SourceFile
from .scopes import Scope, ScopeType


logger = logging.getLogger(__name__)


class NameRef(object):
    node: tree_sitter.Node
    components: Tuple[bytes]

    def __init__(self, node: tree_sitter.Node, components: Tuple[bytes]):
        self.node = node
        self.components = components

    def __repr__(self):
        return f"<NameRef {self.components}>"

    def __eq__(self, other) -> bool:
        return self.components == other.components

    def __hash__(self):
        return hash(self.components)

    def affects(self, other: "NameRef") -> bool:
        l = min(len(self.components), len(other.components))
        return self.components[:l] == other.components[:l]


class DocstringLocation(Enum):
    BEFORE = 1
    AFTER = 2


class Language(object):
    # language name that treesitter uses
    TREE_SITTER_LANG_NAME: Optional[str]
    # where docstrings live (before or after class/function line)
    _docstring_location: DocstringLocation
    # formatter for this lang
    _docstring_formatter: CommentFormatter

    # Type names representing "atomic" name fragments (e.g. `self`, `foo`, `bar`)
    identifier_type_names: Tuple[str]
    # Type names representing any possible "complete" name (e.g. `self.foo.bar`)
    name_types: Tuple[str]
    # Type names and the type names for the descendant source and target
    # representing ways a variable can flow into a new variable (e.g. `assignment`)
    propagating_types: Dict[str, Tuple[str, str]]
    # Type names representing statements. Can use inheritance information from node-types.
    statement_type_names: Tuple[str]

    sf: "SourceFile"

    def __init__(self, sf: "SourceFile"):
        self.sf = sf

    def docstring_location(self, scope: Scope) -> DocstringLocation:
        return self._docstring_location

    def docstring_formatter(self, scope: Scope) -> CommentFormatter:
        return self._docstring_formatter

    # TODO: proper abc and @abstractmethod
    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        raise NotImplementedError()

    def scope_name_node(self, scope: Scope) -> Optional[tree_sitter.Node]:
        return scope.node.child_by_field_name("name")

    def scope_name(self, scope: Scope) -> Optional[str]:
        node = self.scope_name_node(scope)
        if node is not None:
            return self.sf.content_at(Range.from_ts_node(node)).decode("utf-8")

        return None

    def body_node(self, scope: Scope) -> Optional[tree_sitter.Node]:
        return scope.node.child_by_field_name("body")

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        """
        Return the Range in the document representing the comment for the given scope.
        This doesn't return a AST node since:
        1) there's not guaranteed to be comment recognition in AST parsing
        2) a comment may span multiple AST nodes
        """
        node = scope.node.prev_sibling
        rng = None
        while node is not None and node.type == "comment":
            if rng is None:
                rng = Range.from_ts_node(node)
            else:
                rng.start = Range.from_ts_node(node).start
            node = node.prev_sibling
        return rng

    def _name_components(self, node: tree_sitter.Node) -> Tuple[bytes]:
        return tuple(
            self.sf.content_at(Range.from_ts_node(anode))
            for anode in walk_depth_first(node)
            if anode.type in self.identifier_type_names
        )

    def name_at_range(self, target_range: Range) -> NameRef:
        for node in walk_depth_first(self.sf.tree.root_node):
            if Range.from_ts_node(node) != target_range:
                continue

            # Capture the outer-most node which this name is a part of
            # e.g. NameRef('self.foo') if the target_range is only on `foo`
            cur = node.parent
            while cur is not None:
                if cur.type in self.name_types:
                    node = cur
                cur = cur.parent

            return NameRef(node, self._name_components(node))

        raise ValueError("Specified target_range does not point to an identifier")

    def referenced_names(self, node: tree_sitter.Node) -> List[NameRef]:
        def _referenced_names(node):
            if node.type in self.name_types:
                yield NameRef(node, self._name_components(node))
            else:
                for child in node.children:
                    yield from _referenced_names(child)

        return list(_referenced_names(node))

    def propagate_targets(
        self, outer_scope: tree_sitter.Node, target_names: Set[NameRef]
    ) -> Set[NameRef]:
        """
        Propagate the set of target names out through all assignments until we hit a fixed point.
        """
        while True:
            len_before = len(target_names)
            for descendant in walk_depth_first(outer_scope):
                if descendant.type in self.propagating_types:
                    defs = descendant.child_by_field_name(
                        self.propagating_types[descendant.type][0]
                    )
                    refs = descendant.child_by_field_name(
                        self.propagating_types[descendant.type][1]
                    )

                    # Guard against things like `with` which may or may no define vars
                    if defs is None or refs is None:
                        continue

                    defs_names = self.referenced_names(defs)
                    refs_names = self.referenced_names(refs)

                    if any(
                        tname.affects(rname)
                        for tname in target_names
                        for rname in refs_names
                    ):
                        target_names.update(defs_names)
                    if any(
                        tname.affects(dname)
                        for tname in target_names
                        for dname in defs_names
                    ):
                        target_names.update(refs_names)

            if len(target_names) == len_before:
                return target_names

    def imports(self, node: tree_sitter.Node) -> List[ImportRef]:
        raise NotImplementedError()

    def functions(self, node: tree_sitter.Node) -> List[Function]:
        raise NotImplementedError()

    def calls(self, node: tree_sitter.Node) -> Iterator[CallSite]:
        raise NotImplementedError()

    def call_compatible(self, call: CallSite, func: Function) -> bool:
        """
        Determine if the given call is compatible with the given function by checking the arguments.
        """
        raise NotImplementedError()


class JavaScript(Language):
    TREE_SITTER_LANG_NAME = "javascript"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = CommentFormatterChain(
        [
            BlockAndLinePrefixComment("/**", "* ", "*/"),
            BlockAndLinePrefixComment("/*", "* ", "*/"),
            LinePrefixComment("// "),
        ]
    )

    identifier_type_names = (
        "identifier",
        "property_identifier",
    )
    name_types = (
        "identifier",
        "member_expression",
    )
    statement_type_names = (
        "break_statement",
        "continue_statement",
        "debugger_statement",
        "declaration",
        "do_statement",
        "empty_statement",
        "export_statement",
        "expression_statement",
        "for_in_statement",
        "for_statement",
        "if_statement",
        "import_statement",
        "labeled_statement",
        "return_statement",
        "statement_block",
        "switch_statement",
        "throw_statement",
        "try_statement",
        "while_statement",
        "with_statement",
    )

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "class_declaration":
            return ScopeType.CLASS
        elif node.type in (
            "function_declaration",
            "generator_function_declaration",
            "arrow_function",
            "method_definition",
        ):  # XXX: 'generator_function'
            return ScopeType.FUNCTION
        elif node.type == "statement_block":
            return ScopeType.BLOCK_STATEMENT
        return None

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        start = None
        end = None

        # walk node out until we hit a node (on this same line) that has a comment as a sibling
        # n.b. the comment doesn't have to be on the immediately prior line,
        # as long as it is the sibling ast-wise.
        # this is basically just a hack for arrow functions
        node = scope.node
        line = Range.from_ts_node(node).start.line
        while node is not None and Range.from_ts_node(node).start.line == line:
            if node.prev_sibling is not None and node.prev_sibling.type == "comment":
                node = node.prev_sibling
                break
            else:
                node = node.parent

            # if we hit another scope (e.g. statement to parent function), bail
            if node is not None and self.scope_type(node) is not None:
                return None

        while node is not None and node.type == "comment":
            if end is None:
                end = Range.from_ts_node(node).end
            start = Range.from_ts_node(node).start
            node = node.prev_sibling

        if start is not None:
            return Range(start, end)

        return None

    def scope_name_node(self, scope: Scope) -> Optional[tree_sitter.Node]:
        if scope.typ == ScopeType.CLASS:
            return scope.node.child_by_field_name("name")
        elif scope.node.type in (
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
        ):
            return scope.node.child_by_field_name("name")
        elif (
            scope.node.type == "arrow_function"
            and scope.node.parent.type == "variable_declarator"
        ):
            return scope.node.parent.child_by_field_name("name")


class Python(Language):
    TREE_SITTER_LANG_NAME = "python"
    _docstring_location = DocstringLocation.AFTER
    _docstring_formatter = BlockComment('"""', '"""')

    identifier_type_names = ("identifier",)
    name_types = (
        "identifier",
        "attribute",
    )
    propagating_types = {
        "assignment": ("right", "left"),
        "with_item": ("value", "alias"),
    }

    # tree_sitter_languages package doesn't include node-types.json to create this list automatically.
    _compound_statement = (
        "if_statement",
        "for_statement",
        "while_statement",
        "try_statement",
        "with_statement",
        "function_definition",
        "class_definition",
        "decorated_definition",
        "match_statement",
    )
    _simple_statement = (
        "assert_statement",
        "break_statement",
        "continue_statement",
        "delete_statement",
        "exec_statement",
        "expression_statement",
        "future_import_statement",
        "global_statement",
        "import_from_statement",
        "import_statement",
        "nonlocal_statement",
        "pass_statement",
        "print_statement",
        "raise_statement",
        "return_statement",
        "type_alias_statement",
    )
    statement_type_names = (*_compound_statement, *_simple_statement)

    other_span_type_names = (
        "list",
        "list_comprehension",
        "dictionary",
        "dictionary_comprehension",
        "generator_expression",
    )

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type in ("class_definition",):
            return ScopeType.CLASS
        elif node.type in ("function_definition",):
            return ScopeType.FUNCTION
        elif node.type in self._compound_statement:
            return ScopeType.BLOCK_STATEMENT
        elif node.type in self.other_span_type_names:
            return ScopeType.SPAN
        elif node.type == "string" and node.children[0].text == b'"""':
            return ScopeType.SPAN
        return None

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        match self.body_node(scope):
            case tree_sitter.Node(
                children=[
                    tree_sitter.Node(
                        type="expression_statement",
                        children=[
                            docstring,
                            *_,
                        ],
                    ),
                    *_,
                ]
            ) if docstring.type == "string":
                try:
                    start, _, end = docstring.children
                    return Range(
                        Range.from_ts_node(start).end,
                        Range.from_ts_node(end).start,
                    )
                except:
                    pass

        return None

    def imports(self, node: tree_sitter.Node) -> List[ImportRef]:
        refs = []
        for node in walk_depth_first(node):
            if node.type == "import_statement":
                refs.extend(
                    ImportRef(
                        node,
                        name_node.text.decode("utf-8"),
                        [name_node.text.decode("utf-8")],
                    )
                    for name_node in node.children_by_field_name("name")
                )
            elif node.type == "import_from_statement":
                # TODO: expand *
                source = node.child_by_field_name("module_name").text.decode("utf-8")
                refs.extend(
                    ImportRef(node, source, [name_node.text.decode("utf-8")])
                    for name_node in node.children_by_field_name("name")
                )
        return refs

    # Special parameters types for Python
    class ListSplatParam(Parameter):
        def __init__(self):
            pass

    class DictSplatParam(Parameter):
        def __init__(self):
            pass

    def functions(self, node: tree_sitter.Node) -> List[Function]:
        funcs = []
        for node in walk_depth_first(node):
            if self.scope_type(node) == ScopeType.FUNCTION:
                params = []
                for param in node.child_by_field_name("parameters").children[
                    1:-1
                ]:  # strip parens
                    match param.type:
                        case "identifier" | "typed_parameter":
                            params.append(Parameter(param.text.decode("utf-8"), True))
                        case "default_parameter" | "typed_default_parameter":
                            params.append(
                                Parameter(
                                    param.child_by_field_name("name").text.decode(
                                        "utf-8"
                                    ),
                                    False,
                                )
                            )
                        case "list_splat_parameter":
                            params.append(self.ListSplatParam())
                        case "dict_splat_parameter":
                            params.append(self.DictSplatParam())
                funcs.append(
                    Function(
                        node,
                        node.child_by_field_name("name").text.decode("utf-8"),
                        OrderedDict((param.name, param) for param in params),
                        node.child_by_field_name("body"),
                    )
                )
        return funcs

    def calls(self, node: tree_sitter.Node) -> Iterator[CallSite]:
        for node in walk_depth_first(node):
            if node.type == "call":
                params = []
                for param in node.child_by_field_name("arguments").children[
                    1:-1
                ]:  # strip parens
                    match param.type:
                        case "list_splat":
                            params.append(self.ListSplatParam())
                        case "dict_splat":
                            params.append(self.DictSplatParam())
                        case "keyword_argument":
                            params.append(
                                NamedCallArgument(
                                    param,
                                    param.child_by_field_name("name").text.decode(
                                        "utf-8"
                                    ),
                                )
                            )
                        case _:
                            params.append(CallArgument(param))
                yield CallSite(
                    node,
                    node.child_by_field_name("function"),
                    params,
                )

    def call_compatible(self, call: CallSite, func: Function) -> bool:
        # TODO: *args and **kwargs handling.
        if any(
            isinstance(arg, (self.ListSplatParam, self.DictSplatParam))
            for arg in func.args.values()
        ):
            return False

        call_names = {
            arg.name for arg in call.args if isinstance(arg, NamedCallArgument)
        }
        func_names = set(func.args.keys())
        if call_names - func_names:
            return False

        # Simple case, func args are all required
        if all(arg.required for arg in func.args.values()):
            # Must have same number of args
            return len(call.args) == len(func.args)
        else:
            # Func has default args
            # Check that we have at least the required number of args
            return len(call.args) >= sum(
                1 if arg.required else 0 for arg in func.args.values()
            )


class TypeScript(JavaScript):
    TREE_SITTER_LANG_NAME = "typescript"


class TSX(JavaScript):
    TREE_SITTER_LANG_NAME = "tsx"


class UnparsedText(Language):
    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        return None

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        return None


class JSON(UnparsedText):
    TREE_SITTER_LANG_NAME = "json"


class CSS(UnparsedText):
    TREE_SITTER_LANG_NAME = "css"


class BASH(UnparsedText):
    TREE_SITTER_LANG_NAME = "bash"


class Elixir(UnparsedText):
    TREE_SITTER_LANG_NAME = "elixir"


class Hcl(UnparsedText):
    TREE_SITTER_LANG_NAME = "hcl"


class Jsdoc(UnparsedText):
    TREE_SITTER_LANG_NAME = "jsdoc"


class HTML(UnparsedText):
    TREE_SITTER_LANG_NAME = "html"


class Svelte(UnparsedText):
    TREE_SITTER_LANG_NAME = "svelte"


class SCSS(UnparsedText):
    TREE_SITTER_LANG_NAME = "scss"


class Markdown(UnparsedText):
    TREE_SITTER_LANG_NAME = "markdown"


class YAML(UnparsedText):
    TREE_SITTER_LANG_NAME = "yaml"


class TOML(UnparsedText):
    TREE_SITTER_LANG_NAME = "toml"


class C(Language):
    TREE_SITTER_LANG_NAME = "c"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = BlockAndLinePrefixComment("/*", "* ", "*/")

    identifier_type_names = (
        "identifier",
        "field_identifier",
    )
    name_types = (
        "identifier",
        "field_expression",
    )
    propagating_types = (("assignment_expression", ("right", "left")),)
    statement_type_names = ("_statement",)

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "function_definition":
            return ScopeType.FUNCTION
        elif (
            node.type == "struct_specifier"
            and node.child_by_field_name("body") is not None
        ):
            return ScopeType.CLASS
        return None


class CPP(C):
    TREE_SITTER_LANG_NAME = "cpp"

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "function_definition":
            return ScopeType.FUNCTION
        elif (
            node.type in ("class_specifier", "struct_specifier")
            and node.child_by_field_name("body") is not None
        ):
            return ScopeType.CLASS
        elif node.type == "namespace_definition":
            return ScopeType.NAMESPACE
        return None


class CSharp(Language):
    TREE_SITTER_LANG_NAME = "csharp"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = LinePrefixComment("/// ")

    identifier_type_names = ("identifier",)
    name_types = (
        "identifier",
        "member_access_expression",
    )
    propagating_types = (
        ("assignment_expression", ("right", "left")),
        # TODO for in
    )
    statement_type_names = ("_statement",)

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "namespace_declaration":
            return ScopeType.NAMESPACE
        elif node.type == "class_declaration":
            return ScopeType.CLASS
        elif node.type in ("_function_body", "method_declaration"):
            return ScopeType.FUNCTION
        return None


class Go(Language):
    TREE_SITTER_LANG_NAME = "go"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = LinePrefixComment("// ")

    identifier_type_names = (
        "identifier",
        "field_identifier",
    )
    name_types = (
        "identifier",
        "selector_expression",
    )
    propagating_types = (
        ("assignment_statement", ("right", "left")),
        ("short_var_declaration", ("right", "left")),
    )
    statement_type_names = ("_statement",)

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type in ("function_declaration", "method_declaration"):
            return ScopeType.FUNCTION
        elif node.type == "type_spec":
            return ScopeType.CLASS
        return None


class Java(Language):
    TREE_SITTER_LANG_NAME = "java"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = BlockAndLinePrefixComment("/**", "* ", "*/")

    identifier_type_names = ("identifier",)
    name_types = (
        "identifier",
        "field_access",
    )
    propagating_types = (
        ("assignment_expression", ("right", "left")),
        # TODO for in
    )
    statement_type_names = ("statement",)

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "class_declaration":
            return ScopeType.CLASS
        elif node.type in (
            "method_declaration",
            "compact_constructor_declaration",
            "constructor_declaration",
        ):
            return ScopeType.FUNCTION
        return None

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        node = scope.node.prev_sibling
        rng = None
        while node is not None and node.type in ("line_comment", "block_comment"):
            if rng is None:
                rng = Range.from_ts_node(node)
            else:
                rng.start = Range.from_ts_node(node).start
            node = node.prev_sibling
        return rng


class PHP(Language):
    TREE_SITTER_LANG_NAME = "php"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = BlockAndLinePrefixComment("/***", "* ", "*/")

    identifier_type_names = ("name",)
    name_types = ("name", "member_access_expression")
    propagating_types = (("assignment_expression", ("right", "left")),)
    statement_type_names = ("_statement",)

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type in ("class_declaration",):
            return ScopeType.CLASS
        elif node.type in ("function_definition", "method_declaration"):
            return ScopeType.FUNCTION
        return None


class Ruby(Language):
    TREE_SITTER_LANG_NAME = "ruby"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = CommentFormatterChain(
        [
            BlockAndLinePrefixComment("##", "# "),
            BlockComment("=begin", "=end"),
        ]
    )

    identifier_type_names = ("identifier",)
    name_types = (
        "identifier",
        "call",
    )
    propagating_types = (("assignment", ("right", "left")),)
    statement_type_names = (
        "_statement",
        # Can't use _primary since that includes like `integer`
        "begin",
        "while",
        "until",
        "if",
        "unless",
        "for",
        "case",
    )

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type in ("class",) and node.end_byte - node.start_byte != len("class"):
            return ScopeType.CLASS
        elif node.type in (
            "method",
            "singleton_method",
        ):
            return ScopeType.FUNCTION
        return None

    def body_node(self, scope: Scope) -> Optional[tree_sitter.Node]:
        """
        _sigh_. Ruby doesn't actually have function "bodies".
        since we're only using this right now to determine where to insert comments,
        the first child statement is used
        """
        return scope.node.child_by_field_name("body")

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        start = None
        end = None

        node = scope.node.prev_sibling
        while node is not None and node.type == "comment":
            if end is None:
                end = Range.from_ts_node(node).end
            start = Range.from_ts_node(node).start
            node = node.prev_sibling
            # TODO: break when we find a line with just "##"?

        if start is not None:
            return Range(start, end)

        return None

    def scope_comment_insert_location(self, scope: Scope) -> Location:
        return Location(Range.from_ts_node(scope.node).start.line, 0)


class Rust(Language):
    TREE_SITTER_LANG_NAME = "rust"
    _docstring_location = DocstringLocation.BEFORE
    _docstring_formatter = LinePrefixComment("/// ")

    identifier_type_names = ("identifier",)
    name_types = (
        "identifier",
        "token_tree",
    )
    propagating_types = (
        ("assignment_expression", ("right", "left")),
        ("let_declaration", ("value", "pattern")),
        # TODO for in
        # TODO if let
        # TODO while let
    )
    # treesitter (and maybe rust's spec?) doesn't have a normal "statement"
    # so we have to do our best and enumerate what is normally used as a statement
    statement_type_names = (
        "let_declaration",
        "macro_invocation",
        "assignment_expression",
        "await_expression",
        "call_expression",
        "compound_assignment_expr",
        "for_expression",
        "if_expression",
        "if_let_expression",
        "loop_expression",
        "match_expression",  # XXX
        "return_expression",
        "struct_expression",
        "try_expression",
        "while_expression",
        "while_let_expression",
    )

    def scope_type(self, node: tree_sitter.Node) -> Optional[ScopeType]:
        if node.type == "function_item":
            return ScopeType.FUNCTION
        elif node.type in ("struct_item", "enum_item"):
            return ScopeType.CLASS
        elif node.type == "mod_item":
            return ScopeType.NAMESPACE
        return None

    def scope_comment_range(self, scope: Scope) -> Optional[Range]:
        node = scope.node.prev_sibling
        rng = None
        while node is not None and node.type in ("line_comment", "block_comment"):
            if rng is None:
                rng = Range.from_ts_node(node)
            else:
                rng.start = Range.from_ts_node(node).start
            node = node.prev_sibling
        return rng


## Bulk


class Actionscript(UnparsedText):
    TREE_SITTER_LANG_NAME = "actionscript"


class Ada(UnparsedText):
    TREE_SITTER_LANG_NAME = "ada"


class Agda(UnparsedText):
    TREE_SITTER_LANG_NAME = "agda"


class Arduino(UnparsedText):
    TREE_SITTER_LANG_NAME = "arduino"


class Asm(UnparsedText):
    TREE_SITTER_LANG_NAME = "asm"


class Astro(UnparsedText):
    TREE_SITTER_LANG_NAME = "astro"


class Beancount(UnparsedText):
    TREE_SITTER_LANG_NAME = "beancount"


class Bibtex(UnparsedText):
    TREE_SITTER_LANG_NAME = "bibtex"


class Bicep(UnparsedText):
    TREE_SITTER_LANG_NAME = "bicep"


class Bitbake(UnparsedText):
    TREE_SITTER_LANG_NAME = "bitbake"


class Cairo(UnparsedText):
    TREE_SITTER_LANG_NAME = "cairo"


class Capnp(UnparsedText):
    TREE_SITTER_LANG_NAME = "capnp"


class Chatito(UnparsedText):
    TREE_SITTER_LANG_NAME = "chatito"


class Clarity(UnparsedText):
    TREE_SITTER_LANG_NAME = "clarity"


class Clojure(UnparsedText):
    TREE_SITTER_LANG_NAME = "clojure"


class Cmake(UnparsedText):
    TREE_SITTER_LANG_NAME = "cmake"


class Comment(UnparsedText):
    TREE_SITTER_LANG_NAME = "comment"


class Commonlisp(UnparsedText):
    TREE_SITTER_LANG_NAME = "commonlisp"


class Cpon(UnparsedText):
    TREE_SITTER_LANG_NAME = "cpon"


class Csv(UnparsedText):
    TREE_SITTER_LANG_NAME = "csv"


class Cuda(UnparsedText):
    TREE_SITTER_LANG_NAME = "cuda"


class D(UnparsedText):
    TREE_SITTER_LANG_NAME = "d"


class Dart(UnparsedText):
    TREE_SITTER_LANG_NAME = "dart"


class Dockerfile(UnparsedText):
    TREE_SITTER_LANG_NAME = "dockerfile"


class Dot(UnparsedText):
    TREE_SITTER_LANG_NAME = "dot"


class Doxygen(UnparsedText):
    TREE_SITTER_LANG_NAME = "doxygen"


class Elisp(UnparsedText):
    TREE_SITTER_LANG_NAME = "elisp"


class Elm(UnparsedText):
    TREE_SITTER_LANG_NAME = "elm"


class Embeddedtemplate(UnparsedText):
    TREE_SITTER_LANG_NAME = "embeddedtemplate"


class Erlang(UnparsedText):
    TREE_SITTER_LANG_NAME = "erlang"


class Fennel(UnparsedText):
    TREE_SITTER_LANG_NAME = "fennel"


class Firrtl(UnparsedText):
    TREE_SITTER_LANG_NAME = "firrtl"


class Fish(UnparsedText):
    TREE_SITTER_LANG_NAME = "fish"


class Fortran(UnparsedText):
    TREE_SITTER_LANG_NAME = "fortran"


class Func(UnparsedText):
    TREE_SITTER_LANG_NAME = "func"


class Gdscript(UnparsedText):
    TREE_SITTER_LANG_NAME = "gdscript"


class Gitattributes(UnparsedText):
    TREE_SITTER_LANG_NAME = "gitattributes"


class Gitcommit(UnparsedText):
    TREE_SITTER_LANG_NAME = "gitcommit"


class Gitignore(UnparsedText):
    TREE_SITTER_LANG_NAME = "gitignore"


class Gleam(UnparsedText):
    TREE_SITTER_LANG_NAME = "gleam"


class Glsl(UnparsedText):
    TREE_SITTER_LANG_NAME = "glsl"


class Gn(UnparsedText):
    TREE_SITTER_LANG_NAME = "gn"


class Gomod(UnparsedText):
    TREE_SITTER_LANG_NAME = "gomod"


class Gosum(UnparsedText):
    TREE_SITTER_LANG_NAME = "gosum"


class Groovy(UnparsedText):
    TREE_SITTER_LANG_NAME = "groovy"


class Gstlaunch(UnparsedText):
    TREE_SITTER_LANG_NAME = "gstlaunch"


class Hack(UnparsedText):
    TREE_SITTER_LANG_NAME = "hack"


class Hare(UnparsedText):
    TREE_SITTER_LANG_NAME = "hare"


class Haskell(UnparsedText):
    TREE_SITTER_LANG_NAME = "haskell"


class Haxe(UnparsedText):
    TREE_SITTER_LANG_NAME = "haxe"


class Heex(UnparsedText):
    TREE_SITTER_LANG_NAME = "heex"


class Hlsl(UnparsedText):
    TREE_SITTER_LANG_NAME = "hlsl"


class Hyperlang(UnparsedText):
    TREE_SITTER_LANG_NAME = "hyperlang"


class Hyprlang(UnparsedText):
    TREE_SITTER_LANG_NAME = "hyprlang"


class Ispc(UnparsedText):
    TREE_SITTER_LANG_NAME = "ispc"


class Janet(UnparsedText):
    TREE_SITTER_LANG_NAME = "janet"


class Jsonnet(UnparsedText):
    TREE_SITTER_LANG_NAME = "jsonnet"


class Julia(UnparsedText):
    TREE_SITTER_LANG_NAME = "julia"


class Kconfig(UnparsedText):
    TREE_SITTER_LANG_NAME = "kconfig"


class Kdl(UnparsedText):
    TREE_SITTER_LANG_NAME = "kdl"


class Kotlin(UnparsedText):
    TREE_SITTER_LANG_NAME = "kotlin"


class Linkerscript(UnparsedText):
    TREE_SITTER_LANG_NAME = "linkerscript"


class Llvm(UnparsedText):
    TREE_SITTER_LANG_NAME = "llvm"


class Lua(UnparsedText):
    TREE_SITTER_LANG_NAME = "lua"


class Luadoc(UnparsedText):
    TREE_SITTER_LANG_NAME = "luadoc"


class Luap(UnparsedText):
    TREE_SITTER_LANG_NAME = "luap"


class Luau(UnparsedText):
    TREE_SITTER_LANG_NAME = "luau"


class Magik(UnparsedText):
    TREE_SITTER_LANG_NAME = "magik"


class Make(UnparsedText):
    TREE_SITTER_LANG_NAME = "make"


class Matlab(UnparsedText):
    TREE_SITTER_LANG_NAME = "matlab"


class Mermaid(UnparsedText):
    TREE_SITTER_LANG_NAME = "mermaid"


class Meson(UnparsedText):
    TREE_SITTER_LANG_NAME = "meson"


class Ninja(UnparsedText):
    TREE_SITTER_LANG_NAME = "ninja"


class Nix(UnparsedText):
    TREE_SITTER_LANG_NAME = "nix"


class Nqc(UnparsedText):
    TREE_SITTER_LANG_NAME = "nqc"


class Objc(UnparsedText):
    TREE_SITTER_LANG_NAME = "objc"


class Odin(UnparsedText):
    TREE_SITTER_LANG_NAME = "odin"


class Org(UnparsedText):
    TREE_SITTER_LANG_NAME = "org"


class Pascal(UnparsedText):
    TREE_SITTER_LANG_NAME = "pascal"


class Pem(UnparsedText):
    TREE_SITTER_LANG_NAME = "pem"


class Perl(UnparsedText):
    TREE_SITTER_LANG_NAME = "perl"


class Pgn(UnparsedText):
    TREE_SITTER_LANG_NAME = "pgn"


class Po(UnparsedText):
    TREE_SITTER_LANG_NAME = "po"


class Pony(UnparsedText):
    TREE_SITTER_LANG_NAME = "pony"


class Powershell(UnparsedText):
    TREE_SITTER_LANG_NAME = "powershell"


class Printf(UnparsedText):
    TREE_SITTER_LANG_NAME = "printf"


class Prisma(UnparsedText):
    TREE_SITTER_LANG_NAME = "prisma"


class Properties(UnparsedText):
    TREE_SITTER_LANG_NAME = "properties"


class Psv(UnparsedText):
    TREE_SITTER_LANG_NAME = "psv"


class Puppet(UnparsedText):
    TREE_SITTER_LANG_NAME = "puppet"


class Purescript(UnparsedText):
    TREE_SITTER_LANG_NAME = "purescript"


class Pymanifest(UnparsedText):
    TREE_SITTER_LANG_NAME = "pymanifest"


class Ql(UnparsedText):
    TREE_SITTER_LANG_NAME = "ql"


class Qmldir(UnparsedText):
    TREE_SITTER_LANG_NAME = "qmldir"


class Query(UnparsedText):
    TREE_SITTER_LANG_NAME = "query"


class R(UnparsedText):
    TREE_SITTER_LANG_NAME = "r"


class Racket(UnparsedText):
    TREE_SITTER_LANG_NAME = "racket"


class Rbs(UnparsedText):
    TREE_SITTER_LANG_NAME = "rbs"


class Re2c(UnparsedText):
    TREE_SITTER_LANG_NAME = "re2c"


class Readline(UnparsedText):
    TREE_SITTER_LANG_NAME = "readline"


class Requirements(UnparsedText):
    TREE_SITTER_LANG_NAME = "requirements"


class Ron(UnparsedText):
    TREE_SITTER_LANG_NAME = "ron"


class Rst(UnparsedText):
    TREE_SITTER_LANG_NAME = "rst"


class Scala(UnparsedText):
    TREE_SITTER_LANG_NAME = "scala"


class Scheme(UnparsedText):
    TREE_SITTER_LANG_NAME = "scheme"


class Slang(UnparsedText):
    TREE_SITTER_LANG_NAME = "slang"


class Smali(UnparsedText):
    TREE_SITTER_LANG_NAME = "smali"


class Smithy(UnparsedText):
    TREE_SITTER_LANG_NAME = "smithy"


class Solidity(UnparsedText):
    TREE_SITTER_LANG_NAME = "solidity"


class Sql(UnparsedText):
    TREE_SITTER_LANG_NAME = "sql"


class Squirrel(UnparsedText):
    TREE_SITTER_LANG_NAME = "squirrel"


class Starlark(UnparsedText):
    TREE_SITTER_LANG_NAME = "starlark"


class Swift(UnparsedText):
    TREE_SITTER_LANG_NAME = "swift"


class Tablegen(UnparsedText):
    TREE_SITTER_LANG_NAME = "tablegen"


class Tcl(UnparsedText):
    TREE_SITTER_LANG_NAME = "tcl"


class Test(UnparsedText):
    TREE_SITTER_LANG_NAME = "test"


class Thrift(UnparsedText):
    TREE_SITTER_LANG_NAME = "thrift"


class Tsv(UnparsedText):
    TREE_SITTER_LANG_NAME = "tsv"


class Twig(UnparsedText):
    TREE_SITTER_LANG_NAME = "twig"


class Typst(UnparsedText):
    TREE_SITTER_LANG_NAME = "typst"


class Udev(UnparsedText):
    TREE_SITTER_LANG_NAME = "udev"


class Ungrammar(UnparsedText):
    TREE_SITTER_LANG_NAME = "ungrammar"


class Uxntal(UnparsedText):
    TREE_SITTER_LANG_NAME = "uxntal"


class V(UnparsedText):
    TREE_SITTER_LANG_NAME = "v"


class Verilog(UnparsedText):
    TREE_SITTER_LANG_NAME = "verilog"


class Vhdl(UnparsedText):
    TREE_SITTER_LANG_NAME = "vhdl"


class Vim(UnparsedText):
    TREE_SITTER_LANG_NAME = "vim"


class Vue(UnparsedText):
    TREE_SITTER_LANG_NAME = "vue"


class Wgsl(UnparsedText):
    TREE_SITTER_LANG_NAME = "wgsl"


class Xcompose(UnparsedText):
    TREE_SITTER_LANG_NAME = "xcompose"


class Xml(UnparsedText):
    TREE_SITTER_LANG_NAME = "xml"


class Yuck(UnparsedText):
    TREE_SITTER_LANG_NAME = "yuck"


class Zig(UnparsedText):
    TREE_SITTER_LANG_NAME = "zig"


class GenericText(UnparsedText):
    """
    Special case for things which dont even have a treesitter parser, e.g. .txt
    """

    TREE_SITTER_LANG_NAME = None
