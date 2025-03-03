from dataclasses import dataclass
from typing import List, Optional, Tuple

from daneel.services.code_analysis.analysis.langs import Python

from .analysis import (
    scopes,
    enclosing_scopes,
    Scope,
    ScopeType,
    SourceFile,
    Repository,
    Location,
    Range,
    do_slice,
)


def repo_skeleton(r: Repository, include_class_members: bool = False) -> dict[str, str]:
    """
    Create a "skeleton" of the repository, which is the source files with only the class and function declarations/prototypes.
    In the case of python, this is similar to a type stub file for example.
    """
    out = {}
    for filename, sf in r.files.items():
        # TODO: imports which define symbols referenced in class/func declarations? (e.g. superclasses, type hints)
        out[filename] = ""
        f_scopes = sorted(
            (
                s
                for s in scopes(sf)
                if s.typ in (ScopeType.NAMESPACE, ScopeType.CLASS, ScopeType.FUNCTION)
            ),
            key=lambda s: s.rng.start.line,
        )
        for scope in f_scopes:
            start = scope.rng.start
            # If doc comes before the function itself (e.g. JS/TS)
            if scope.doc_rng is not None:
                start = min(start, scope.doc_rng.start)

            # Prefer the end of the docstring if it exists
            if scope.doc_rng is not None:
                end = scope.doc_rng.end
            # Or the start of the body if not
            elif scope.body_rng is not None:
                end = Location(scope.body_rng.start.line, 0)
                if end.line == start.line:
                    end = Location(end.line + 1, 0)
            else:
                print(f"no body for {scope}?")
                end = scope.rng.end

            if include_class_members and scope.typ == ScopeType.CLASS:
                # Include class vars (basically anything up to the first method)
                child_scopes = [
                    s
                    for s in scopes(sf)
                    if s.typ == ScopeType.FUNCTION and s.rng.is_subset(scope.rng)
                ]
                end = Location(
                    min((s.rng.start.line for s in child_scopes), default=end.line), 0
                )

            # Include decorators
            if isinstance(sf.lang, Python):
                lines = sf.contents.splitlines()
                while start.line >= 0 and lines[start.line].strip().startswith(b"@"):
                    start = Location(start.line - 1, 0)

            out[filename] += sf.content_at(Range(start, end).line_align()).decode(
                "utf-8"
            )
    return out


def resolve_dotted_symbol(symbol: str, sf: SourceFile) -> Tuple[Scope, List[Scope]]:
    for scope in scopes(sf):
        # Quick check before we do more expensive node traversal + scope construction
        if sf.lang.scope_name(scope) != symbol.rsplit(".", 1)[-1]:
            continue
        enclosing = list(
            s
            for s in enclosing_scopes(sf, scope.node)
            if s.typ in (ScopeType.NAMESPACE, ScopeType.CLASS, ScopeType.FUNCTION)
        )
        enclosing = enclosing[
            ::-1
        ]  # enclosing_scopes is inner -> outer, we want outer -> inner

        # endswith here because the symbol might be a full, module-qualified name
        # from mypy and we don't have module name info here
        if symbol.endswith(
            ".".join(sf.lang.scope_name(s) for s in enclosing + [scope])
        ):
            return scope, enclosing
    raise ValueError(f"Symbol {symbol} not found")


