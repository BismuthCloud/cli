from fnmatch import fnmatch


def path_matches(path_str: str, patterns: list[str]) -> bool:
    """
    Match a path against a list of glob patterns, supporting ** for recursive matching.
    Returns True if the path matches any of the patterns.

    Features:
    - Supports ** for matching zero or more directory levels
    - Supports * for matching within path segments
    - Supports ? for matching single characters
    - Case-sensitive matching
    - Forward slashes and backslashes are treated equivalently

    Args:
        path_str: The path to check as a string
        patterns: List of glob patterns to match against

    Returns:
        bool: True if path matches any pattern, False otherwise

    Examples:
        >>> match_path_against_globs('foo/bar/baz.txt', ['foo/**/baz.txt'])
        True
        >>> match_path_against_globs('foo/bar/baz.txt', ['foo/*.txt'])
        False
        >>> match_path_against_globs('foo/bar/baz.txt', ['foo/**/*.txt'])
        True
    """
    # Normalize path separators
    path_str = path_str.replace("\\", "/")

    # Convert path to segments
    path_parts = [p for p in path_str.split("/") if p]

    def match_pattern(pattern: str) -> bool:
        # Normalize pattern separators
        pattern = pattern.replace("\\", "/")

        # Split pattern into segments
        pattern_parts = [p for p in pattern.split("/") if p]

        def match_segments(path_idx: int, pattern_idx: int) -> bool:
            # Base cases
            if pattern_idx >= len(pattern_parts):
                return path_idx >= len(path_parts)
            if path_idx >= len(path_parts):
                # Check if remaining patterns are all '**'
                return all(p == "**" for p in pattern_parts[pattern_idx:])

            pattern_segment = pattern_parts[pattern_idx]
            path_segment = path_parts[path_idx]

            if pattern_segment == "**":
                # Try matching zero or more segments
                for next_idx in range(path_idx, len(path_parts) + 1):
                    if match_segments(next_idx, pattern_idx + 1):
                        return True
                return False

            # Regular glob matching for current segment
            if not fnmatch(path_segment, pattern_segment):
                return False

            # Move to next segments
            return match_segments(path_idx + 1, pattern_idx + 1)

        return match_segments(0, 0)

    return any(match_pattern(pattern) for pattern in patterns)
