from typing import Optional, List


class CommentFormatter(object):
    def format_comment(self, comment: str) -> str:
        raise NotImplementedError()

    def applys(self, comment: str) -> bool:
        raise NotImplementedError()

    def unformat_comment(self, comment: str) -> str:
        raise NotImplementedError()


class CommentFormatterChain(CommentFormatter):
    formatters: List[CommentFormatter]

    def __init__(self, formatters):
        self.formatters = formatters

    def format_comment(self, comment: str) -> str:
        return self.formatters[0].format_comment(comment)

    def unformat_comment(self, comment: str) -> str:
        for formatter in self.formatters:
            if formatter.applys(comment):
                return formatter.unformat_comment(comment)
        return comment


class LinePrefixComment(CommentFormatter):
    """
    LinePrefixComment is used for langs where each line of a comment should have a prefix.
    E.g. JS: //
    """

    prefix: str

    def __init__(self, prefix: str):
        self.prefix = prefix

    def format_comment(self, comment: str) -> str:
        return "\n".join(self.prefix + line for line in comment.splitlines())

    def applys(self, comment: str) -> bool:
        return all(line.startswith(self.prefix) for line in comment.splitlines())

    def unformat_comment(self, comment: str) -> str:
        should_use = comment.splitlines()[0].strip().startswith(self.prefix)
        new = ""
        for line in comment.splitlines():
            line = line.strip()
            if should_use and line.startswith(self.prefix):
                line = line[len(self.prefix) :]
            new += line + "\n"
        return new[:-1]


class BlockComment(CommentFormatter):
    """
    BlockComment is used for langs where a comment has a start (and optional end) token
    E.g. Ruby starts with `##`
    """

    start: str
    end: Optional[str]

    def __init__(self, start: str, end: Optional[str] = None):
        self.start = start
        self.end = end

    def format_comment(self, comment: str) -> str:
        return self.start + comment + (self.end if self.end else "")

    def applys(self, comment: str) -> bool:
        if not comment.startswith(self.start):
            return False
        if self.end and not comment.endswith(self.end):
            return False
        return True

    def unformat_comment(self, comment: str) -> str:
        if comment.startswith(self.start):
            comment = comment[len(self.start) :]
        if self.end and comment.endswith(self.end):
            comment = comment[: -len(self.end)]
        return comment.strip()


class BlockAndLinePrefixComment(CommentFormatter):
    """
    All of the above!
    """

    block: BlockComment
    prefix: LinePrefixComment

    def __init__(
        self, block_start: str, line_prefix: str, block_end: Optional[str] = None
    ):
        self.block = BlockComment(block_start, block_end)
        self.prefix = LinePrefixComment(line_prefix)

    def format_comment(self, comment: str) -> str:
        return self.block.format_comment(self.prefix.format_comment(comment))

    def applys(self, comment: str) -> bool:
        return self.block.applys(comment)

    def unformat_comment(self, comment: str) -> str:
        return self.prefix.unformat_comment(self.block.unformat_comment(comment))