def extract_full_defs(
    target_filename: str,
    target_file_contents: str,
    target_symbols: List[str],
    add_block_comments: bool = False,
) -> tuple[str, list[tuple[Range, Range]]]:
    """
    Extract the full definition of a class or function.
    The targets are passed as dotted paths (e.g. Class.method).
    Returns the sliced file with the target symbols retained (plus parts of relevant parent scopes - e.g. class definition),
    plus a list of tuples denoting range mappings between the original and sliced file.
    """
    sf = SourceFile(target_filename, target_file_contents.encode("utf-8"))
    retain: set[Range] = set()
    for sym in target_symbols:
        try:
            scope, parent_scopes = resolve_dotted_symbol(sym, sf)
        except ValueError:
            print(f"Symbol {sym} not found in {target_filename}")
            continue

        # Include full definition of the symbol
        retain.add(scope.rng)

        # And prototype of parent scopes
        for parent_scope in parent_scopes:
            start = parent_scope.rng.start

            if parent_scope.typ == ScopeType.CLASS:
                # Include class vars (basically anything up to the first method)
                cls_scopes = [
                    s
                    for s in scopes(sf)
                    if s.typ == ScopeType.FUNCTION and s.rng.is_subset(parent_scope.rng)
                ]
                end = Location(min(s.rng.start.line for s in cls_scopes), 0)
            elif parent_scope.body_rng is not None:
                end = Location(parent_scope.body_rng.start.line, 0)
            else:
                print(f"no body for {parent_scope}?")
                end = parent_scope.rng.end
            retain.add(Range(start, end))

    rngs = []
    out_text = ""
    for rng in sorted(retain, key=lambda r: r.start.line):
        rng = rng.line_align()
        rngs.append(
            (
                rng,
                Range(
                    out_text.count("\n"),
                    out_text.count("\n") + (rng.end.line - rng.start.line),
                ),
            )
        )
        content = sf.content_at(rng).decode("utf-8")

        if add_block_comments:
            content = f"# <BLOCK id='{f"{sf.filename}:{rng.start.line+1}"}' original_lines='{rng.start.line+1}-{rng.end.line+1}'>\n"
            content += sf.content_at(rng).decode("utf-8") + "\n# </BLOCK>\n"

        out_text += content

    return out_text, rngs


def extract_defs_only(
    target_filename: str,
    target_file_contents: str,
    target_symbols: List[str],
    add_block_comments: bool = False,
) -> list[tuple[str, Range]]:
    """
    Extract the full definition of a class or function.
    The targets are passed as dotted paths (e.g. Class.method).
    Returns the chunks of the file containing the target symbols, plus their corresponding ranges.
    """
    sf = SourceFile(target_filename, target_file_contents.encode("utf-8"))
    retain: set[Range] = set()
    for sym in target_symbols:
        try:
            scope, parent_scopes = resolve_dotted_symbol(sym, sf)
        except ValueError:
            print(f"Symbol {sym} not found in {target_filename}")
            continue

        retain.add(scope.rng)

    out = []
    for rng in sorted(retain, key=lambda r: r.start.line):
        rng = rng.line_align()

        content = sf.content_at(rng).decode("utf-8")

        if add_block_comments:
            content = f"# <BLOCK id='{f"{sf.filename}:{rng.start.line+1}"}' original_lines='{rng.start.line+1}-{rng.end.line+1}'>\n"
            content += sf.content_at(rng).decode("utf-8") + "\n# </BLOCK>\n"

        out.append(
            (
                content,
                rng,
            )
        )

    return out


@dataclass
class Block(object):
    rng: Range
    indent: str


def get_blocks(sf: SourceFile) -> List[Block]:
    blocks = [Block(Range(Location(0, 0), Location(0, 0)), "")]
    # TODO: having this on classes seems to trip up the model making it want to rewrite the entire class
    # so just don't have them for now.
    f_scopes = sorted(
        (s for s in scopes(sf) if s.typ in (ScopeType.FUNCTION,)),
        key=lambda s: s.rng.start.line,
    )
    f_lines = sf.contents.splitlines()
    for scope in f_scopes:
        whitespace = sf.content_at(
            Range(Location(scope.rng.start.line, 0), scope.rng.start)
        ).decode("utf-8")
        start = scope.rng.start.line
        # temp hack for decorators
        if start > 0 and f_lines[start - 1].lstrip().startswith(b"@"):
            start -= 1
            while f_lines[start].lstrip().startswith(b"@"):
                start -= 1
        blocks[-1].rng.end = Location(start, 0)
        if start == 0:
            blocks = []
        blocks.append(Block(Range(Location(start, 0), Location(0, 0)), whitespace))
    blocks[-1].rng.end = Range.from_ts_node(sf.tree.root_node).end
    return blocks


