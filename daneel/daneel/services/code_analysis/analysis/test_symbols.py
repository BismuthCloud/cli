import pytest
import pathlib
from .source_file import SourceFile
from .symbols import symbol_ranges


module_root = pathlib.Path(__file__).parent

@pytest.mark.parametrize("filename", [
    "test_fixtures/simple_symbols.js",
    "test_fixtures/simple_symbols.py",
])
def test_symbols(filename):
    with open(module_root / filename, 'rb') as f:
        sf = SourceFile(filename, f.read())
        actual_symbols = set(sf.content_at(rng).decode('utf-8') for rng in symbol_ranges(sf))
        assert actual_symbols == set(["foo", "a", "b", "x"])
