from .scopes import nested_scopes, scopes
from .source_file import SourceFile
import json


def ts_dump(ext: str, code: str) -> str:
    sf = SourceFile(f"test.{ext}", code.encode("utf-8"))
    print(sf.tree.root_node.sexp())


def scopes_dump(ext: str, code: str) -> str:
    sf = SourceFile(f"test.{ext}", code.encode("utf-8"))
    print(json.dumps(nested_scopes(sf), indent=2))


if __name__ == "__main__":
    import sys
    import pathlib

    fn = pathlib.Path(sys.argv[1])
    ts_dump(fn.suffix[1:], fn.read_text())
    scopes_dump(fn.suffix[1:], fn.read_text())
