"""Visualization module for ACI (Agent Code Interaction) functionality.

This module handles the visualization of agent interactions with code, including
file viewing, diagnostics, and analysis results. It maintains the exact same
visual feedback behavior as the original implementation while providing better
code organization.
"""

from typing import List, Optional, Tuple

from daneel.constants import LANGUAGE_CONSTRAINTS
from daneel.utils.websockets import ACIMessage, WSMessage, WSMessageType


class ACIVisualizer:
    """Handles visualization of ACI interactions and feedback."""

    @staticmethod
    def get_status_box(status: str = "normal") -> Tuple[str, str]:
        """Get the appropriate status box borders based on status.

        Args:
            status: The status to use for box styling ("error", "success", "warning", or "normal")

        Returns:
            Tuple containing top and bottom box borders
        """
        if status == "error":
            return "╔═╡!╞═", "╚═╡!╞═"
        elif status == "success":
            return "╔═╡✓╞═", "╚═╡✓╞═"
        elif status == "warning":
            return "╔═╡?╞═", "╚═╡?╞═"
        else:
            return "╔════", "╚════"

    @staticmethod
    def format_header(title: str, status: str = "normal", width: int = 50) -> str:
        """Format a section header with appropriate styling.

        Args:
            title: The title text for the header
            status: Status for box styling
            width: Total width of the header

        Returns:
            Formatted header string
        """
        top, _ = ACIVisualizer.get_status_box(status)
        return f"{top}═ {title} {'═' * (width - len(title) - 7)}╗"

    @classmethod
    def generate_viewer_state(
        cls,
        input_task: str,
        files_with_id: List[str],
        fn: str,
        lines_above: int,
        lines_below: int,
        new_lines: List[str],
        analysis_lines: List[str],
        test_output: str,
        system_analysis: str,
        available_tools: str = "",
        thoughts: str = "",
        turns_remaining: int = 0,
    ) -> str:
        """Generate the complete viewer state visualization.

        Args:
            input_task: The current task being worked on
            files_with_id: List of files with their IDs
            fn: Current file name
            lines_above: Number of lines above current view
            lines_below: Number of lines below current view
            new_lines: Lines of code being displayed
            analysis_lines: Code analysis results
            test_output: Output from test runs
            system_analysis: System analysis results

        Returns:
            Complete formatted viewer state string
        """

        content = (
            # Task always present - normal box
            f"{cls.format_header('TASK')}\n"
            f"│ {input_task}\n"
            "╚════════════════════════════════════════════════╝\n\n"
            # Language and scenario specific constraints
            + f"{cls.format_header('CONSTRAINTS')}\n"
            + f"│ {LANGUAGE_CONSTRAINTS}\n"
            + "╚════════════════════════════════════════════════╝\n\n"
            # Files navigation
            + f"{cls.format_header('FILES')}\n"
            + "\n".join(f"│ {file_id}" for file_id in files_with_id)
            + "\n╚════════════════════════════════════════════════╝\n\n"
            # Subtask (Communication between subsystems)
            + f"{cls.format_header('SUBTASK RESULT')}\n"
            + f"│ {thoughts}\n"
            + "╚════════════════════════════════════════════════╝\n\n"
            # Current file view with line counts
            + f"{cls.format_header(f'VIEWING: {fn}')}\n"
            + f"│ Lines above: {lines_above} | Lines below: {lines_below}\n"
            + "├────────────────────────────────────────────────┤\n"
            + "\n".join(
                f"│ {lines_above+i:4d} │{line}" for i, line in enumerate(new_lines)
            )
            + (
                "\n╠═══════════════ EOF ══════════════════╣\n"
                if lines_below == 0
                else "\n"
            )
            + "╚════════════════════════════════════════════════╝\n\n"
            # Diagnostics section
            + f"{cls.format_header('DIAGNOSTICS', 'error' if analysis_lines or 'fail' in test_output.lower() else 'success')}\n"
            + "┌──────────────── Code Analysis ─────────────────┐\n"
            + (
                ("\n".join(f"│{line}" for line in analysis_lines) + "\n")
                if analysis_lines
                else "│ No issues found\n"
            )
            + "└────────────────────────────────────────────────┘\n"
            + "┌──────────────── Test Results ──────────────────┐\n"
            + f"│{test_output}\n"
            + "└────────────────────────────────────────────────┘\n"
            + "╚════════════════════════════════════════════════╝\n"
            # Analysis when present - uses warning/success based on findings
            + (
                f"{cls.format_header('ANALYSIS', 'warning' if 'issue' in system_analysis.lower() else 'success')}\n"
                f"│{system_analysis}\n"
                "╚════════════════════════════════════════════════╝\n\n"
                if system_analysis
                else ""
            )
            # Tools
            + f"{cls.format_header('AVAILABLE_TOOLS')}\n"
            + f"│ {available_tools}\n"
            + "╚════════════════════════════════════════════════╝\n\n"
            # Turns remaining
            + f"{cls.format_header('Turns Remaining')}\n"
            + f"│ {turns_remaining}\n"
            + "╚════════════════════════════════════════════════╝\n\n"
        )
        return content

    @classmethod
    async def send_visualization_message(
        cls,
        send_message_callback,
        message_type: ACIMessage.Action,
        status: str,
        active_file: Optional[str] = None,
        files: Optional[List[str]] = None,
        new_contents: Optional[str] = None,
        scroll_position: Optional[int] = None,
        test_output: Optional[str] = None,
        changed_range: Optional[tuple[int, int]] = None,
    ):
        """Send a visualization message through the websocket.

        Args:
            send_message_callback: Callback function to send messages
            message_type: Type of message being sent
            status: Status message to display
            active_file: Currently active file
            files: List of open files
            new_contents: New file contents
            scroll_position: Current scroll position
            test_output: Test execution output
            changed_range: Range of lines that were changed
        """
        message = ACIMessage(
            action=message_type,
            status=status,
            active_file=active_file,
            files=files,
            new_contents=new_contents,
            scroll_position=scroll_position,
            test_output=test_output,
            changed_range=changed_range,
        )

        await send_message_callback(
            WSMessage(
                type=WSMessageType.ACI,
                aci=message,
            )
        )
