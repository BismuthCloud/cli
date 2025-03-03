from .location import Location, Range
from .source_file import SourceFile
from .symbols import symbol_ranges
from .scopes import ScopeType, Scope, scopes, nested_scopes, enclosing_scopes, collapse_nested_scopes
from .repo import Repository
from .slicing import do_slice