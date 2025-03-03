from typing import Optional, Type
import tree_sitter
from tree_sitter_language_pack import get_parser

from .langs import *
from .location import Range


class UnknownExtensionException(ValueError):
    pass


class SourceFile(object):
    EXT_MAP = {
        ".py": Python,
        ".css": CSS,
        ".js": JavaScript,
        ".jsx": JavaScript,
        ".ts": TypeScript,
        ".tsx": TSX,
        ".toml": TOML,
        ".md": Markdown,
        ".ex": Elixir,
        ".exs": Elixir,
        ".htm": HTML,
        ".html": HTML,
        ".hcl": Hcl,
        ".json": JSON,
        ".jsdoc": Jsdoc,
        ".yaml": YAML,
        ".yml": YAML,
        ".c": C,
        ".h": C,
        ".cpp": CPP,
        ".hpp": CPP,
        ".cc": CPP,
        ".hh": CPP,
        ".cxx": CPP,
        ".hxx": CPP,
        ".cs": CSharp,
        ".go": Go,
        ".java": Java,
        ".php": PHP,
        ".rb": Ruby,
        ".rs": Rust,
        ".sh": BASH,
        ".scss": SCSS,
        ".svelte": Svelte,
        # Bulk
        # Actionscript
        ".as": Actionscript,
        # Ada
        ".ada": Ada,
        ".adb": Ada,
        ".ads": Ada,
        # Agda
        ".agda": Agda,
        # Arduino
        ".ino": Arduino,
        # Assembly
        ".asm": Asm,
        ".s": Asm,
        ".nasm": Asm,
        # Astro
        ".astro": Astro,
        # Beancount
        ".beancount": Beancount,
        # BibTeX
        ".bib": Bibtex,
        # Bicep
        ".bicep": Bicep,
        # BitBake
        ".bb": Bitbake,
        ".bbclass": Bitbake,
        ".bbappend": Bitbake,
        # Cairo
        ".cairo": Cairo,
        # Cap'n Proto
        ".capnp": Capnp,
        # Chatito
        ".chatito": Chatito,
        # Clarity
        ".clar": Clarity,
        # Clojure
        ".clj": Clojure,
        ".cljs": Clojure,
        ".cljc": Clojure,
        ".edn": Clojure,
        # CMake
        ".cmake": Cmake,
        "CMakeLists.txt": Cmake,
        # Common Lisp
        ".lisp": Commonlisp,
        ".cl": Commonlisp,
        # CPON
        ".cpon": Cpon,
        # CSV
        ".csv": Csv,
        # CUDA
        ".cu": Cuda,
        ".cuh": Cuda,
        # D
        ".d": D,
        # Dart
        ".dart": Dart,
        # Dockerfile
        "Dockerfile": Dockerfile,
        # Graphviz
        ".dot": Dot,
        ".gv": Dot,
        # Doxygen
        ".dox": Doxygen,
        # Emacs Lisp
        ".el": Elisp,
        # Elm
        ".elm": Elm,
        # Embedded Template
        ".ejs": Embeddedtemplate,
        # Erlang
        ".erl": Erlang,
        ".hrl": Erlang,
        # Fennel
        ".fnl": Fennel,
        # FIRRTL
        ".fir": Firrtl,
        # Fish
        ".fish": Fish,
        # Fortran
        ".f": Fortran,
        ".f90": Fortran,
        ".f95": Fortran,
        ".f03": Fortran,
        ".f08": Fortran,
        # Func
        ".func": Func,
        # GDScript
        ".gd": Gdscript,
        # Git files
        ".gitattributes": Gitattributes,
        ".gitignore": Gitignore,
        "COMMIT_EDITMSG": Gitcommit,
        # Gleam
        ".gleam": Gleam,
        # GLSL
        ".glsl": Glsl,
        ".vert": Glsl,
        ".frag": Glsl,
        # GN
        ".gn": Gn,
        ".gni": Gn,
        # Go module files
        "go.mod": Gomod,
        "go.sum": Gosum,
        # Groovy
        ".groovy": Groovy,
        ".gradle": Groovy,
        # GStreamer
        ".launch": Gstlaunch,
        # Hack
        ".hack": Hack,
        ".hh": Hack,
        # Hare
        ".ha": Hare,
        # Haskell
        ".hs": Haskell,
        ".lhs": Haskell,
        # Haxe
        ".hx": Haxe,
        # HEEx (Phoenix Framework)
        ".heex": Heex,
        # HLSL
        ".hlsl": Hlsl,
        # Hypr
        ".hypr": Hyprlang,
        # ISPC
        ".ispc": Ispc,
        # Janet
        ".janet": Janet,
        # Jsonnet
        ".jsonnet": Jsonnet,
        ".libsonnet": Jsonnet,
        # Julia
        ".jl": Julia,
        # Kconfig
        "Kconfig": Kconfig,
        ".kconfig": Kconfig,
        # KDL
        ".kdl": Kdl,
        # Kotlin
        ".kt": Kotlin,
        ".kts": Kotlin,
        # Linker Script
        ".ld": Linkerscript,
        # LLVM
        ".ll": Llvm,
        # Lua
        ".lua": Lua,
        # Luadoc
        ".luadoc": Luadoc,
        # Luau
        ".luau": Luau,
        # Magik
        ".magik": Magik,
        # Make
        "Makefile": Make,
        ".mk": Make,
        # MATLAB
        ".m": Matlab,
        ".matlab": Matlab,
        # Mermaid
        ".mmd": Mermaid,
        ".mermaid": Mermaid,
        # Meson
        "meson.build": Meson,
        "meson_options.txt": Meson,
        # Ninja
        ".ninja": Ninja,
        # Nix
        ".nix": Nix,
        # NQC
        ".nqc": Nqc,
        # Objective-C
        ".m": Objc,
        ".mm": Objc,
        # Odin
        ".odin": Odin,
        # Org mode
        ".org": Org,
        # Pascal
        ".pas": Pascal,
        ".pp": Pascal,
        # PEM
        ".pem": Pem,
        # Perl
        ".pl": Perl,
        ".pm": Perl,
        ".t": Perl,
        # PGN (Chess)
        ".pgn": Pgn,
        # Gettext
        ".po": Po,
        ".pot": Po,
        # Pony
        ".pony": Pony,
        # PowerShell
        ".ps1": Powershell,
        ".psm1": Powershell,
        ".psd1": Powershell,
        # Printf
        ".printf": Printf,
        # Prisma
        ".prisma": Prisma,
        # Properties
        ".properties": Properties,
        # PSV (pipe-separated values)
        ".psv": Psv,
        # Puppet
        ".pp": Puppet,
        # PureScript
        ".purs": Purescript,
        # Python Manifest
        ".manifest": Pymanifest,
        # QL
        ".ql": Ql,
        ".qll": Ql,
        # QML dir
        "qmldir": Qmldir,
        # Tree-sitter Query
        ".scm": Query,
        # R
        ".r": R,
        ".R": R,
        # Racket
        ".rkt": Racket,
        # RBS
        ".rbs": Rbs,
        # re2c
        ".re": Re2c,
        # Readline
        ".inputrc": Readline,
        # Requirements
        "requirements.txt": Requirements,
        # RON (Rusty Object Notation)
        ".ron": Ron,
        # reStructuredText
        ".rst": Rst,
        # Scala
        ".scala": Scala,
        ".sc": Scala,
        # Scheme
        ".scm": Scheme,
        ".ss": Scheme,
        # Slang
        ".slang": Slang,
        # Smali
        ".smali": Smali,
        # Smithy
        ".smithy": Smithy,
        # Solidity
        ".sol": Solidity,
        # SQL
        ".sql": Sql,
        # Squirrel
        ".nut": Squirrel,
        # Starlark
        ".star": Starlark,
        ".bzl": Starlark,
        # Swift
        ".swift": Swift,
        # TableGen
        ".td": Tablegen,
        # Tcl
        ".tcl": Tcl,
        # Test
        ".test": Test,
        # Thrift
        ".thrift": Thrift,
        # TSV
        ".tsv": Tsv,
        # Twig
        ".twig": Twig,
        # Typst
        ".typ": Typst,
        # udev
        ".rules": Udev,
        # Ungrammar
        ".ungram": Ungrammar,
        # uxntal
        ".tal": Uxntal,
        # V
        ".v": V,
        # Verilog
        ".v": Verilog,
        ".sv": Verilog,
        # VHDL
        ".vhd": Vhdl,
        ".vhdl": Vhdl,
        # Vim script
        ".vim": Vim,
        ".vimrc": Vim,
        # Vue
        ".vue": Vue,
        # WGSL
        ".wgsl": Wgsl,
        # XCompose
        ".XCompose": Xcompose,
        # XML
        ".xml": Xml,
        ".xaml": Xml,
        ".svg": Xml,
        ".plist": Xml,
        # Yuck
        ".yuck": Yuck,
        # Zig
        ".zig": Zig,
        ".txt": GenericText,
    }

    filename: str
    lang: Language
    contents: bytes
    _tree: Optional[tree_sitter.Tree]

    def __init__(
        self, filename: str, contents: bytes, lang: Optional[Type[Language]] = None
    ):
        self.filename = filename
        self.contents = contents
        if lang is None:
            for ext, lang_cls in self.EXT_MAP.items():
                if filename.endswith(ext):
                    self.lang = lang_cls(self)
                    break
            else:
                raise UnknownExtensionException()
        else:
            self.lang = lang(self)

        self._tree = None
        self._whitespace_pattern = None  # Cache for whitespace pattern analysis

    @staticmethod
    def from_fs(filename: str) -> "SourceFile":
        with open(filename, "rb") as f:
            return SourceFile(filename, f.read())

    def parser(self):
        if self.lang.TREE_SITTER_LANG_NAME is None:
            return None
        return get_parser(self.lang.TREE_SITTER_LANG_NAME)

    def get_ast_string(self):
        def _node_to_string(node, indent=""):
            result = f"{indent}{node.type}: {self.content_at(Range.from_ts_node(node))}"
            for child in node.children:
                result += "\\n" + _node_to_string(child, indent + "  ")
            return result

        return _node_to_string(self.tree.root_node)

    def print_ast(self):
        print(self.get_ast_string())

    class WhitespacePattern:
        """Stores the detected whitespace patterns for a file."""

        def __init__(self):
            self.indent_type = "space"  # or "tab"
            self.indent_size = 4  # number of spaces or 1 for tabs
            self.line_ending = "\n"  # or "\r\n"

    def analyze_whitespace_pattern(self) -> WhitespacePattern:
        """
        Analyzes the file content to determine whitespace patterns.
        Returns a WhitespacePattern object with the detected settings.
        Uses caching to avoid re-analyzing unchanged content.
        """
        if self._whitespace_pattern is not None:
            return self._whitespace_pattern

        pattern = self.WhitespacePattern()

        # Convert bytes to string for line analysis
        if not self.contents:
            return self.WhitespacePattern()

        content = self.contents.decode("utf-8")
        lines = content.splitlines(keepends=True)

        # Detect line endings
        if lines and "\r\n" in lines[0]:
            pattern.line_ending = "\r\n"

        # Count indentation types
        space_indents = 0
        tab_indents = 0
        space_sizes = {}  # Track common space indent sizes

        for line in lines:
            if not line.strip():  # Skip empty lines
                continue

            # Count leading whitespace
            leading_spaces = len(line) - len(line.lstrip(" "))
            leading_tabs = len(line) - len(line.lstrip("\t"))

            if leading_spaces > 0:
                space_indents += 1
                if leading_spaces > 0:
                    space_sizes[leading_spaces] = space_sizes.get(leading_spaces, 0) + 1
            if leading_tabs > 0:
                tab_indents += 1

        # Determine predominant indentation type
        if tab_indents > space_indents:
            pattern.indent_type = "tab"
            pattern.indent_size = 1
        elif space_sizes:
            minimum_indent = min(space_sizes.keys())

            while True:
                if space_sizes[minimum_indent] < 5:
                    space_sizes.pop(minimum_indent, None)

                    if len(space_sizes.keys()) == 0:
                        break

                    minimum_indent = min(space_sizes.keys())
                else:
                    break

            pattern.indent_size = minimum_indent

        # Cache the pattern
        self._whitespace_pattern = pattern
        return pattern

    def normalize_whitespace(self, lines: list[str], pattern: WhitespacePattern) -> str:
        """
        Normalizes the whitespace in a line to match the file's pattern.
        Preserves internal spacing and only adjusts leading indentation.

        Args:
            line: The line to normalize
            indent_level: The target indentation level (0-based)

        Returns:
            The line with normalized whitespace
        """

        normalized = []

        if lines == []:
            return []

        for line in lines:
            if not line.strip():  # Preserve empty or whitespace-only lines as-is
                normalized.append(line)

                continue

            indent_level = 0
            if line.strip() and (line[0] == " " or line[0] == "\t"):
                indent_level = len(line) - len(line.lstrip())
                indent_level = indent_level // (
                    pattern.indent_size if pattern.indent_type == "space" else 1
                )

            content = line.lstrip("\t ")  # Remove existing indentation

            # Calculate new indentation
            if pattern.indent_type == "tab":
                indentation = "\t" * indent_level
            else:
                indentation = " " * (pattern.indent_size * indent_level)

            normalized.append(indentation + content)

        return normalized

    @property
    def tree(self) -> tree_sitter.Tree:
        if self._tree is None:
            parser = self.parser()
            if parser:
                self._tree = parser.parse(self.contents)
            else:
                # Dummy tree for non-parsable files
                self._tree = get_parser("comment").parse(b"")

        return self._tree

    def clear_whitespace_pattern_cache(self):
        """Clear the cached whitespace pattern. Should be called when file content changes."""
        self._whitespace_pattern = None

    def content_at(self, rng: Range) -> bytes:
        """
        Get the content of the file within the given range.
        The start is inclusive, the end is exclusive, so an ending location of (1,0)
        will return the entire first line, including newline.
        """
        lines = self.contents.splitlines()
        out_lines = []
        for lineno in range(rng.start.line, rng.end.line + 1):
            start = 0
            end = None

            if lineno == rng.start.line:
                start = rng.start.col

            if lineno == rng.end.line:
                end = rng.end.col
                if end == 0:
                    out_lines.append(b"")
                    break
            # If file contents are 'a\n', it's useful to be able to reference the
            # empty contents of the second line with Range((1, 0), (2, 0)),
            # so check for that here. Col must be 0 though.
            if lineno == len(lines):
                assert rng.end.col == 0
                break

            out_lines.append(lines[lineno][start:end])

        return b"\n".join(out_lines)
