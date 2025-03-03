import pytest
import pathlib

from . import (
    Repository,
    repo_skeleton,
    extract_full_defs,
    exemplar,
)

module_root = pathlib.Path(__file__).parent


@pytest.mark.parametrize(
    "filename,expected",
    [
        (
            "analysis/test_fixtures/simple_scopes.py",
            '''class Thing(object):
    """
    Thing is a class
    """
    def foo(self):
        """
        foo is a class method
        """
def top_level_function():
    """
    top_level_function is a function
    """
def func_no_comment():
''',
        ),
        ("analysis/test_fixtures/thing.md", ""),
        ("analysis/test_fixtures/thing.json", ""),
        ("analysis/test_fixtures/thing.yaml", ""),
        ("analysis/test_fixtures/thing.toml", ""),
    ],
)
def test_skeleton(filename, expected):
    r = Repository({filename: open(module_root / filename).read()})
    assert repo_skeleton(r)[filename] == expected


@pytest.mark.parametrize(
    "filename,symbols,expected",
    [
        (
            "analysis/test_fixtures/simple_scopes.py",
            ["Thing.foo"],
            '''class Thing(object):
    """
    Thing is a class
    """

    ivar = 0

    def foo(self):
        """
        foo is a class method
        """
        return self.foo
''',
        ),
        (
            "analysis/test_fixtures/simple_scopes.py",
            ["top_level_function"],
            '''def top_level_function():
    """
    top_level_function is a function
    """
    pass
''',
        ),
        (
            "analysis/test_fixtures/slice.py",
            ["Thing.do_thing"],
            """class Thing(object):
    x: int
    def do_thing(self, y: int):
        return self.x + y
""",
        ),
        (
            "analysis/test_fixtures/slice.py",
            ["Thing.__init__", "Thing.do_thing"],
            """class Thing(object):
    x: int
    def __init__(self, x: int, optional_var: int = 0):
        self.x = x
    def do_thing(self, y: int):
        return self.x + y
""",
        ),
    ],
)
def test_extract_def(filename, symbols, expected):
    assert (
        extract_full_defs(filename, open(module_root / filename).read(), symbols)[0]
        == expected
    )


@pytest.mark.parametrize(
    "filename,symbol,expected",
    [
        (
            "analysis/test_fixtures/slice.py",
            "Foo.bar",
            """def slice_out_baz():
    x = Foo()
    z = x.bar(1)
    return z
""",
        ),
        (
            "analysis/test_fixtures/slice.py",
            "Thing",
            """def make_a_thing():
    x = Thing(1337, optional_var=42)
    x.do_thing(123)
""",
        ),
    ],
)
def test_exemplar(filename, symbol, expected):
    r = Repository({filename: open(module_root / filename).read()})
    assert exemplar(r, filename, symbol) == expected
