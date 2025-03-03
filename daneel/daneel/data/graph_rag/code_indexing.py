import logging

from daneel.data.graph_rag.graph import KGNode, KGNodeType
from daneel.services.code_analysis.analysis.scopes import ScopeType, nested_scopes
from daneel.services.code_analysis.analysis.source_file import SourceFile

logger = logging.getLogger(__name__)


def ast_parse(
    files: dict[str, str]
) -> tuple[list[KGNode], list[str], list[tuple[str, str, str]]]:
    """
    Parse the given files into KGNodes + contents using AST based indexing only.
    Returns nodes, contents, and list of edges (node symbol, parent symbol, edge type).
    """
    nodes = []
    contents = []
    # node symbol, parent symbol, edge type
    edges: list[tuple[str, str, str]] = []

    unknown_ctr = 0

    def get_unknown_ctr():
        nonlocal unknown_ctr
        unknown_ctr += 1
        return f"unknown_{unknown_ctr}"

    for file_name, file_content in files.items():
        try:
            sf = SourceFile(file_name, file_content.encode("utf-8"))

        except:
            logger.info(f"error reading {file_name}")
            continue

        file_lines = file_content.splitlines()

        try:
            file_scope = nested_scopes(sf)
        except:
            logger.exception(f"error parsing {file_name}")
            file_scope = {"children": []}

        if not file_scope["children"]:
            logger.debug(f"no scopes in {file_name}, indexing by chunking")
            # Assume an unsupported language, so break file contents into 50 line chunks and index those
            # TODO: break on nearest whitespace change?
            for i in range(0, len(file_lines), 50):
                nodes.append(
                    KGNode(
                        type=KGNodeType.FILE,
                        symbol=file_name.rsplit(".", 1)[0].replace("/", "."),
                        file_name=file_name,
                        line=i,
                        end_line=min(i + 50, len(file_lines)),
                    )
                )
                contents.append(
                    f"# {file_name}\n" + "\n".join(file_lines[i : i + 50]).strip()
                )

            continue

        file_node = KGNode(
            type=KGNodeType.FILE,
            symbol=file_name.rsplit(".", 1)[0].replace("/", "."),
            file_name=file_name,
            line=0,
        )

        nodes.append(file_node)
        contents.append("")

        def recurse_scope(scopes, parent_node):
            for scope in scopes:
                if scope["type"] == ScopeType.CLASS.name:
                    node = KGNode(
                        type=KGNodeType.CLASS,
                        symbol=scope["name"] or get_unknown_ctr(),
                        file_name=file_name,
                        line=scope["range"]["start"]["line"] - 1,
                    )
                    start_line = scope["range"]["start"]["line"] - 1
                    end_line = scope["range"]["end"]["line"]
                    if scope["range"]["end"]["col"] == 0:
                        end_line -= 1

                    for child_scope in scope["children"]:
                        end_line = min(
                            end_line,
                            child_scope["range"]["start"]["line"] - 1,
                        )

                    edge_type = "class_def"

                elif scope["type"] == ScopeType.FUNCTION.name:
                    node = KGNode(
                        type=KGNodeType.FUNCTION,
                        symbol=scope["name"] or get_unknown_ctr(),
                        file_name=file_name,
                        line=scope["range"]["start"]["line"] - 1,
                    )

                    start_line = scope["range"]["start"]["line"] - 1
                    end_line = scope["range"]["end"]["line"]
                    if scope["range"]["end"]["col"] == 0:
                        end_line -= 1

                    edge_type = "function_def"

                else:
                    recurse_scope(scope["children"], parent_node)
                    continue

                node.end_line = end_line
                node.symbol = parent_node.symbol + "." + node.symbol

                nodes.append(node)
                edges.append((node.symbol, parent_node.symbol, edge_type))

                content = ""
                content += "# " + file_name + "\n"
                content += "# " + node.symbol + "\n"
                content += "\n".join(file_lines[start_line:end_line])
                contents.append(content)

                recurse_scope(
                    scope["children"],
                    node,
                )

        recurse_scope(file_scope["children"], file_node)

    return nodes, contents, edges
