from difflib import SequenceMatcher
import json
from asimov.asimov_base import AsimovBase
import enum
from daneel.data.graph_rag import GraphRag
from daneel.data.graph_rag.graph import KGNodeType
from daneel.data.postgres.models import ChatSessionEntity
from daneel.data.file_rpc import FileRPC
from daneel.executors.aci.visualization import ACIVisualizer
import logging
from daneel.executors.aci.prompts import *
from pathlib import Path

from asimov.graph import AgentModule
from asimov.services.inference_clients import InferenceClient
from difflib import SequenceMatcher
from gasp import WAILGenerator

import os
from pydantic import Field, PrivateAttr
from jinja2 import Template

from typing import Any, Awaitable, Callable, Iterable, Optional, Literal
from asimov.caches.cache import Cache
import textwrap

from daneel.utils import find_text_chunk
from daneel.services.code_analysis.analysis.source_file import (
    SourceFile,
    UnknownExtensionException,
)
from daneel.services.code_analysis import repo_skeleton, Repository
import math

from daneel.utils.tracing import trace_output
from daneel.utils.websockets import (
    ACIMessage,
    WSMessage,
    WSMessageType,
    FileEdit,
    FileCreate,
    FileDelete,
    null_recv_callback,
    null_send_callback,
)

from daneel.executors.aci.aci_types import *

LINES_IN_VIEW = 500
LINES_IN_VIEW_CONSTRAINED = 2000
RECURSION_LIMIT = 1

GIT_HOST = os.environ.get("GIT_HOST", "localhost:8765")


class ACIExecutionMode(enum.Enum):
    SINGLE = "single"