def add_block_comments(sf: SourceFile) -> str:
    out = ""
    for block in get_blocks(sf):
        out += f"# <BLOCK id='{f"{sf.filename}:{block.rng.start.line+1}"}' original_lines='{block.rng.start.line+1}-{block.rng.end.line+1}'>\n"
        out += sf.content_at(block.rng).decode("utf-8") + "\n# </BLOCK>\n"
    return out


def exemplar(r: Repository, target_filename: str, target_symbol: str) -> Optional[str]:
    """
    Create an exemplar of using the target symbol by slicing.
    TODO: This would require full proper type inference to know if we're selecting an ambiguous symbol.
    And in this case, ambiguity could be as simple as the same method name on two different classes,
    since the slice point could be like 'foo.bar()' and we don't know if foo is an instance of class A or B.
    Right now, we do 2 things to try and avoid using the wrong thing:
    1. Ensure the argument count to the function is compatible with the target symbol's definition.
    2. Assume the parent class name would be referenced somewhere else in the function that we're slicing.

    TODO: how hard/slow would it be to spin up an LSP and get xrefs?
    """
    if target_filename not in r.files:
        raise ValueError(f"Target file {target_filename} not in files")
    target, _ = resolve_dotted_symbol(target_symbol, r.files[target_filename])
    if target is None:
        raise ValueError(f"Target symbol {target_symbol} not found")

    if target.typ == ScopeType.FUNCTION:
        func = r.files[target_filename].lang.functions(target.node)[0]
    elif target.typ == ScopeType.CLASS:
        class_name = r.files[target_filename].lang.scope_name(target)
        # Get the class's __init__ method for our argument checking
        target, _ = resolve_dotted_symbol(
            target_symbol + ".__init__", r.files[target_filename]
        )
        if target is None:
            # Don't error here because we can't resolve __init__ defined in a parent class which won't be uncommon
            return None
        func = r.files[target_filename].lang.functions(target.node)[0]
        func.name = class_name
    else:
        raise ValueError("Target symbol must be a class or function")

    # prefer target file for candidates first
    for sf in [r.files[target_filename]] + list(r.files.values()):
        candidate_calls = []
        for callsite in sf.lang.calls(sf.tree.root_node):
            if callsite.target.text.decode("utf-8").rsplit(".")[-1] == func.name:
                candidate_calls.append(callsite)

        # If we're looking for a class method, we need to find the class name somewhere in the function
        requires_class = [
            r.files[target_filename].lang.scope_name(scope)
            for scope in enclosing_scopes(r.files[target_filename], target.node)
            if scope.typ == ScopeType.CLASS
        ]
        if requires_class:
            class_name = requires_class[0]
            filtered_calls = []
            for c in candidate_calls:
                func_scope = [
                    scope
                    for scope in enclosing_scopes(sf, c.node)
                    if scope.typ == ScopeType.FUNCTION
                ]
                if func_scope:
                    if class_name in sf.content_at(func_scope[0].rng).decode("utf-8"):
                        filtered_calls.append(c)
            candidate_calls = filtered_calls

        # Now sort candidates by length, and slice the longest to ideally give the most context
        best_candidate = None
        best_func = None
        best_func_len = 0
        for c in candidate_calls:
            func_scope = [
                scope
                for scope in enclosing_scopes(sf, c.node)
                if scope.typ == ScopeType.FUNCTION
            ]
            if func_scope:
                func_len = func_scope[0].rng.end.line - func_scope[0].rng.start.line
                if func_len > best_func_len:
                    best_candidate = c
                    best_func = func_scope[0]
                    best_func_len = func_len

        if best_candidate is not None:
            scope, delete_ranges = do_slice(
                sf, Range.from_ts_node(best_candidate.node), best_func
            )
            lines = sf.content_at(scope.rng).decode("utf-8").splitlines()
            code = ""
            for i, line in enumerate(lines):
                if any(
                    rng.start.line <= i + scope.rng.start.line <= rng.end.line
                    for rng in delete_ranges
                ):
                    continue
                code += line + "\n"
            return code

    return None
