import pytest
import pathlib
from .scopes import scopes, nested_scopes, ScopeType
from .source_file import SourceFile
from .test_util import LooseEqualDict

module_root = pathlib.Path(__file__).parent


@pytest.mark.parametrize(
    "filename,expected",
    [
        (
            "test_fixtures/simple_scopes.js",
            [
                {
                    "type": "FILE",
                },
                {
                    "type": "CLASS",
                    "name": "thing",
                    "comment": {
                        "text": "class thing",
                    },
                    "range": {
                        "start": {"line": 4},
                        "end": {"line": 11},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "constructor",
                    "comment": {
                        "text": "ctor",
                    },
                    "range": {
                        "start": {"line": 8},
                        "end": {"line": 10},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "topLevelFunction",
                    "comment": {
                        "text": "topLevelFunction is a function",
                    },
                    "range": {
                        "start": {"line": 16},
                        "end": {"line": 18},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "arrowFunc",
                    "comment": {
                        "text": "arrow func",
                    },
                    "range": {
                        "start": {"line": 23},
                        "end": {"line": 24},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "anotherFunc",
                    "comment": {
                        "text": "comment using multiple\nprefixed lines",
                    },
                    "range": {
                        "start": {"line": 28},
                        "end": {"line": 30},
                    },
                },
            ],
        ),
        (
            "test_fixtures/simple_scopes.py",
            [
                {
                    "type": "FILE",
                },
                {
                    "type": "CLASS",
                    "name": "Thing",
                    "comment": {
                        "text": "Thing is a class",
                    },
                    "range": {
                        "start": {"line": 1},
                        "end": {"line": 12},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "foo",
                    "comment": {
                        "text": "foo is a class method",
                    },
                    "range": {
                        "start": {"line": 5},
                        "end": {"line": 9},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "top_level_function",
                    "comment": {
                        "text": "top_level_function is a function",
                    },
                    "range": {
                        "start": {"line": 11},
                        "end": {"line": 15},
                    },
                },
                {
                    "type": "FUNCTION",
                    "name": "func_no_comment",
                    "range": {
                        "start": {"line": 17},
                        "end": {"line": 18},
                    },
                },
            ],
        ),
    ],
)
def test_scopes(filename, expected):
    with open(module_root / filename, "rb") as f:
        sf = SourceFile(filename, f.read())
        assert [LooseEqualDict(e) for e in expected] == [
            scope.to_dict()
            for scope in scopes(sf)
            if scope.typ != ScopeType.BLOCK_STATEMENT
            and scope.rng.start.line != scope.rng.end.line
        ]


@pytest.mark.parametrize(
    "filename,expected",
    [
        (
            "test_fixtures/simple_scopes.py",
            {
                "type": "FILE",
                "children": [
                    {
                        "type": "CLASS",
                        "name": "Thing",
                        "comment": {
                            "text": "Thing is a class",
                        },
                        "range": {
                            "start": {"line": 1},
                            "body_start": {"line": 2},
                            "end": {"line": 9},
                        },
                        "children": [
                            {
                                "type": "FUNCTION",
                                "name": "foo",
                                "comment": {
                                    "text": "foo is a class method",
                                },
                                "range": {
                                    "start": {"line": 5},
                                    "body_start": {"line": 6},
                                    "end": {"line": 9},
                                },
                            }
                        ],
                    },
                    {
                        "type": "FUNCTION",
                        "name": "top_level_function",
                        "comment": {
                            "text": "top_level_function is a function",
                        },
                        "range": {
                            "start": {"line": 11},
                            "body_start": {"line": 12},
                            "end": {"line": 15},
                        },
                    },
                    {
                        "type": "FUNCTION",
                        "name": "func_no_comment",
                        "range": {
                            "start": {"line": 17},
                            "body_start": {"line": 18},
                            "end": {"line": 18},
                        },
                    },
                ],
            },
        ),
    ],
)
def test_nested_scopes(filename, expected):
    allowed_types = [
        ScopeType.NAMESPACE,
        ScopeType.CLASS,
        ScopeType.FUNCTION,
    ]  # STATEMENT purposefully left out
    cb = lambda node: node["range"]["start"]["line"] != node["range"]["end"][
        "line"
    ] and node["type"] in [t.name for t in allowed_types]

    with open(module_root / filename, "rb") as f:
        sf = SourceFile(filename, f.read())
        nested = nested_scopes(sf, cb)
        assert LooseEqualDict(expected) == nested