class ACI(AsimovBase):
    cache: Cache
    viewer_state: str = Field(default="")
    tool_executors: dict["str", AgentModule] = Field(default_factory=dict)
    send_message_callback: Callable[[WSMessage], Awaitable[None]]
    recv_message_callback: Callable[[], Awaitable[WSMessage]]
    file_rpc: FileRPC
    driver_mode: ACIMode = ACIMode.CONSTRAINED
    initial_turns: int = 3
    interactive_mode: bool = Field(default=False)
    recursion_depth: int = Field(default=0)
    finalized: bool = Field(default=False)
    mode: ACIExecutionMode = Field(default=ACIExecutionMode.SINGLE)
    unstructured: bool = Field(default=False)
    _input_task: str = PrivateAttr()
    _step_count: int = PrivateAttr()
    _pinned_files: dict[str, str] = PrivateAttr(default_factory=dict)
    _turns_remaining: int = PrivateAttr(default=0)
    _visualizer: Optional[ACIVisualizer] = PrivateAttr(default=None)
    _test_failure_count: int = PrivateAttr(default=0)
    _attempted_finalize: bool = PrivateAttr(default=False)
    _last_ran_tests: int = PrivateAttr(default=0)
    _mode: ACIMode = PrivateAttr(default=ACIMode.CONSTRAINED)
    _logger: logging.Logger = PrivateAttr()
    _schema_parser: WAILGenerator = PrivateAttr()

    tool_schemas: dict[str, dict[str, Any]] = {
        "switch_file": {
            "name": "switch_file",
            "description": "Close the specific file to viewing and editing when you are finished making changes to it. Only close the file if you're sure you are done with the file such as after you've made all your changes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_id": {
                        "type": "number",
                        "description": "The id of the file you want to switch to as shown next to the file path between the <files> tags.",
                    }
                },
                "required": ["file_id"],
            },
        },
        "create_files": {
            "name": "create_files",
            "description": "Creates one or many new files with the contents you specify opening all files created for editing. This operation is useful for generating new files or templates to support the completion of a task. The last file created will be set to the active file for editing.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "creates": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "thoughts": {
                                    "type": "string",
                                    "description": "Your thoughts about the change you are making given the state of the system. These should detail why this change is moving you closer to completing the task as stated in the users prompt.",
                                },
                                "step": {
                                    "type": "string",
                                    "description": "An english description of the change you are making. This helps document the purpose of the file creation.",
                                },
                                "file": {
                                    "type": "string",
                                    "description": "The name of the file you are creating.",
                                },
                                "content": {
                                    "type": "string",
                                    "description": "The contents that will be written to the file.",
                                },
                            },
                            "required": [
                                "thoughts",
                                "file",
                                "step",
                                "content",
                            ],
                        }
                    },
                },
                "required": ["creates"]
            },
        },
        "edit_files": {
            "name": "edit_files",
            "description": "Performs a targeted replacement of specified text within a file or set of files. This operation allows you to identify specific lines of text and replace them with new content while maintaining a file's structure. Each edit is tracked with a unique identifier and includes a human-readable description of the change being made. This operation is useful for making precise modifications to configuration files, source code, or any text-based document where specific lines need to be updated.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": [
                                "thoughts",
                                "file",
                                "lines_to_replace",
                                "step",
                                "replace_text",
                                "id",
                                "file_id",
                            ],
                            "properties": {
                                "thoughts": {
                                    "type": "string",
                                    "description": "Your thoughts about the change you are making given the state of the system. These should detail why this change is moving you closer to completing the task as stated in the users prompt.",
                                },
                                "file_id": {
                                    "type": "number",
                                    "description": "The id of the file you want to edit as shown next to the file path between the <files> tags.",
                                },
                                "step": {
                                    "type": "string",
                                    "description": "An english description of the change you are making. This helps document the purpose of the edit.",
                                },
                                "file": {
                                    "type": "string",
                                    "description": "The name of the file you are editing.",
                                },
                                "lines_to_replace": {
                                    "type": "string",
                                    "description": "The exact content of the lines of text to be replaced. These lines must exist within the content currently in the viewer state. Whitespace and linebreaks must be the same. Do not include the line number.",
                                },
                                "replace_text": {
                                    "type": "string",
                                    "description": "The content of the lines of text doing the replacing. This field is absolutely required and contains the new content that will replace the specified lines. Do not include the line number.",
                                },
                                "id": {
                                    "type": "string",
                                    "description": "A unique id representing the edit. This allows for tracking and referencing specific changes.",
                                },
                            },
                        },
                    },
                },
                "required": [
                    "edits",
                ],
            },
        },
        "delete_files": {
            "name": "delete_files",
            "description": "Delete one or many currently open files. This action is permanent, you must recreate the files if you wish to work on them again. Because this action is destructive you may only delete files that are currently open.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "deletes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": {
                                    "type": "string",
                                    "description": "An english description of the change you are making. This helps document the purpose of the deletion.",
                                },
                                "file_id": {
                                    "type": "number",
                                    "description": "The numeric identifier of the file to delete, as displayed next to the file path within the <files> tags. Each open file has a unique ID that persists throughout the editing session.",
                                },
                            },
                            "required": ["file_id", "step"],
                        } 
                    },
                },
                "required": ["deletes"]
            },
        },
        "close_file": {
            "name": "close_file",
            "description": "Closes a currently open file in the editing session. When the last open file is closed, the editing process automatically terminates. This operation helps manage system resources and maintain a clean workspace by closing files that are no longer needed for the current editing task.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_id": {
                        "type": "number",
                        "description": "The numeric identifier of the file to close, as displayed next to the file path within the <files> tags. Each open file has a unique ID that persists throughout the editing session.",
                    }
                },
                "required": ["file_id"],
            },
        },
        "open_file": {
            "name": "open_file",
            "description": "Opens a file if it is not already open and switches to it in the viewer.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "The path to the file you wish to open, these can be determined through the 'list_files' command.",
                    }
                },
                "required": ["file"],
            },
        },
        "list_files": {
            "name": "list_files",
            "description": "Lists all available files in the project, this will populate the system analysis portion of the viewer with a list of all available files in the project.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "subpath": {
                        "type": "string",
                        "description": "The directory subpath you want to list files under.",
                    }
                },
                "required": [],
            },
        },
        "scroll_down_file": {
            "name": "scroll_down_file",
            "description": "Navigates downward in the currently active file to reveal additional content. This operation is essential for reviewing or analyzing files that are too long to display in a single view. It enables systematic exploration of file contents by moving the viewport forward through the document.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "scroll": {
                        "type": "number",
                        "description": "The number of lines to move downward in the file. Must be a positive integer. Larger values will reveal more content at once, while smaller values allow for more precise navigation.",
                    }
                },
                "required": ["scroll"],
            },
        },
        "scroll_up_file": {
            "name": "scroll_up_file",
            "description": "Navigates upward in the currently active file to reveal previous content. This operation allows for reviewing earlier portions of the file that have scrolled out of view. It's particularly useful when needing to reference or compare content across different sections of the file.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "scroll": {
                        "type": "number",
                        "description": "The number of lines to move upward in the file. Must be a positive integer. Larger values will reveal more previous content at once, while smaller values enable fine-grained navigation.",
                    }
                },
                "required": ["scroll"],
            },
        },
        "finalize": {
            "name": "finalize",
            "description": "Mark the task you're working on as done. Only use this when you're certain your work is finished as it can complete the process.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": [],
            },
        },
    }

    @classmethod
    async def _create_executors(
        cls,
        inference_client: InferenceClient,
        inference_client_factory,
        send_message_callback,
        recv_message_callback,
        aci: "ACI",
    ):
        return {}

    def _template_root(self):
        template_root = Path(__file__).parent / "prompts" / "wail"
        return template_root

    def _unstructured_prompts(self):
        def template_path(path_str):
            template_root = self._template_root()
            return template_root.joinpath(path_str)

        prompts = {
            ACIMode.CONSTRAINED: template_path("constrained.wail"),
        }

        for k,v in prompts.items():
            with open(v, "r") as f:
                content = f.read()
            
            prompts[k] = content

        return prompts

    def prompts(self) -> dict[ACIMode, str]:
        if self.unstructured:
            return self._unstructured_prompts()

        prompts = {
            ACIMode.CONSTRAINED: CONSTRAINED_PROMPT,
        }

        return prompts

    async def send_aci_status(self, status: str):
        await self.send_message_callback(
            WSMessage(
                type=WSMessageType.ACI,
                aci=ACIMessage(
                    action=ACIMessage.Action.STATUS,
                    status=status,
                ),
            )
        )

    async def toggle_build_lint_test_running_tool(self, resp):
        task_config = await self.cache.get("planned_task_config", {})

        task_config["build_lint_test_execution_enabled"] = not task_config[
            "build_lint_test_execution_enabled"
        ]

        await self.cache.set("planned_task_config", task_config)

        state = "ON" if task_config["build_lint_test_execution_enabled"] else "OFF"

        await self.send_aci_status(f"Toggled build+test command running {state}.")

        return f"Toggled build lint test running tool {state}."

    async def toggle_command_running_tool(self, resp):
        task_config = await self.cache.get("planned_task_config", {})

        task_config["command_execution_enabled"] = not task_config[
            "command_execution_enabled"
        ]

        await self.cache.set("planned_task_config", task_config)

        state = "ON" if task_config["command_execution_enabled"] else "OFF"

        await self.send_aci_status(f"Toggled command running {state}.")

        return f"Toggled command running tool {state}."

    def _turn_wrapper(self, action_func):
        def wrapped_fn(*args, **kwargs):
            action = action_func.__name__

            if self._turns_remaining <= 0:
                self._logger.info(
                    f"Bailing out of action {action} due to no turns remaining."
                )
                return "You have run out of turns to complete this task. Call finalize now."

            self._take_turn(action)

            return action_func(*args, **kwargs)

        return wrapped_fn

    async def show_skeleton(self, resp):
        interesting_files = resp["interesting_files"]

        repo = Repository(
            {
                fn: await self.file_rpc.read(fn, overlay_modified=True)
                for fn in interesting_files
            }
        )
        skeleton = repo_skeleton(repo)

        return skeleton

    async def toolsets(self) -> dict[ACIMode, list[Any]]:
        sets = {
            ACIMode.CONSTRAINED: [
                (self.edit_files, self.tool_schemas["edit_files"]),
                (self.create_files, self.tool_schemas["create_files"]),
                (self.delete_files, self.tool_schemas["delete_files"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.finalize, self.tool_schemas["finalize"]),
            ],
        }

        for set, tools in sets.items():
            wrapped_tools = list(map(lambda t: (self._turn_wrapper(t[0]), t[1]), tools))

            sets[set] = wrapped_tools

        return sets

    def _lines_in_view(self):
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            return LINES_IN_VIEW_CONSTRAINED
        else:
            return LINES_IN_VIEW

    def set_input_task(self, task: str):
        self._input_task = task

    async def set_starting_context(self, starting_context: dict[str, list[str]]):
        cache = self.cache
        open_files = []
        # await cache.set("output_modified_files", {})

        if starting_context == {}:
            starting_context["placeholder_file"] = [""]

            async with cache.with_suffix(f"file_edit_selection_placeholder_file"):
                await cache.set("lines_above", 0)
                await cache.set("lines_below", 0)
                await cache.set("index", 0)
                await cache.set(
                    "lines",
                    [""],
                )
            open_files.append("placeholder_file")
        else:
            for fn, starting_slices in starting_context.items():
                contents = await self.file_rpc.read(fn)
                if contents is None:
                    self._logger.error(
                        f"File {fn} not found despite being in starting_context."
                    )
                    continue

                lines = contents.split("\n")
                open_files.append(fn)
                ln_range = None

                # Multiple symbols might be in the same file, just start with the earliest for now.
                if starting_slices:
                    ln_range = find_text_chunk(contents, starting_slices[0].split("\n"))
                    if ln_range is None:
                        self._logger.error(
                            f"Could not find starting slice {starting_slices[0]} in {fn}."
                        )
                if ln_range is None:
                    ln_range = {
                        "start": 0,
                        "end": min(self._lines_in_view(), len(lines)),
                    }

                slice_in_view = lines[ln_range["start"] : ln_range["end"]]

                if len(slice_in_view) > self._lines_in_view():
                    slice_in_view = slice_in_view[: self._lines_in_view()]
                    start = ln_range["start"]
                    end = ln_range["start"] + self._lines_in_view()
                else:
                    view_remaining = self._lines_in_view() - len(slice_in_view)

                    before = view_remaining // 2
                    after = math.ceil(view_remaining / 2)

                    if ln_range["start"] - before <= 0:
                        start = 0
                        end = self._lines_in_view()
                    else:
                        start = max(0, ln_range["start"] - before)
                        end = min(len(lines), ln_range["end"] + after)

                index = end
                lines_above = start
                lines_below = max(0, len(lines) - index)

                async with cache.with_suffix(f"file_edit_selection_{fn}"):
                    await cache.set("lines_above", lines_above)
                    await cache.set("lines_below", lines_below)
                    await cache.set("index", index)
                    await cache.set("lines", lines)

        await cache.set("code_analysis", [])
        await cache.set("test_output", "")
        await cache.set("system_analysis_output", "")
        await cache.set("viewer_open_files", open_files)
        starting_file = list(starting_context.keys())[0]

        await cache.set("active_file", starting_file)

        await self.manipulate(StartAction(), starting_file)

    @classmethod
    async def create(
        cls,
        cache: Cache,
        inference_client: InferenceClient,
        inference_client_factory: Callable[[str], InferenceClient],
        file_rpc: FileRPC,
        send_message_callback: Callable[
            [WSMessage], Awaitable[None]
        ] = null_send_callback,
        recv_message_callback: Callable[[], Awaitable[WSMessage]] = null_recv_callback,
        interactive_mode=False,
        driver_mode=ACIMode.CONSTRAINED,
        initial_turns=3,
        unstructured=False,
    ) -> "ACI":
        if send_message_callback is None:

            async def _send_message_callback(message):
                pass

            send_message_callback = _send_message_callback

        instance = cls(
            cache=cache,
            viewer_state="",
            tool_executors={},
            send_message_callback=send_message_callback,
            recv_message_callback=recv_message_callback,
            interactive_mode=interactive_mode,
            file_rpc=file_rpc,
            driver_mode=driver_mode,
            initial_turns=initial_turns,
            unstructured=unstructured,
        )
        instance._logger = logging.getLogger("ACI").getChild(
            await cache.get("request_id")
        )

        instance._step_count = 0
        instance._turns_remaining = initial_turns

        executors = await ACI._create_executors(
            inference_client,
            inference_client_factory,
            send_message_callback,
            recv_message_callback,
            instance,
        )

        instance.tool_executors = executors

        return instance

    def _take_turn(self, action: str):
        if (
            action
            in (
                "create_files",
                "edit_files",
                "delete_files",
            )
            and self.mode.value == ACIExecutionMode.SINGLE.value
        ):
            self._turns_remaining -= 1

        self._logger.debug(f"TURNS REMAINING {self._turns_remaining}")

    def validate_llm_call(
        self, resp, schema
    ) -> tuple[Literal[True], None] | tuple[Literal[False], str]:
        for key in schema["properties"].keys():
            if key not in resp and key in schema["required"]:
                self._logger.info(f"Missing key {key} in response.")
                return False, key

        return True, None

    def replace_closest_edit_distance(
        self, whole: str, part: str, replace: str
    ) -> Optional[str]:
        similarity_thresh = 0.8
        whole_lines = whole.split("\n")
        part_lines = part.split("\n")

        replace_lines = replace.split("\n")

        max_similarity = 0.0
        most_similar_chunk_start = -1
        most_similar_chunk_end = -1

        scale = 0.1
        min_len = math.floor(len(part_lines) * (1 - scale))
        max_len = math.ceil(len(part_lines) * (1 + scale))

        for length in range(min_len, max_len):
            for i in range(len(whole_lines) - length + 1):
                chunk = "".join(whole_lines[i : i + length])
                part_to_match = "".join(part_lines)

                similarity = SequenceMatcher(None, chunk, part_to_match).ratio()

                if similarity > max_similarity and similarity:
                    max_similarity = similarity
                    most_similar_chunk_start = i
                    most_similar_chunk_end = i + length

        if max_similarity < similarity_thresh:
            return None

        modified_whole = (
            whole_lines[:most_similar_chunk_start]
            + replace_lines
            + whole_lines[most_similar_chunk_end:]
        )

        return "\n".join(modified_whole)

    def set_pinned_files(self, pinned_files: dict[str, str]):
        self._pinned_files = pinned_files

    async def manipulate(self, action: ACIAction, fn: str) -> str:
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])

        input_task = await cache.get("input_message")

        output_modified_files = await cache.get("output_modified_files", {})
        analysis_lines = await cache.get("code_analysis", [])
        test_output = await cache.get("test_output")
        system_analysis = await cache.get("system_analysis_output")

        self._step_count += 1

        files_with_id = []
        for idx, file in enumerate(open_files):
            files_with_id.append(f"{idx}: {file}")

        if not isinstance(action, (CreateAction, OpenAction)):
            async with cache.with_suffix(f"file_edit_selection_{fn}"):
                index = await cache.get("index")
                lines_below = await cache.get("lines_below")
                lines_above = await cache.get("lines_above")
                lines = await cache.get("lines")

        match action:
            case StartAction():
                self._logger.debug(f"START {fn}")
                new_lines = lines[lines_above:index]

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.START,
                            status="",
                            files=open_files,
                            active_file=fn,
                            new_contents="\n".join(lines),
                            scroll_position=lines_above,
                        ),
                    )
                )
            case SwitchAction():
                self._logger.debug(f"SWITCH {fn}")
                await cache.set("active_file", action.file)
                index = min(index, len(lines))
                # TODO: adjust below/above
                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.SWITCH,
                            status=f"Looking through {action.file}",
                            active_file=action.file,
                            new_contents="\n".join(lines),
                            scroll_position=lines_above,
                        ),
                    )
                )
                new_lines = lines[:index][-self._lines_in_view() :]
            case TestAction():
                self._logger.debug(f"TEST {fn}")
                test_output = action.test_output
                await cache.set("test_output", test_output)
                new_lines = lines[:index][-self._lines_in_view() :]

                status = f"Ran tests for {fn}"

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.TEST,
                            status=status,
                            test_output=test_output,
                        ),
                    )
                )
            case SystemAnalysisAction():
                self._logger.debug(f"SYSTEM_ANALYSIS")
                new_lines = lines[:index][-self._lines_in_view() :]

                system_analysis = action.system_analysis_output

                await cache.set("system_analysis_output", system_analysis)
            case CreateAction() | OpenAction():
                open_files = await cache.get("viewer_open_files", [])
                if action.file not in open_files:
                    open_files.append(action.file)
                    files_with_id.append(f"{len(open_files) - 1}: {action.file}")

                await cache.set("viewer_open_files", open_files)

                lines = action.content.split("\n")

                index = min(len(lines), self._lines_in_view())
                lines_above = 0
                lines_below = len(lines) - index

                async with cache.with_suffix(f"file_edit_selection_{action.file}"):
                    await cache.set("lines_above", lines_above)
                    await cache.set("lines_below", lines_below)
                    await cache.set("index", index)
                    await cache.set("lines", lines)

                status = f"Opened {action.file}"

                if isinstance(action, CreateAction):
                    output_modified_files[action.file] = action.content
                    status = f"Created {action.file}"
                    await cache.set("output_modified_files", output_modified_files)

                await cache.set("active_file", action.file)

                new_lines = lines[lines_above:index]

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.CREATE,
                            status=status,
                            files=open_files,
                            active_file=action.file,
                            scroll_position=index,
                            new_contents=action.content,
                        ),
                    )
                )

            case EditAction():
                self._logger.debug(f"EDIT: {fn}")

                start_chunk = lines[:lines_above]
                end_chunk = lines[index:]

                new_content = []
                new_content.extend(start_chunk)
                viewer_lines = lines[lines_above:][: self._lines_in_view()]

                file_content = await self.file_rpc.read(fn, overlay_modified=True)
                if file_content is None:
                    self._turns_remaining += 1
                    self._logger.warning(f"Failed to read {fn} in EDIT.")
                    return "It appears the file is empty somehow this is an invalid state, please try to cope with this as best you can but otherwise. Cede control back to the driver."

                try:
                    source_file = SourceFile(fn, file_content.encode("utf-8"))
                    pattern = source_file.analyze_whitespace_pattern()
                except UnknownExtensionException:
                    pattern = SourceFile.WhitespacePattern()

                text = pattern.line_ending.join(viewer_lines)
                normalized_search = action.lines_to_replace
                normalized_replace = action.replace_text

                text = self.replace_closest_edit_distance(
                    text, normalized_search, normalized_replace
                )

                if not text:
                    self._turns_remaining += 1
                    return "Tried fuzzy replace and was not able to find a match, please double check the lines you are trying to replace."

                # Use normalize
                # Use normalized versions for replacement with consistent whitespace patterns
                try:
                    new_lines = text.split(pattern.line_ending)
                    new_content.extend(new_lines)
                    new_content.extend(end_chunk)

                    normalized_file_content = pattern.line_ending.join(new_content)

                    output_modified_files[fn] = normalized_file_content
                except Exception:
                    self._logger.exception(
                        f"Final whitespace normalization failed, falling back to basic normalization"
                    )
                    # Fallback to basic normalization
                    text = text.replace(normalized_search, action.replace_text.rstrip())
                    new_lines = text.split("\n")
                    new_content.extend(new_lines)
                    new_content.extend(end_chunk)
                    output_modified_files[fn] = "\n".join(new_content).rstrip()

                nl_len = len(new_lines)

                if nl_len < self._lines_in_view():
                    diff = self._lines_in_view() - nl_len
                    line_additions = lines[index:][:diff]
                    new_lines.extend(line_additions)
                    lines_below -= diff
                    lines_below = max(lines_below, 0)
                elif nl_len > self._lines_in_view():
                    diff = nl_len - self._lines_in_view()
                    new_lines = new_lines[: self._lines_in_view()]
                    lines_below += diff

                lines = new_content

                marker = max(len(new_lines), self._lines_in_view())
                index = lines_above + marker

                analysis_lines = []

                await cache.set("output_modified_files", output_modified_files)
                async with cache.with_suffix(f"file_edit_selection_{fn}"):
                    await cache.set("lines_below", lines_below)
                    await cache.set("lines", new_content)
                    await cache.set("index", index)

                edit_line = "\n".join(new_content)[
                    : "\n".join(new_content).find(action.replace_text)
                ].count("\n")
                edit_len = action.replace_text.count("\n")
                if action.replace_text.startswith(action.lines_to_replace):
                    edit_line += action.lines_to_replace.count("\n")
                    edit_len -= action.lines_to_replace.count("\n")
                elif action.replace_text.endswith(action.lines_to_replace):
                    edit_len -= action.lines_to_replace.count("\n")

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.EDIT,
                            status=f"Made changes to {fn}",
                            new_contents="\n".join(new_content),
                            scroll_position=edit_line,
                            changed_range=(
                                edit_line,
                                edit_line + edit_len,
                            ),
                        ),
                    )
                )

                await cache.set("code_analysis", analysis_lines)
                await cache.set("test_output", "")

                test_output = ""

            case ScrollDownAction() | ScrollUpAction():
                if isinstance(action, ScrollDownAction):
                    scroll = action.scroll
                else:
                    scroll = -action.scroll
                new_index = max(self._lines_in_view(), index + scroll)

                # Ensure the new_index is an index inside of the actual lines.
                new_index = min(new_index, len(lines) - 1)

                new_lines = lines[:new_index][-self._lines_in_view() :]

                lines_above = max(0, new_index - self._lines_in_view())
                lines_below = max(0, len(lines) - new_index)

                self._logger.debug(f"SCROLL: {fn}")

                async with cache.with_suffix(f"file_edit_selection_{fn}"):
                    await cache.set("lines_above", lines_above)
                    await cache.set("lines_below", lines_below)
                    await cache.set("index", new_index)

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.SCROLL,
                            status=f"Looking through {fn}",
                            scroll_position=lines_above,
                        ),
                    )
                )
            case JumpAction():
                self._logger.debug(f"JUMP: {fn}")
                self._logger.debug(f"LINE: {action.line}")

                # Center the view on the target line
                view_start = max(0, action.line - (self._lines_in_view() // 2))
                new_index = min(view_start + self._lines_in_view(), len(lines))
                new_lines = lines[view_start:new_index]

                lines_above = view_start
                lines_below = max(0, len(lines) - new_index)

                async with cache.with_suffix(f"file_edit_selection_{fn}"):
                    await cache.set("lines_above", lines_above)
                    await cache.set("lines_below", lines_below)
                    await cache.set("index", new_index)
                await cache.set("active_file", fn)

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.SWITCH,
                            status=f"Looking through {fn}",
                            active_file=fn,
                            scroll_position=view_start,
                            new_contents="\n".join(lines),
                        ),
                    )
                )

        thoughts = await self.cache.get("driver_subsystem_communications", "")

        toolset = (await self.toolsets())[ACIMode.CONSTRAINED]

        tool_names = "|".join([tool[1]["name"] for tool in toolset]) + "\n"

        viewer_state = ACIVisualizer.generate_viewer_state(
            input_task=input_task,
            files_with_id=files_with_id,
            fn=fn,
            lines_above=lines_above,
            lines_below=lines_below,
            new_lines=new_lines,
            analysis_lines=analysis_lines,
            test_output=test_output,
            system_analysis=system_analysis,
            available_tools=tool_names,
            thoughts=thoughts,
            turns_remaining=self._turns_remaining,
        )

        self.viewer_state = viewer_state
        await trace_output(viewer_state, "aci_output")
        viewer_history = await self.cache.get("viewer_history", [])
        viewer_history.append(viewer_state)
        await self.cache.set("viewer_history", viewer_history)

        return viewer_state

    async def edit_files(self, resp):
        cache = self.cache
        active_file = await cache.get("active_file", "")
        active_file_content = None

        if not resp.get("edits"):
            self._turns_remaining += 1
            return "You must provide edits to make."

        edits = resp["edits"]
        local_edits = []
        passing = True
        error_messages = []

        for i, edit in enumerate(edits):
            passed, failing_key = self.validate_llm_call(
                edit, self.tool_schemas["edit_files"]["input_schema"]["properties"]["edits"]["items"]
            )
            lines_to_replace = edit["lines_to_replace"]
            replace_text = edit["replace_text"]

            passing = passing and passed

            if not passed:
                error_messages.append(f"{failing_key} for item at index {i} in the items array is required for 'edit_files', please try again with the correct parameters.")

            if lines_to_replace.strip() == "BISMUTH_DELETED_FILE":
                passing = False
                error_messages.append("File at index {i} in the items array has been previously deleted you either need to recreate it or create an entirely new file.")

            if not lines_to_replace.strip():
                passing = False
                error_messages.append("'lines_to_replace' at index {i} in the items array is missing or empty. You must provide content in the lines to replace.")

        if not passing:
            print("Not passing edits returning")
            self._turns_remaining += 1
            return "\n".join(error_messages)

        print("after error")

        last_edited_files = await cache.get("last_edited_files", [])

        compound_state = []
        for i, edit in enumerate(edits):
            id = edit["id"]
            step = edit["step"]
            file = edit["file"]
            lines_to_replace = edit["lines_to_replace"]
            replace_text = edit["replace_text"]


            lines_to_replace = lines_to_replace.rstrip()
            replace_text = replace_text.rstrip()
            output_modified_files = await cache.get("output_modified_files", {})
            open_files = await cache.get("viewer_open_files", [])
            if (
                file not in open_files
                or output_modified_files.get(file) == "BISMUTH_DELETED_FILE"
            ):
                self._turns_remaining += 1
                return f"File {file} was deleted or closed, can only edit existing, open files."

            try:
                action = EditAction(
                    lines_to_replace=lines_to_replace,
                    replace_text=replace_text,
                    file=file,
                )
                print("Edit: Before manip")
                content = await self.manipulate(action, file)
                compound_state.append(content)
                print("Edit: After manip")

                if file == active_file:
                    active_file_content = content
            except ValueError as e:
                self._logger.exception(f"Error in manipulate(EditAction)")
                return str(e)

            session_id = await cache.get("msg_session_id")
            chat_session = ChatSessionEntity.get(session_id)
            assert chat_session is not None
            session_context = chat_session.get_context()

            context_edited_files = session_context.get("edited_files", [])
            context_edited_files.append(
                {
                    "file": file,
                    "step": step,
                }
            )

            session_context["edited_files"] = context_edited_files
            chat_session.set_context(session_context)

            locators = await cache.get("locators")
            locators.append(
                {
                    "id": id,
                    "file": file,
                    "step": step,
                    "lines_to_replace": lines_to_replace,
                    "replace": replace_text,
                }
            )
            await self.update_change_log(step)

            last_edited_files.append(file)

            await cache.set("locators", locators)

        await cache.set("last_edited_files", last_edited_files)

        print("Edit: before RPC")
        output_modified_files = await cache.get("output_modified_files", {})

        # Just try to grab the file raw, I'd rather this error if somehow it wasn't applied than nuke a local file for the user
        local_edits.append(FileEdit(path=file, replace=output_modified_files[file]))
        results = await self.file_rpc.edit(local_edits)
        print("Edit: after RPC")

        for result in results:
            if not result.success:
                print(f"Failed to apply edit file locally to {result.path} with error {result.message}.")

        if active_file_content:
            return "\n".join(compound_state)
        else:
            return "Files have been edited."

    async def switch_file(self, resp: dict[str, Any]) -> str:
        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["switch_file"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'switch_file', please try again with the correct parameters."

        file_id = int(resp["file_id"])
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])

        if file_id >= len(open_files) or file_id < 0:
            files_with_id = []
            for idx, file in enumerate(open_files):
                files_with_id.append(f"{idx}: {file}")
            return "Invalid file id. Valid files are:\n" + "\n".join(files_with_id)

        file = open_files[file_id]

        if file == "CLOSED":
            return "That file has been closed please try switching to a different file."

        old_active_file = await cache.get("active_file", "")

        if file == old_active_file:
            return "You are already on that file. Please take another action."

        content = await self.manipulate(SwitchAction(file=file), file)

        return content

    async def open_file(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["open_file"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'open_file', please try again with the correct parameters."

        file = resp["file"]

        open_files = await cache.get("viewer_open_files", [])

        file_content = await self.file_rpc.read(file, overlay_modified=True)

        if not file_content or file_content == "BISMUTH_DELETED_FILE":
            return "File doesn't exist - perhaps you have the wrong path?"

        if file in open_files:
            content = await self.manipulate(SwitchAction(file=file), file)
        else:
            content = await self.manipulate(
                OpenAction(content=file_content, file=file), file
            )

            session_id = await cache.get("msg_session_id")
            chat_session = ChatSessionEntity.get(session_id)
            assert chat_session is not None
            session_context = chat_session.get_context()
            context_opened_files = session_context.get("opened_files", [])
            context_opened_files.append(file)

            session_context["opened_files"] = context_opened_files
            chat_session.set_context(session_context)

        return content

    async def symbol_search(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        query = resp["query"]

        await self.send_aci_status(f"Searching for code related to {query}")

        feature_id = await cache.get("feature_id")

        search_vars = await cache.get("search_vars")

        try:
            graph = GraphRag(feature_id=feature_id, **search_vars)

            graph_results = await graph.search(
                query,
                overlay_files=await cache.get("modified_files", []),
                only_tests=False,
            )

            results = []
            for n, weight in graph_results:
                # Ignore FILE nodes which have more specific data (i.e. classes/funcs within the file)
                # but allow them if there are only file nodes for that file (e.g. chunked text)
                file_has_other_nodes = any(
                    n2
                    for n2, _ in graph_results
                    if n2.file_name == n.file_name and n2.type != KGNodeType.FILE
                )
                if n.type != KGNodeType.FILE or not file_has_other_nodes:
                    content = await self.file_rpc.read(n.file_name)
                    if not content:
                        continue
                    lines = content.split("\n")
                    if n.line < len(lines):
                        results.append(
                            {
                                "file": n.file_name,
                                "line_number": n.line,
                                "content": lines[n.line],
                                "weight": weight,
                            }
                        )

        except Exception:
            self._logger.exception(f"Error in symbol search, returning empty.")
            return json.dumps([])

        return json.dumps(results)

    def get_immediate_children(
        self, all_descendants: Iterable[str], current_path: str
    ) -> list[str]:
        """
        Extract immediate children of current_path from a list of all descendant paths.

        Args:
            all_descendants (list): List of strings containing all descendant file/directory paths
            current_path (str): The parent path to find immediate children for

        Returns:
            list: Immediate children paths of the current_path
        """
        # Normalize the current path to ensure consistent handling
        normalized_path = current_path.rstrip("/") + "/"

        children = set()

        for path in all_descendants:
            # Skip if path is the same as current_path
            if path == current_path or path == normalized_path:
                continue

            # Check if this path is under the current_path
            if path.startswith(normalized_path):
                # Get the relative path from current_path
                relative_path = path[len(normalized_path) :]

                # Split on first '/' to get immediate child
                first_segment = relative_path.split("/", 1)[0]

                # If there's content in first_segment, it's an immediate child
                if first_segment:
                    children.add(normalized_path + first_segment)

        return sorted(list(children))

    async def list_files(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        await self.send_aci_status("Listing files in the repository...")

        subpath = resp.get("subpath", "")

        files = await self.file_rpc.list(overlay_modified=True)

        if sum(len(f) for f in files if f.startswith(subpath)) > 100_000:
            out = f"The repository is too large to list all files. Here are the files and folders within '{subpath}':\n"
            out += "\n".join(self.get_immediate_children(files, subpath))
            return out

        files = sorted(f for f in files if f.startswith(subpath))
        active_file = await cache.get("active_file")
        analysis = textwrap.dedent(
            f"""
        <all_repo_files>
        {"\n".join(files)}
        </all_repo_files>
        """
        )

        content = await self.manipulate(
            SystemAnalysisAction(system_analysis_output=analysis), active_file
        )

        return content

    async def finalize(self, resp: dict[str, Any]) -> str:
        self._logger.debug("Finalizing...")
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])
        task_config = await cache.get("planned_task_config", {})
        if not self._attempted_finalize and self.recursion_depth == 0:
            self._test_failure_count = 0

        self._attempted_finalize = True

        steps_since_test = self._last_ran_tests - self._step_count

        if self.recursion_depth == 0:
            for i in range(0, len(open_files)):
                open_files[i] = "CLOSED"

            await cache.set("viewer_open_files", open_files)

        self.finalized = True

        self._logger.info("Finalized.")

        raise StopAsyncIteration(f"Finalized.")

    async def update_change_log(self, change: str):
        cache = self.cache
        change_log = await cache.get("change_log", [])
        change_log.append(change)
        await cache.set("change_log", change_log)

    async def close_file(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["close_file"]["input_schema"]
        )
        if not passed:
            return f"{failing_key} is required for 'close_file', please try again with the correct parameters."
        open_files = await cache.get("viewer_open_files", [])

        file_id = int(resp["file_id"])

        if len(open_files) == 1 and open_files[file_id] == "placeholder_file":
            return "You must create or open at least one new file before closing the placeholder file!"

        if file_id >= len(open_files) or file_id < 0:
            files_with_id = []
            for idx, file in enumerate(open_files):
                files_with_id.append(f"{idx}: {file}")
            return "Invalid file id. Valid files are:\n" + "\n".join(files_with_id)

        self._logger.debug(f"CLOSE {open_files[file_id]}")

        if open_files[file_id] == "CLOSED":
            return "That file is already closed, please take a new action."

        await self.send_message_callback(
            WSMessage(
                type=WSMessageType.ACI,
                aci=ACIMessage(
                    action=ACIMessage.Action.CLOSE,
                    status=f"Closed {open_files[file_id]}",
                ),
            )
        )

        only_placeholder = False

        if len(open_files) == 1 and open_files[file_id] == "placeholder_file":
            only_placeholder = True

        open_files[file_id] = "CLOSED"

        await cache.set("viewer_open_files", open_files)

        if all([file == "CLOSED" for file in open_files]) and not only_placeholder:
            self._logger.debug("ALL FILES CLOSED CALLED FINALIZE WITH TESTS")
            return await self.finalize({})

        try:
            new_active_fn = next((fn for fn in open_files if fn != "CLOSED"))
            return await self.manipulate(
                SwitchAction(file=new_active_fn), new_active_fn
            )
        except StopIteration:
            self._logger.warning(
                "No next file, likely closed placeholder_file first in new project."
            )
            return ""

    async def delete_files(self, resp) -> str:
        deletes = resp["deletes"]

        passing = True
        error_messages = []
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])

        for i, delete in enumerate(deletes):
            passed, failing_key = self.validate_llm_call(
                delete, self.tool_schemas["delete_files"]["input_schema"]["properties"]["deletes"]["items"]
            )
            
            passing = passing and passed

            if not passed:
                error_messages.append(f"{failing_key} for item at index {i} is required for 'delete_files', please try again with the correct parameters.")

            file_id = int(delete["file_id"])
            if file_id >= len(open_files) or file_id < 0:
                files_with_id = []
                for idx, file in enumerate(open_files):
                    files_with_id.append(f"{idx}: {file}")
                error_messages.append(f"Invalid file id {file_id} for element {i}. Valid files are:\n" + "\n".join(files_with_id))
                passing = False

            if open_files[file_id] == "CLOSED":
                error_messages.append(f"File {file_id} at array element {i} is already closed.")
                passing = False


        if not passing:
            self._turns_remaining += 1
            return "\n".join(error_messages)
        
        new_active_fn = None
        local_deletes = []

        for i, delete in enumerate(deletes):
            file_id = int(delete["file_id"])

            self._logger.debug(f"DELETE {open_files[file_id]}")

            modified_files = await cache.get("output_modified_files", {})

            fn = open_files[file_id]

            modified_files[fn] = "BISMUTH_DELETED_FILE"

            async with cache.with_suffix(f"file_edit_selection_{fn}"):
                await cache.delete("lines_above")
                await cache.delete("lines_below")
                await cache.delete("index")
                await cache.delete("lines")

            await cache.set("output_modified_files", modified_files)

            open_files[file_id] = "CLOSED"

            await self.update_change_log(delete["step"])
            await cache.set("viewer_open_files", open_files)

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.ACI,
                    aci=ACIMessage(
                        action=ACIMessage.Action.CLOSE,
                        status=f"Deleted {fn}",
                    ),
                )
            )

            local_deletes.append(FileDelete(path=fn))

            try:
                new_active_fn = next((fn for fn in open_files if fn != "CLOSED"))
            except StopIteration:
                self._logger.warning(
                    "No next file, likely deleted placeholder_file first in new project."
                )
                return ""

            session_id = await cache.get("msg_session_id")
            chat_session = ChatSessionEntity.get(session_id)
            assert chat_session is not None
            session_context = chat_session.get_context()
            context_deleted_files = session_context.get("deleted_files", [])
            context_deleted_files.append(fn)

            session_context["deleted_files"] = context_deleted_files
            chat_session.set_context(session_context)

        results = await self.file_rpc.delete(local_deletes)
        for result in results:
            if not result.success:
                print(f"Failed to apply delete file locally to {result.path} with error {result.message}.")

        return await self.manipulate(SwitchAction(file=new_active_fn), new_active_fn)

    async def scroll_down_file(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["scroll_down_file"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'scroll_down_file', please try again with the correct parameters."

        scroll = int(resp["scroll"])

        self._logger.debug(f"SCROLL DOWN {scroll}")

        active_file = await cache.get("active_file")

        content = await self.manipulate(ScrollDownAction(scroll=scroll), active_file)

        return content

    async def scroll_up_file(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["scroll_up_file"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'scroll_up_file', please try again with the correct parameters."

        scroll = int(resp["scroll"])

        self._logger.debug(f"SCROLL UP {scroll}")

        active_file = await cache.get("active_file")

        content = await self.manipulate(ScrollUpAction(scroll=scroll), active_file)

        return content
    
    async def tool_result_reducer(self, states: list[dict[str, Any]]) -> str:
        return "\n".join([state["content"] for state in states])

    async def create_files(self, resp) -> str:
        cache = self.cache

        creates = resp["creates"]

        passing = True
        error_messages = []

        last_created_files = await cache.get("last_created_files", [])

        compound_state = []

        for i, create in enumerate(creates):
            passed, failing_key = self.validate_llm_call(
                create, self.tool_schemas["create_files"]["input_schema"]["properties"]["creates"]["items"]
            )

            passing = passing and passed

            if not passed:
                error_messages.append(f"{failing_key} in item {i} of the 'creates' array is required for 'create_files', please try again with the correct parameters.")

            file = create["file"]

            exists = (
                await self.file_rpc.read(file, overlay_modified=True) is not None
            ) or (
                (await cache.get("output_modified_files", {})).get(
                    file, "BISMUTH_DELETED_FILE"
                )
                != "BISMUTH_DELETED_FILE"
            )
            if exists:
                error_messages.append(f"Element {i}, {file} in the 'creates' array exists already.")
                passing = False

        if not passing:
            self._turns_remaining += 1
            return "\n".join(error_messages)

        local_file_creates = []

        for i, create in enumerate(creates):
            content = create["content"]
            step = create["step"]
            file = create["file"]
            self._logger.debug(f"CREATE FILE {file}")

            content = await self.manipulate(CreateAction(content=content, file=file), file)

            session_id = await cache.get("msg_session_id")
            chat_session = ChatSessionEntity.get(session_id)
            assert chat_session is not None

            session_context = chat_session.get_context()
            context_created_files = session_context.get("created_files", [])
            context_created_files.append(file)
            session_context["created_files"] = context_created_files
            chat_session.set_context(session_context)
            await self.update_change_log(step)

            local_file_creates.append(FileCreate(path=file, content=content))

            compound_state.append(content)

            last_created_files.append(file)

        await cache.set("last_created_files", last_created_files)

        results = await self.file_rpc.create(local_file_creates)
        
        for result in results:
            if not result.success:
                print(f"Failed to apply create file locally to {result.path} with error {result.message}.")

        return "\n".join(compound_state)

    async def go_to_line(self, resp: dict[str, Any]) -> str:
        self._logger.debug("GO TO LINE")

        cache = self.cache
        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["go_to_line"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'go_to_line', please try again with the correct parameters."

        active_file = await cache.get("active_file")

        line = int(resp["line_num"])

        content = await self.manipulate(JumpAction(line=line), active_file)

        return content

    async def tool_parser(self, content):
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            mode = ACIMode.CONSTRAINED
        else:
            mode = self._mode
        prompt = self.prompts()[mode]
        self._schema_parser = WAILGenerator(str(self._template_root()))

        self._schema_parser.load_wail(prompt)

        out = self._schema_parser.parse_llm_output(content)

        tools = []
        has_create_edit = False

        for tool in out["res"]:
            tools.append({"name": tool["_type"], "input": tool } | tool)

            if tool["_type"] in ["CreateFiles", "EditFiles"]:
                has_create_edit = True

        if has_create_edit:
            tools.append(
                {"name": "AnalyzeCode", "input": {}}
            )

        return tools

    async def prompt_and_toolset_for_current_mode(self):
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            mode = ACIMode.CONSTRAINED
        else:
            mode = self._mode

        input_task = self._input_task

        task_config = await self.cache.get("planned_task_config", {})
        prompt_extra = await self.cache.get("prompt_extra", {})
        starting_context = await self.cache.get("unmodified_context", {})

        pinned_file_context = ""

        if bool(self._pinned_files):
            for fn, content in self._pinned_files.items():
                tmp = textwrap.dedent(
                    f"""
                <file name="{fn}">
                {content}
                </file>\n
                """
                )

                pinned_file_context += tmp

        toolset = (await self.toolsets())[mode]

        files = "\n".join(list(starting_context.keys()))
        prompt = self.prompts()[mode]

        viewer_history = await self.cache.get("viewer_history", [])
        
        if self.unstructured:
            prompt = self.prompts()[mode]
            self._schema_parser = WAILGenerator(str(self._template_root()))
            self._schema_parser.load_wail(prompt)

            (prompt, warnings, errs) = self._schema_parser.get_prompt(
                lines=self._lines_in_view(),
                task=input_task,
                files=files,
                viewer_state=viewer_history[0],
                turns=self.initial_turns,
                execution_mode=str(self.mode),
                pinned_files=pinned_file_context,
                **task_config,
                **prompt_extra,
            )
        else:
            prompt = Template(prompt).render(
                lines=self._lines_in_view(),
                task=input_task,
                files=files,
                viewer_state=viewer_history[0],
                turns=self.initial_turns,
                execution_mode=self.mode,
                pinned_files=pinned_file_context,
                **task_config,
                **prompt_extra,
            )

        return (prompt, toolset, mode)
    