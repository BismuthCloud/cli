from dataclasses import dataclass
from typing import Tuple
from functools import total_ordering
import tree_sitter


@total_ordering
class Location(object):
    # TODO: store byte offset?

    # Both line and col are 0-indexed
    line: int
    col: int

    def __init__(self, line, col):
        self.line = line
        self.col = col

    def __repr__(self):
        return f"<Location {self.line}:{self.col}>"

    @staticmethod
    def from_ts_point(pt: Tuple[int, int]) -> "Location":
        return Location(pt[0], pt[1])

    @staticmethod
    def from_dict(d) -> "Location":
        return Location(int(d["line"]), int(d["col"]))

    def to_dict(self):
        return {
            "line": self.line + 1,
            "col": self.col,
        }

    def __eq__(self, other):
        return (self.line, self.col) == (other.line, other.col)

    def __ne__(self, other):
        return (self.line, self.col) != (other.line, other.col)

    def __lt__(self, other):
        return (self.line, self.col) < (other.line, other.col)

    def __hash__(self):
        return hash((self.line, self.col))

    def line_align(self) -> "Location":
        if self.col == 0:
            return self
        else:
            return Location(self.line + 1, 0)


@total_ordering
class Range(object):
    start: Location
    end: Location

    def __init__(self, start, end):
        self.start = start
        self.end = end

    def __repr__(self):
        return f"<Range from {self.start.line}:{self.start.col}-{self.end.line}:{self.end.col}>"

    @staticmethod
    def from_ts_node(node: tree_sitter.Node) -> "Range":
        return Range(
            Location.from_ts_point(node.start_point),
            Location.from_ts_point(node.end_point),
        )

    @staticmethod
    def from_dict(d) -> "Range":
        return Range(Location.from_dict(d["start"]), Location.from_dict(d["end"]))

    def to_dict(self):
        return {
            "start": self.start.to_dict(),
            "end": self.end.to_dict(),
        }

    def __eq__(self, other):
        return isinstance(other, Range) and (self.start, self.end) == (
            other.start,
            other.end,
        )

    def __ne__(self, other):
        return (self.start, self.end) != (other.start, other.end)

    def __lt__(self, other):
        if isinstance(other, Range):
            return self.start < other.start
        elif isinstance(other, Location):
            return self.start < other
        raise ValueError()

    def __hash__(self):
        return hash((self.start, self.end))

    def is_subset(self, other: "Range") -> bool:
        """
        Returns whether this range is a subset of (i.e. fully included in) the other range.
        """
        return other.start <= self.start and other.end >= self.end

    def line_align(self) -> "Range":
        """
        Return a new range that aligns with line boundaries.
        If the end column is non-zero, the entire end line is included.
        """
        if self.end.col == 0:
            return Range(Location(self.start.line, 0), Location(self.end.line, 0))
        else:
            return Range(Location(self.start.line, 0), Location(self.end.line + 1, 0))


@dataclass
class Block(object):
    rng: Range
    indent: str
