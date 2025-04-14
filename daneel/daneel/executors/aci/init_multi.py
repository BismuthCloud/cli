from difflib import SequenceMatcher
import pathlib
import tomllib
from asimov.asimov_base import AsimovBase
import enum
from daneel.data.bismuth_config import BismuthTOML, BismuthTestTOML
from pathlib import Path
from daneel.data.postgres.models import ChatSessionEntity
from daneel.data.file_rpc import FileRPC
from daneel.data.graph_rag import GraphRag
from daneel.data.postgres.models import FeatureEntity
from daneel.executors.aci.bug_finding_visualizer import BugFindingVisualizer
from daneel.executors.aci.fuzz_visualizer import FuzzVisualizer
from daneel.executors.aci.aci_interactive_driver_executor import ACIDriverExecutor
from daneel.executors.aci.visualization import ACIVisualizer
import json
import logging
from text_unidecode import unidecode  # type: ignore
from asimov.graph import ModuleConfig
from daneel.executors.aci.aci_system_analysis_executor import (
    ACISystemAnalysisExecutor,
)
from daneel.data.graph_rag.graph import KGNodeType
from daneel.executors.aci.prompts import *
from daneel.executors.summary_executor import SummaryExecutor

from asyncio import Semaphore

from asimov.graph import AgentModule, ModuleType
from asimov.services.inference_clients import InferenceClient
from difflib import SequenceMatcher

import os
from daneel.services.analysis_client import (
    AnalysisStrictness,
    CodeAnalysisClient,
)
from pydantic import Field, model_validator, PrivateAttr
from jinja2 import Template

from typing import Any, Awaitable, Callable, Iterable, Optional
from asimov.caches.cache import Cache
import textwrap

from daneel.utils import find_text_chunk
from daneel.services.ast_code_analysis.analysis.source_file import (
    SourceFile,
    UnknownExtensionException,
)
from daneel.services.ast_code_analysis import repo_skeleton, Repository
import math

from daneel.utils.repo import get_clone_url
from daneel.utils.tracing import trace_output
from daneel.utils.websockets import (
    ACIMessage,
    ChatModifiedFile,
    RunCommandMessage,
    RunCommandResponse,
    WSMessage,
    WSMessageType,
    FileEdit,
    FileCreate,
    FileDelete,
    null_recv_callback,
    null_send_callback,
)

from daneel.executors.aci.aci_types import *
from gasp import WAILGenerator

LINES_IN_VIEW = 500
LINES_IN_VIEW_CONSTRAINED = 2000
RECURSION_LIMIT = 1

GIT_HOST = os.environ.get("GIT_HOST", "localhost:8000")


class ACIExecutionMode(enum.Enum):
    SINGLE = "single"
    MULTI = "multi"


class ACI(AsimovBase):
    cache: Cache
    viewer_state: str = Field(default="")
    tool_executors: dict["str", AgentModule] = Field(default_factory=dict)
    send_message_callback: Callable[[WSMessage], Awaitable[None]]
    recv_message_callback: Callable[[], Awaitable[WSMessage]]
    file_rpc: FileRPC
    driver_mode: ACIMode = ACIMode.DRIVER
    initial_turns: int = 25
    interactive_mode: bool = Field(default=False)
    run_tests_on_finalize: bool = Field(default=False)
    recursion_depth: int = Field(default=0)
    finalized: bool = Field(default=False)
    mode: ACIExecutionMode = Field(default=ACIExecutionMode.MULTI)
    unstructured: bool = Field(default=False)
    _input_task: str = PrivateAttr()
    _step_count: int = PrivateAttr()
    _pinned_files: dict[str, str] = PrivateAttr(default_factory=dict)
    _turns_remaining: int = PrivateAttr(default=0)
    _recursive_task: Optional[str] = PrivateAttr(default=None)
    _attempted_finalize: bool = PrivateAttr(default=False)
    _last_ran_tests: int = PrivateAttr(default=0)
    _mode: ACIMode = PrivateAttr(default=ACIMode.DRIVER)
    _humanlayer_enabled: bool = PrivateAttr(default=False)
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
        "switch_to_navigation_mode": {
            "name": "switch_to_navigation_mode",
            "description": "Switch to navigation mode, this mode unlocks the abilty to scroll a file, switch active files, go to def, go to line, list files, find by phrase, close a file and open a file.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "What you want the navigation system to accomplish before returning control to you. Be detailed, it should be step by step.",
                    }
                },
                "required": ["goal"],
            },
        },
        "switch_to_editing_mode": {
            "name": "switch_to_editing_mode",
            "description": "Switch to editing mode, this mode unlocks the ability to delete a file, create a file, edit existing files, run commands and testing / building.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "goal": {
                        "type": "string",
                        "description": "What you want the editing system to accomplish before returning control to you. Be detailed, it should be step by step.",
                    }
                },
                "required": ["goal"],
            },
        },
        "show_skeleton": {
            "name": "show_repo_skeleton",
            "description": "Show a 'skeleton' of the files you provide from the codebase, which is the source files with only the class and function declarations/prototypes. In the case of python, this is similar to a type stub file for example. This is useful for understanding the structure of files in the codebase and the relationships between files.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "interesting_files": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "A list of interesting files whose skeletons will be shown.",
                    }
                },
                "required": [],
            },
        },
        "switch_to_driver_mode": {
            "name": "switch_to_driver_mode",
            "description": "Switch to driver mode which can call finalize if the task is done or switch to one of 3 other modes, navigation, editing or debug modes depending on what it decides based on the current visualizer state.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "results": {
                        "type": "string",
                        "description": "Thoughts about the results of your current goal that you want to communicate to the driver system as you return control.",
                    }
                },
                "required": ["results"],
            },
        },
        "analyze_system": {
            "name": "analyze_system",
            "description": "Performs a deep analysis of recent issues you are seeing in completing your task and attempts to provide tactical steps to become unstuck and complete the task.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "focus": {
                        "type": "string",
                        "description": "The specific aspect or issue you want to analyze. This helps direct the analysis to relevant parts of the system.",
                    }
                },
                "required": ["focus"],
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
        "symbol_search": {
            "name": "symbol_search",
            "description": "Searches for symbols (function or class) within the codebase based on a natural language query. This tool uses callgraph information and can find dependent symbols as well as leaf symbols. This operation helps locate relevant code components by identifying symbols related to the query up and down call graphs. It's particularly useful for understanding code dependencies and exploring the implementation details of referenced symbols especially when combined with 'go_to_def'. Quality of this tool is language dependent. If you are not getting the results you expect, try rephrasing your query or using 'find_phrase' instead.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The query should describe the symbol you are looking for in plain English based on the functionality or purpose of the symbol. For example, 'function that reads a file' or 'class that handles user authentication'.",
                    }
                },
                "required": ["query"],
            },
        },
        "find_phrase": {
            "name": "find_phrase",
            "description": "Finds the specific, case sensitive phrase across the codebase, returns filename and line number for all matches.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "exact_match_phrase": {
                        "type": "string",
                        "description": "A phrase to search for exact case sensitive matches over the codebase.",
                    }
                },
                "required": ["exact_match_phrase"],
            },
        },
        "go_to_line": {
            "name": "go_to_line",
            "description": "Jumps to the specific line number in the current active file. Useful for navigating the codebase combined with 'find' and 'open'. This call will make sure the line is in the current viewer state. Calling it more than once does nothing.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "line_num": {
                        "type": "number",
                        "description": "The line to jump to in the current active file.",
                    }
                },
                "required": ["line_num"],
            },
        },
        "run_command": {
            "name": "run_command",
            "description": "Run the specified bash command and return the result. Each command is executed in a separate shell session, so commands that rely on environment variables or other state changes will not persist between commands.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to run.",
                    }
                },
                "required": ["command"],
            },
        },
        "recurse": {
            "name": "recurse",
            "description": "A subagent will attempt to independently finish the singular specifc subtask you specify for it. This spawns a full copy of yourself with all current state included. This is a very powerful command and should be used only in cases that truly call for it such when you are very stuck or the task is vauge or broad.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "subtask": {
                        "type": "string",
                        "description": "A SINGLE specific and detailed subtask of the larger task you are trying to complete that you believe would be best served with having a copy of your full attention on it. Vague subtasks are likely to fail, so be very specific in what you ask.",
                    }
                },
                "required": ["subtask"],
            },
        },
        "recurse_fuzz": {
            "name": "recurse_fuzz",
            "description": "A subagent will attempt to test the specified target using fuzzing techniques. This spawns a full copy of yourself with all current state included.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "The target function to fuzz test. The function should be pure (not depend on file I/O, network I/O, etc.) and have a well-defined input/output contract.",
                    }
                },
                "required": ["target"],
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
        "go_to_def": {
            "name": "go_to_def",
            "description": "Navigates directly to the definition of a specified symbol (function or class) anywhere within the codebase. This operation enables quick navigation between related code components, automatically opening new files if needed. It's particularly useful for understanding code dependencies and exploring the implementation details of referenced symbols.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "The name of the function or class whose definition you want to navigate to.",
                    },
                    "line": {
                        "type": "string",
                        "description": "The line number of the current file which references the symbol.",
                    },
                },
                "required": ["symbol", "line"],
            },
        },
        "find_references": {
            "name": "find_references",
            "description": "Finds all references to a specified symbol (function or class) within the codebase. This operation provides a comprehensive list of all locations where the symbol is used, enabling you to explore the context and usage of the symbol across the codebase.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "The name of the function or class whose references you want to find.",
                    },
                    "line": {
                        "type": "string",
                        "description": "The line number of the current file which references the symbol.",
                    },
                },
                "required": ["symbol", "line"],
            },
        },
        "reach_out_to_human_for_assistance": {
            "name": "reach_out_to_human_for_assistance",
            "description": "Reach out to humans for assistance when you are stuck writing code that means tests or building are getting stuck failing.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message you want to send to the human reaching out for help.",
                    }
                },
                "required": ["message"],
            },
        },
        "report_bug_ci": {
            "name": "report_bug",
            "description": "Report a bug to the user as part of code review.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "The file that contains the bug.",
                    },
                    "lines_to_replace": {
                        "type": "string",
                        "description": "The exact content of the lines of text to be replaced. These lines must exist within the content currently in the viewer state. Whitespace and linebreaks must be the same. Do not include the line number.",
                    },
                    "replace_text": {
                        "type": "string",
                        "description": "The new content that will replace the specified lines. This field is required if a fix for the problem is easily implemented. If a bug report is complex or only advisory, do not include this field. Do not include the line number.",
                    },
                    "bug_description": {
                        "type": "string",
                        "description": "A detailed description of the bug you are experiencing. Use markdown formatting to make the bug report clear and easy to understand.",
                    },
                    "grounding": {
                        "type": "string",
                        "description": 'What makes you believe the bug is truly an issue. This should be the name of a failing test, or the word "fuzzing" if the bug was found through fuzzing. Omit this field if no test or fuzzing was involved.',
                    },
                },
                "required": [
                    "file",
                    "lines_to_replace",
                    "replace_text",
                    "bug_description",
                ],
            },
        },
        "pop_go_to_def": {
            "name": "pop_go_to_def",
            "description": "Pop the last item off the call stack and return to the previous file and line.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": [],
            },
        },
        "add_question": {
            "name": "add_question",
            "description": "Add a question to guide further exploration.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question you want answered based on your current understanding of the codebase.",
                    }
                },
                "required": ["question"],
            },
        },
        "resolve_question": {
            "name": "resolve_question",
            "description": "Resolve a question previously asked to provider better understanding of the codebase.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "question_id": {
                        "type": "number",
                        "description": "The id of the question you want to resolve.",
                    },
                    "answer": {
                        "type": "string",
                        "description": "The answer to the question you want to resolve.",
                    },
                },
                "required": ["question_id"],
            },
        },
        "add_memory": {
            "name": "add_memory",
            "description": "Add a memory about a non-obvious fact that you have found while exploring.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "memory": {
                        "type": "string",
                        "description": "The memory you want to add. Be sure to include all relevant information to help you remember the fact later.",
                    }
                },
                "required": ["memory"],
            },
        },
        "run_fuzzer": {
            "name": "run_fuzzer",
            "description": "Run the given fuzzer against the code base.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The code for the fuzzer.",
                    }
                },
                "required": ["code"],
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

    analysis_endpoint: str = os.environ.get("CODE_ANALYSIS_URL", "localhost:8051")
    _analysis_client: CodeAnalysisClient = PrivateAttr()

    @model_validator(mode="after")
    def setup_analysis_client(self):
        self._analysis_client = CodeAnalysisClient(self.analysis_endpoint)
        self._mode = self.driver_mode
        return self

    @classmethod
    async def _create_executors(
        cls,
        inference_client: InferenceClient,
        inference_client_factory: Callable[[str], InferenceClient],
        send_message_callback: Callable[[WSMessage], Awaitable[None]],
        recv_message_callback: Callable[[], Awaitable[WSMessage]],
        aci: "ACI",
    ) -> dict[str, AgentModule]:
        from daneel.executors.bug_detection.fuzz_gen import FuzzGenExecutor

        return {
            "analyze_system": ACISystemAnalysisExecutor(
                name="ACISystemAnalysis",
                type=ModuleType.EXECUTOR,
                inference_client=inference_client,
            ),
            "recurse": ACIDriverExecutor(
                name=f"RecursiveACIDriverExecutor",
                type=ModuleType.EXECUTOR,
                config=ModuleConfig(
                    timeout=3000,
                ),
                inference_client_factory=inference_client_factory,
                send_message_callback=send_message_callback,
                recv_message_callback=recv_message_callback,
                file_rpc=aci.file_rpc,
                aci=aci,
            ),
            "summary": SummaryExecutor(
                name="ACISummaryExec",
                type=ModuleType.EXECUTOR,
                inference_client=inference_client,
            ),
            "fuzz": FuzzGenExecutor(
                name="FuzzGenExecutor",
                type=ModuleType.EXECUTOR,
                file_rpc=aci.file_rpc,
                inference_client=inference_client,
                aci=aci,
            ),
        }
    
    def _template_root(self):
        template_root = Path(__file__).parent / "prompts" / "wail"

        return template_root

    def _unstructured_prompts(self):
        def template_path(path_str):
            template_root= self._template_root()

            return template_root.joinpath(path_str)

        prompts = {
            ACIMode.CONSTRAINED: template_path("constrained.wail"),
            ACIMode.DRIVER: template_path("driver.wail"),
            ACIMode.EDIT: template_path("edit.wail"),
            ACIMode.NAVIGATE: template_path("navigation.wail"),
            ACIMode.DEBUG: template_path("debug.wail")
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
            ACIMode.DRIVER: DRIVER_PROMPT,
            ACIMode.EDIT: EDIT_PROMPT,
            ACIMode.NAVIGATE: NAVIGATION_PROMPT,
            ACIMode.CI_BUG_FINDER: CI_BUG_FINDER_PROMPT,
            ACIMode.FULL_SCAN_BUG_FINDER: FULL_SCAN_BUG_FINDER_PROMPT,
            ACIMode.FUZZ_GEN: FUZZ_GEN_PROMPT,
        }

        return prompts

    def enable_humanlayer(self, enabled: bool) -> None:
        self._humanlayer_enabled = enabled

    async def send_aci_status(self, status: str) -> None:
        await self.send_message_callback(
            WSMessage(
                type=WSMessageType.ACI,
                aci=ACIMessage(
                    action=ACIMessage.Action.STATUS,
                    status=status,
                ),
            )
        )

    def set_mode(self, mode: str) -> None:
        self._logger.info(f"SET MODE {mode}")

        if mode == "single":
            self.mode = ACIExecutionMode.SINGLE
        else:
            self.mode = ACIExecutionMode.MULTI

        if self.mode.value == ACIExecutionMode.SINGLE.value:
            self.initial_turns = 10
            self._turns_remaining = 10

    def _turn_wrapper(self, action_func):
        def wrapped_fn(*args, **kwargs):
            action = action_func.__name__

            if self._turns_remaining <= 0:
                self._logger.info(
                    f"Bailing out of action {action} due to no turns remaining."
                )
                return "You have run out of turns to complete this task. Call finalize now."

            self._take_turn(action)

            try:
                return action_func(*args, **kwargs)
            except Exception as e:
                self._logger.exception(f"Error in action {action}: {e}")
                return f"Error in action {action}: {e} perhaps your input was incorrect or malformed."

        return wrapped_fn

    async def show_skeleton(self, resp: dict[str, Any]) -> str:
        interesting_files = resp["interesting_files"]

        repo = Repository(
            {
                fn: (await self.file_rpc.read(fn, overlay_modified=True)) or ""
                for fn in interesting_files
            }
        )
        skeleton = repo_skeleton(repo)

        return skeleton
    
    async def _unstructured_toolsets(self):
        constrained_tools = [
            (self.create_files, {"name": "CreateFiles"}),
            (self.edit_files, {"name": "EditFiles"}),
            (self.delete_files, {"name": "DeleteFiles"}),
            (self.open_file, {"name": "OpenFile"}),
            (self.switch_file, {"name": "SwitchFile"}),
            (self.list_files, {"name": "ListFiles"}),
            (self.finalize, {"name": "Finalize"}),
            (self.analyze_code, {"name": "AnalyzeCode"}),
            (self.run_command, {"name": "RunCommand"}),
        ]

        driver_tools = [
            (self.switch_to_navigation_mode, {"name": "SwitchToNavigationMode"}),
            (self.switch_to_editing_mode, {"name": "SwitchToEditingMode"}),
            (self.analyze_code, {"name": "AnalyzeCode"})
        ]

        edit_tools = [
            (self.edit_files, {"name": "EditFiles"}),
            (self.delete_files, {"name": "DeleteFiles"}),
            (self.open_file, {"name": "OpenFile"}),
            (self.switch_file, {"name": "SwitchFile"}),
            (self.create_files, {"name": "CreateFiles"}),
            (self.scroll_down_file, {"name": "ScrollDownFile"}),
            (self.scroll_up_file, {"name": "ScrollUpFile"}),
            (self.switch_to_driver_mode, {"name": "SwitchToDriverMode"}),
            (self.switch_to_navigation_mode, {"name": "SwitchToNavigationMode"}),
            (self.switch_to_editing_mode, {"name": "SwitchToEditingMode"}),
            (self.analyze_code, {"name": "AnalyzeCode"})
        ]

        nav_tools = [
            (self.scroll_down_file, {"name": "ScrollDownFile"}),
            (self.scroll_up_file, {"name": "ScrollUpFile"}),
            (self.switch_file, {"name": "SwitchFile"}),
            (self.close_file, {"name": "CloseFile"}),
            (self.open_file, {"name": "OpenFile"}),
            (self.list_files, {"name": "ListFiles"}),
            (self.find_phrase, {"name": "FindPhrase"}),
            (self.go_to_line, {"name": "GoToLine"}),
            (self.go_to_def, {"name": "GoToDef"}),
            (self.show_skeleton, {"name": "ShowSkeleton"}),
            (self.symbol_search, {"name": "SymbolSearch"}),
            (self.switch_to_driver_mode, {"name": "SwitchToDriverMode"}),
            (self.switch_to_editing_mode, {"name": "SwitchToEditingMode"}),
            (self.analyze_code, {"name": "AnalyzeCode"})
        ]

        debug_tools = [
            (self.edit_files, {"name": "EditFiles"}),
            (self.list_files, {"name": "ListFiles"}),
            (self.find_phrase, {"name": "FindPhrase"}),
            (self.go_to_line, {"name": "GoToLine"}),
            (self.go_to_def, {"name": "GoToDef"}),
            (self.switch_file, {"name": "SwitchFile"}),
            (self.analyze_system, {"name": "AnalyzeSystem"}),
            (self.open_file, {"name": "OpenFile"}),
            (self.analyze_code, {"name": "AnalyzeCode"})
        ]

        sets = {
            ACIMode.DRIVER: driver_tools,
            ACIMode.EDIT: edit_tools,
            ACIMode.DEBUG: debug_tools,
            ACIMode.NAVIGATE: nav_tools,
            ACIMode.CONSTRAINED: constrained_tools
        }

        for set, tools in sets.items():
            wrapped_tools = list(map(lambda t: (self._turn_wrapper(t[0]), t[1]), tools))

            sets[set] = wrapped_tools

        return sets

    async def toolsets(self) -> dict[ACIMode, list[Any]]:
        if self.unstructured:
            sets = await self._unstructured_toolsets()

            return sets

        sets = {
            ACIMode.DRIVER: [
                (self.finalize, self.tool_schemas["finalize"]),
                (
                    self.switch_to_editing_mode,
                    self.tool_schemas["switch_to_editing_mode"],
                ),
                (
                    self.switch_to_navigation_mode,
                    self.tool_schemas["switch_to_navigation_mode"],
                ),
                (self.run_command, self.tool_schemas["run_command"]),
            ],
            ACIMode.EDIT: [
                (self.edit_files, self.tool_schemas["edit_files"]),
                (self.delete_files, self.tool_schemas["delete_files"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.create_files, self.tool_schemas["create_files"]),
                (self.scroll_down_file, self.tool_schemas["scroll_down_file"]),
                (self.scroll_up_file, self.tool_schemas["scroll_up_file"]),
                (
                    self.switch_to_driver_mode,
                    self.tool_schemas["switch_to_driver_mode"],
                ),
                (
                    self.switch_to_navigation_mode,
                    self.tool_schemas["switch_to_navigation_mode"],
                ),
                (self.run_command, self.tool_schemas["run_command"]),
            ],
            ACIMode.NAVIGATE: [
                (self.scroll_down_file, self.tool_schemas["scroll_down_file"]),
                (self.scroll_up_file, self.tool_schemas["scroll_up_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.close_file, self.tool_schemas["close_file"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.find_phrase, self.tool_schemas["find_phrase"]),
                (self.go_to_line, self.tool_schemas["go_to_line"]),
                (self.go_to_def, self.tool_schemas["go_to_def"]),
                (
                    self.switch_to_driver_mode,
                    self.tool_schemas["switch_to_driver_mode"],
                ),
                (
                    self.switch_to_editing_mode,
                    self.tool_schemas["switch_to_editing_mode"],
                ),
                (
                    self.show_skeleton,
                    self.tool_schemas["show_skeleton"],
                ),
                (
                    self.symbol_search,
                    self.tool_schemas["symbol_search"],
                ),
            ],
            ACIMode.CONSTRAINED: [
                (self.edit_files, self.tool_schemas["edit_files"]),
                (self.create_files, self.tool_schemas["create_files"]),
                (self.delete_files, self.tool_schemas["delete_files"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.finalize, self.tool_schemas["finalize"]),
            ],
            ACIMode.CI_BUG_FINDER: [
                (self.scroll_down_file, self.tool_schemas["scroll_down_file"]),
                (self.scroll_up_file, self.tool_schemas["scroll_up_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.close_file, self.tool_schemas["close_file"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.find_phrase, self.tool_schemas["find_phrase"]),
                (self.go_to_line, self.tool_schemas["go_to_line"]),
                (self.go_to_def, self.tool_schemas["go_to_def"]),
                (self.bug_recurse_fuzz, self.tool_schemas["recurse_fuzz"]),
                (self.finalize, self.tool_schemas["finalize"]),
                (self.run_command, self.tool_schemas["run_command"]),
                (self.report_bug_ci, self.tool_schemas["report_bug_ci"]),
            ],
            ACIMode.FULL_SCAN_BUG_FINDER: [
                (self.scroll_down_file, self.tool_schemas["scroll_down_file"]),
                (self.scroll_up_file, self.tool_schemas["scroll_up_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.close_file, self.tool_schemas["close_file"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.find_phrase, self.tool_schemas["find_phrase"]),
                (self.go_to_line, self.tool_schemas["go_to_line"]),
                (self.go_to_def, self.tool_schemas["go_to_def"]),
                (self.pop_go_to_def, self.tool_schemas["pop_go_to_def"]),
                (self.add_question, self.tool_schemas["add_question"]),
                (self.resolve_question, self.tool_schemas["resolve_question"]),
                (self.add_memory, self.tool_schemas["add_memory"]),
                (self.bug_recurse_edit, self.tool_schemas["recurse"]),
                (self.bug_recurse_fuzz, self.tool_schemas["recurse_fuzz"]),
                (self.finalize, self.tool_schemas["finalize"]),
            ],
            ACIMode.FUZZ_GEN: [
                (self.scroll_down_file, self.tool_schemas["scroll_down_file"]),
                (self.scroll_up_file, self.tool_schemas["scroll_up_file"]),
                (self.switch_file, self.tool_schemas["switch_file"]),
                (self.close_file, self.tool_schemas["close_file"]),
                (self.open_file, self.tool_schemas["open_file"]),
                (self.list_files, self.tool_schemas["list_files"]),
                (self.find_phrase, self.tool_schemas["find_phrase"]),
                (self.go_to_line, self.tool_schemas["go_to_line"]),
                (self.go_to_def, self.tool_schemas["go_to_def"]),
                (self.find_references, self.tool_schemas["find_references"]),
                (
                    self.show_skeleton,
                    self.tool_schemas["show_skeleton"],
                ),
                (
                    self.symbol_search,
                    self.tool_schemas["symbol_search"],
                ),
                (
                    self.run_fuzzer,
                    self.tool_schemas["run_fuzzer"],
                ),
                (
                    self.finalize,
                    self.tool_schemas["finalize"],
                ),
            ],
        }

        if self._humanlayer_enabled:
            sets[ACIMode.DRIVER].extend(
                [
                    (
                        self.reach_out_to_human_for_assistance,
                        self.tool_schemas["reach_out_to_human_for_assistance"],
                    ),
                ]
            )

        for set, tools in sets.items():
            wrapped_tools = list(map(lambda t: (self._turn_wrapper(t[0]), t[1]), tools))

            sets[set] = wrapped_tools

        return sets

    def _lines_in_view(self) -> int:
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            return LINES_IN_VIEW_CONSTRAINED
        else:
            return LINES_IN_VIEW

    async def recurse(self, resp: dict[str, Any]) -> str:
        subtask = resp["subtask"]
        cache = self.cache

        self.recursion_depth += 1
        self._recursive_task = subtask

        if self.recursion_depth > 1:
            self._logger.info("AGENT RECURSION LIMIT REACHED, SKIPPING RECURSE")
            self.recursion_depth -= 1

            return "Recursion depth limit has been reached please try completing the task by yourself."

        self._logger.info(f"GOING RECURSIVE {subtask}")

        executor = self.tool_executors["recurse"]

        # NOP Switch to force recursive subtask into viewer state
        file = await cache.get("active_file")

        await self.manipulate(SwitchAction(file=file), file)

        cur_mode = self._mode
        await self._switch_to_mode(ACIMode.DRIVER, subtask)

        try:
            _response = await executor.process(self.cache, Semaphore(), subtask=subtask)  # type: ignore
        except Exception:
            self._logger.warning("Exception in recursive task.", exc_info=True)

        modified_files = await cache.get("output_modified_files", {})
        pairs = []

        for fn, content in modified_files.items():
            text = f"""
            <file fn={fn}>
            <before>
            {await self.file_rpc.read(fn) or "New file, no previous changes."}
            </before>
            <after>
            {content}
            </after>
            </file>
            """

            pairs.append(text)

        await cache.set("change_log", "\n".join(pairs))

        summary_exec = self.tool_executors["summary"]

        resp = await summary_exec.process(  # type: ignore
            self.cache, Semaphore(), input_message=subtask
        )

        await cache.set("change_log", "")

        self.recursion_depth -= 1
        self._recursive_task = None
        self.finalized = False
        self._attempted_finalize = False
        self._mode = cur_mode

        # Reset viewer_state to have the non recursive task in its most recent state
        file = await cache.get("active_file")

        await self.manipulate(SwitchAction(file=file), file)

        return "<recursion_result>\n" + resp["result"] + "</recursion_result>"

    def set_input_task(self, task: str) -> None:
        self._input_task = task

    async def set_starting_context(
        self, starting_context: dict[str, list[str]]
    ) -> None:
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
        run_tests_on_finalize=False,
        debug_mode=False,
        driver_mode=ACIMode.DRIVER,
        initial_turns=25,
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
            run_tests_on_finalize=run_tests_on_finalize,
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

    async def analyze_code(self, resp):
        cache = self.cache
        edits = await cache.get("last_edited_files", [])
        creates = await cache.get("last_created_files", [])

        files = edits + creates

        analysis_lines = await self._code_analysis(files)

        await cache.set("analysis_lines", analysis_lines)

        await cache.set("last_created_files", [])
        await cache.set("last_edited_files", [])

        action = RefreshAnalysisLines(lines=analysis_lines)

        active_file = await cache.get("active_file")

        return await self.manipulate(action=action, fn=active_file)

    async def _code_analysis(self, fns: list[str]):
        cache = self.cache
        output_modified_files = await cache.get("output_modified_files", {})
        analysis_lines = []

        feature_id = await cache.get("feature_id")
        feature = FeatureEntity.get(feature_id)
        assert feature is not None
        git_url = get_clone_url(feature)

        self._logger.debug("In code analysis stuff.")
        if self.file_rpc.has_pushed and not os.environ.get("DISABLE_LSP"):
            for fn in fns:
                    try:
                        analysis_lines.extend([
                            json.dumps(d.__dict__)
                            for d in (
                                await self._analysis_client.lsp_analyze(
                                    git_url,
                                    feature_id,
                                    feature.name,
                                    fn,
                                    output_modified_files,
                                )
                            ).diagnostics
                        ])
                        self._logger.debug(f"analyze lines: {analysis_lines}")
                    except Exception:
                        self._logger.warning("Exception in analysis", exc_info=True)
                        pass

        return analysis_lines


    def _take_turn(self, action: str):
        print(f"TURN {action}")

        if (
            action
            in (
                "create_files",
                "edit_files",
                "delete_files",
                "run_command",
                "recurse",
                "reach_out_to_human_for_assistance",
                "report_bug_ci",
                "resolve_question",
            )
            and self.mode.value == ACIExecutionMode.MULTI.value
        ):
            self._turns_remaining -= 1
            self._logger.debug(f"TURNS REMAINING {self._turns_remaining}")
            return

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
            return

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

    def set_pinned_files(self, pinned_files: dict[str, str]) -> None:
        self._pinned_files = pinned_files

    async def manipulate(self, action: ACIAction, fn: str) -> str:
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])

        if self.recursion_depth == 0:
            input_task = await cache.get("input_message")
        else:
            input_task = self._recursive_task

        output_modified_files = await cache.get("output_modified_files", {})
        analysis_lines = await cache.get("code_analysis", [])
        test_output = await cache.get("test_output")
        system_analysis = ""

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

            new_lines = lines

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

                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.ACI,
                        aci=ACIMessage(
                            action=ACIMessage.Action.TEST,
                            status=f"Ran tests for {fn}",
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

                text = unidecode(pattern.line_ending.join(viewer_lines))
                normalized_search = unidecode(action.lines_to_replace)
                normalized_replace = unidecode(action.replace_text)

                text = self.replace_closest_edit_distance(
                    text, normalized_search, normalized_replace
                )

                if not text:
                    self._turns_remaining += 1
                    return "Tried fuzzy replace and was not able to find a match, please double check the lines you are trying to replace."

                # Use normalized versions for replacement with consistent whitespace patterns
                try:
                    new_lines = text.split(pattern.line_ending)
                    new_content.extend(new_lines)
                    new_content.extend(end_chunk)

                    normalized_file_content = pattern.line_ending.join(
                        new_content
                    ).rstrip()

                    output_modified_files[fn] = unidecode(normalized_file_content)
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

        toolset = await self.toolsets()
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            mode = ACIMode.CONSTRAINED
        else:
            mode = self._mode
        toolset = toolset[mode]

        tool_names = "|".join([tool[1]["name"] for tool in toolset]) + "\n"

        if self._mode == ACIMode.FULL_SCAN_BUG_FINDER:
            viewer_state = BugFindingVisualizer.generate_viewer_state(
                files_with_id=files_with_id,
                fn=fn,
                lines_above=lines_above,
                lines_below=lines_below,
                new_lines=new_lines,
                current_questions=await self.cache.get("current_questions"),
                memories=await self.cache.get("bug_finding_memories", []),
                call_stack=await self.cache.get("go_to_def_stack", []),
                available_tools=tool_names,
            )
        elif self._mode == ACIMode.FUZZ_GEN:
            viewer_state = FuzzVisualizer.generate_viewer_state(
                input_task=input_task,
                files_with_id=files_with_id,
                fn=fn,
                lines_above=lines_above,
                lines_below=lines_below,
                new_lines=new_lines,
                test_output=test_output,
                available_tools=tool_names,
            )
        else:
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
                mode=self._mode,
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
            print("Tried to switch to already active file.")
            return f"You are already switched on to the file {file}. Please take a different action."

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

        await self.send_aci_status(f"Searching for code")

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

    async def find_phrase(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        phrase = resp["exact_match_phrase"]
        matches = [
            f"{fn}:{i}|{content}"
            for fn, i, content in await self.file_rpc.search(
                phrase, overlay_modified=True
            )
        ]

        active_file = await cache.get("active_file")
        analysis = textwrap.dedent(
            f"""
        <found_matches>
        {"\n".join(matches[:100]) + ("\n(limited to first 100 results)" if len(matches) > 100 else "")}
        </found_matches>
        """
        )

        self._logger.debug(f"FIND PHRASE: {phrase}")

        content = await self.manipulate(
            SystemAnalysisAction(system_analysis_output=analysis), active_file
        )

        return content

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

    async def run_single_command(
        self,
        command: str,
        image: Optional[str] = None,
        mount_dir: Optional[str] = None,
        timeout: int = 60,
    ) -> tuple[int, str]:
        self._logger.debug(f"RUNNING SINGLE COMMAND {command}")

        cache = self.cache
        modified_files = await cache.get("output_modified_files", {})

        if await cache.get("headless_command_execution", False):
            feature_id = await cache.get("feature_id")
            feature = FeatureEntity.get(feature_id)
            assert feature is not None
            git_url = await cache.get("repo_url")
            try:
                shell_resp = await self._analysis_client.shell(
                    git_url,
                    feature_id,
                    feature.name,
                    command,
                    overlay_files=modified_files,
                    image=image,
                    mount_dir=mount_dir,
                    timeout=timeout,
                )
            except:
                self._logger.exception("Error in shell execution.")
                return -123, "There was an internal error while running the command."

            # Proto -> WSMessage transform so all the below logic doesn't have to be duplicated
            cmd_resp = RunCommandResponse(
                exit_code=shell_resp.exit_code,
                output=shell_resp.output,
                modified_files=[
                    ChatModifiedFile(
                        name=mf.name,
                        projectPath=mf.project_path,
                        content=mf.content,
                        deleted=mf.deleted,
                    )
                    for mf in shell_resp.modified_files
                ],
            )

        else:
            chat_modified_files: list[ChatModifiedFile] = []
            for fn, content in modified_files.items():
                deleted = content == "BISMUTH_DELETED_FILE"

                if deleted:
                    print("DELETED_FN", fn)

                chat_modified_files.append(
                    ChatModifiedFile(
                        name=pathlib.Path(fn).name,
                        projectPath=fn,
                        content=content,
                        deleted=deleted,
                    )
                )

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.RUN_COMMAND,
                    run_command=RunCommandMessage(
                        command=command,
                        output_modified_files=chat_modified_files,
                    ),
                )
            )

            ws_resp = await self.recv_message_callback()
            assert ws_resp.type == WSMessageType.RUN_COMMAND_RESPONSE
            assert ws_resp.run_command_response is not None
            cmd_resp = ws_resp.run_command_response

        for mf in cmd_resp.modified_files:
            modified_files[mf.project_path] = (
                mf.content if not mf.deleted else "BISMUTH_DELETED_FILE"
            )
        await cache.set("output_modified_files", modified_files)

        for mf in cmd_resp.modified_files:
            async with cache.with_suffix(f"file_edit_selection_{mf.project_path}"):
                before = await cache.get("lines", None)
                if before is None:
                    continue
                adjust = len(mf.content.splitlines()) - len(before)
                await cache.set(
                    "lines_below", (await cache.get("lines_below")) + adjust
                )
                await cache.set("lines", mf.content.splitlines())

        def reduce_cli_output(raw_output):
            """
            Reduces CLI output that contains carriage returns to show only the final visible content of each line.
            """
            lines = raw_output.split("\n")
            result = []

            for line in lines:
                segments = line.split("\r")
                non_empty_segments = [s for s in segments if s.strip()]
                if non_empty_segments:
                    result.append(non_empty_segments[-1])
                else:
                    result.append(line)

            return "\n".join(result)

        output = reduce_cli_output(cmd_resp.output)

        return cmd_resp.exit_code, output

    async def get_test_config(self) -> Optional[BismuthTestTOML]:
        bismuth_toml = await self.file_rpc.read("bismuth.toml", overlay_modified=True)
        if bismuth_toml is None:
            return None
        try:
            config = BismuthTOML.model_validate(tomllib.loads(bismuth_toml))
            return config.test
        except:
            return None

    async def finalize(self, resp: dict[str, Any]) -> str:
        self._logger.debug("Finalizing...")
        cache = self.cache
        open_files = await cache.get("viewer_open_files", [])
        task_config = await cache.get("planned_task_config", {})
        if not self._attempted_finalize and self.recursion_depth == 0 and not self.mode.value == ACIExecutionMode.SINGLE.value:
            self._test_failure_count = 0
            self._attempted_finalize = True
            return f"Are you sure the users overall task is complete? Please confirm that you have completed the task before finalizing. Remember the task is:\n{self._input_task}"

        self._attempted_finalize = True

        test_cfg = await self.get_test_config()
        if test_cfg:
            await self.send_aci_status("Running test suite...")
            exit_code, test_out = await self.run_single_command(
                test_cfg.command,
                image=test_cfg.image,
                mount_dir=test_cfg.mount_dir,
                timeout=600,
            )
            await trace_output(test_out, "finalize_test_run")
            if exit_code != 0 and exit_code != -123:
                test_out = "\n".join(test_out.split("\n")[-500:])
                return f"Project test suite failed. This exact test command must pass: `{test_cfg.command}`\nRunning it currently fails with exit code: {exit_code}\nOutput:\n```\n{test_out}\n```"
        else:
            self._logger.info("No test config?")

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

            await self.update_change_log(resp["step"])
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

        await cache.set("last_created_files", [])

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

    async def switch_to_driver_mode(self, resp) -> str:
        if "results" not in resp:
            return "You must provide results for your subtask to switch back to the driver."

        self._logger.debug("SWITCHING TO DRIVER")
        await self.send_aci_status("Planning next step...")
        await self._switch_to_mode(self.driver_mode, resp["results"])

        return "Switched to driver mode."

    async def switch_to_navigation_mode(self, resp) -> str:
        if "goal" not in resp:
            return "You must provide a goal to switch to navigation mode."

        self._logger.debug("SWITCHING TO NAVIGATOR")
        await self.send_aci_status("Planning next step...")
        await self._switch_to_mode(ACIMode.NAVIGATE, resp["goal"])

        return "Switched to navigation mode."

    async def switch_to_editing_mode(self, resp) -> str:
        if "goal" not in resp:
            return "You must provide a goal to switch to editing mode."

        self._logger.debug("SWITCHING TO EDITOR")
        await self.send_aci_status("Planning next step...")
        await self._switch_to_mode(ACIMode.EDIT, resp["goal"])

        return "Switched to editing mode."

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

    async def _switch_to_mode(self, mode: ACIMode, communication):
        self._logger.debug(
            f"Switching to mode {mode} with communication {communication}"
        )
        self._mode = mode
        await self.cache.set("driver_subsystem_communications", communication)

    async def prompt_and_toolset_for_current_mode(self):
        if self.mode.value == ACIExecutionMode.SINGLE.value:
            mode = ACIMode.CONSTRAINED
        else:
            mode = self._mode

        input_task = self._input_task

        if self._recursive_task:
            input_task = self._recursive_task

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

    async def go_to_def(self, resp: dict[str, Any]) -> str:
        cache = self.cache
        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["go_to_def"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'go_to_def', please try again with the correct parameters."

        active_file = await cache.get("active_file")

        symbol = resp["symbol"]
        line = (
            int(resp["line"]) + 1
        )  # display line numbers are 0-indexed, lsp interface is 1-indexed
        self._logger.debug(f"GO TO DEF {symbol} {line}")
        current_contents = await self.file_rpc.read(active_file, overlay_modified=True)
        assert current_contents is not None
        col = current_contents.split("\n")[line].find(symbol)
        if col == -1:
            return f"The symbol {symbol} was not found on line {line}."

        git_url = await cache.get("repo_url")
        feature_id = await cache.get("feature_id")
        feature = FeatureEntity.get(feature_id)
        assert feature is not None

        try:
            res = await self._analysis_client.go_to_def(
                git_url,
                feature_id,
                feature.name,
                active_file,
                line,
                col,
                await cache.get("output_modified_files", {}),
            )
        except Exception:
            self._logger.warning("Exception in go_to_def", exc_info=True)
            res = None

        self._logger.debug(f"GO TO DEF: {symbol} = {res}")
        if res is None or not res.HasField("definition"):
            return "No definition found."

        def_file = res.definition.filename
        def_line = res.definition.line
        def_line -= 1

        await cache.set(
            "go_to_def_stack",
            (await cache.get("go_to_def_stack", [])) + [(symbol, active_file, line)],
        )

        open_files = await cache.get("viewer_open_files", [])
        if def_file not in open_files:
            open_files.append(def_file)
            await cache.set("viewer_open_files", open_files)

            def_file_contents = await self.file_rpc.read(def_file)
            assert def_file_contents is not None
            lines = def_file_contents.split("\n")

            # These will be immediately overwritten by the next manipulate call
            index = min(def_line + self._lines_in_view(), len(lines))
            lines_above = def_line
            lines_below = len(lines) - index

            async with cache.with_suffix(f"file_edit_selection_{def_file}"):
                await cache.set("lines_above", lines_above)
                await cache.set("lines_below", lines_below)
                await cache.set("index", index)
                await cache.set("lines", lines)

        content = await self.manipulate(JumpAction(line=def_line), def_file)

        return content

    async def pop_go_to_def(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        stack = await cache.get("go_to_def_stack", [])

        if not stack:
            return "No more definitions to pop."

        _, file, line = stack.pop()

        await cache.set("go_to_def_stack", stack)

        open_files = await cache.get("viewer_open_files", [])
        if file not in open_files:
            open_files.append(file)
            await cache.set("viewer_open_files", open_files)

            lines = (await self.file_rpc.read(file)).split("\n")  # type: ignore

            index = min(line + self._lines_in_view(), len(lines))
            lines_above = line
            lines_below = len(lines) - index

            async with cache.with_suffix(f"file_edit_selection_{file}"):
                await cache.set("lines_above", lines_above)
                await cache.set("lines_below", lines_below)
                await cache.set("index", index)
                await cache.set("lines", lines)

        return await self.manipulate(JumpAction(line=line), file)

    async def find_references(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["find_references"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'find_references', please try again with the correct parameters."

        active_file = await cache.get("active_file")

        symbol = resp["symbol"]
        line = (
            int(resp["line"]) + 1
        )  # display line numbers are 0-indexed, lsp interface is 1-indexed
        current_contents = await self.file_rpc.read(active_file, overlay_modified=True)
        assert current_contents is not None
        col = current_contents.split("\n")[line].find(symbol)
        if col == -1:
            return f"The symbol {symbol} was not found on line {line}."

        self._logger.debug(f"FIND REFS: {symbol}")

        git_url = await cache.get("repo_url")
        feature_id = await cache.get("feature_id")
        feature = FeatureEntity.get(feature_id)
        assert feature is not None

        try:
            res = list(
                (
                    await self._analysis_client.symbol_refs(
                        git_url,
                        feature_id,
                        feature.name,
                        active_file,
                        line,
                        col,
                        await cache.get("output_modified_files", {}),
                    )
                ).references
            )
        except Exception:
            self._logger.warning("Exception in find_references", exc_info=True)
            res = []

        analysis = textwrap.dedent(
            f"""
        <references>
        {"\n".join(f"{ref.filename}:{ref.line}" for ref in res)}
        </references>
        """
        )

        content = await self.manipulate(
            SystemAnalysisAction(system_analysis_output=analysis), active_file
        )

        return content

    async def reach_out_to_human_for_assistance(self, resp: dict[str, Any]) -> str:
        self._logger.debug("REACHING OUT TO HUMAN")

        message = resp["message"]

        from humanlayer import HumanLayer

        hl = HumanLayer()
        tool = hl.human_as_tool()

        hr = tool(message)

        return hr

    async def analyze_system(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["analyze_system"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'analyze_system', please try again with the correct parameters."

        active_file = await cache.get("active_file")

        await cache.set("test_failure_loop_count", 0)

        test_output = await cache.get("test_output")

        # test_output = test_output.split("<system_message>")[0]

        await cache.set("test_output", test_output)

        executor = self.tool_executors["analyze_system"]
        assert isinstance(executor, ACISystemAnalysisExecutor)

        semaphore = Semaphore()

        exec_response = await executor.process(cache, semaphore, focus=resp["focus"])

        if exec_response["status"] == "success":
            analysis = exec_response["result"]
        else:
            analysis = "Analysis failed, you're flying blind. Good luck!"

        # Add analysis to current viewer state
        content = await self.manipulate(
            SystemAnalysisAction(system_analysis_output=analysis), active_file
        )

        return content

    async def run_command(self, resp: dict[str, Any]) -> str:
        self._logger.debug(f"RUNNING COMMAND {resp['command']}")
        await self.send_aci_status(f"Running commands")

        if self.send_message_callback == null_send_callback:
            return "Running commands is not supported in this environment."

        command = resp["command"]
        test_cfg = await self.get_test_config()
        if test_cfg:
            image = test_cfg.image
            mount_dir = test_cfg.mount_dir
        else:
            image = None
            mount_dir = None
        exit_code, output = await self.run_single_command(
            command, image=image, mount_dir=mount_dir
        )

        out = f"exit code: {exit_code}\noutput:\n"
        out += "```\n"
        out += "\n".join(output.split("\n")[-500:])
        out += "\n```"
        if len(output.split("\n")) > 500:
            out += "\n(Output truncated to last 500 lines.)"

        return out

    async def report_bug_ci(self, resp: dict[str, Any]) -> str:
        self._logger.debug("REPORTING BUG")

        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["report_bug_ci"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'report_bug', please try again with the correct parameters."

        file = resp["file"]
        lines_to_replace = resp["lines_to_replace"].rstrip()
        replace_text = resp["replace_text"].rstrip()
        description = resp["bug_description"]
        grounding = resp.get("grounding", None)

        file_content = await self.file_rpc.read(file, overlay_modified=True)
        if file_content is None:
            return "The file you specified does not exist."

        if lines_to_replace not in file_content:
            return "The lines you specified to replace are not in the file."
        start_line = (
            file_content[: file_content.index(lines_to_replace)].count("\n") + 1
        )
        end_line = start_line + lines_to_replace.count("\n")

        await self.send_aci_status(f"Fixing a bug in {file}")

        issues = await cache.get("identified_bugs", [])
        for issue in issues:
            if issue["file"] == file and issue["start_line"] == start_line:
                return "An issue has already been reported on this line of code. If there are no other bugs to report, call finalize."

        if grounding is not None:
            if grounding == "fuzzing":
                grounding = {
                    "source": "fuzzing",
                    "code": await cache.get("fuzzing_code", None),
                }
            else:
                grounding = {"source": "test", "test": grounding}

        issues.append(
            {
                "file": file,
                "start_line": start_line,
                "end_line": end_line,
                "description": description,
                "suggested_fix": replace_text,
                "grounding": grounding,
            }
        )
        await cache.set("identified_bugs", issues)

        await self.update_change_log(description)

        if replace_text:
            active_file = await cache.get("active_file")

            if active_file != file:
                await self.switch_file(resp)

            try:
                action = EditAction(
                    lines_to_replace=lines_to_replace,
                    replace_text=replace_text,
                    file=file,
                )
                return await self.manipulate(action, active_file)
            except ValueError as e:
                self._logger.exception("Error editing file")
                return str(e)

        return "Bug reported. Continue to report other potential issues, or call finalize if done."

    async def add_question(self, resp: dict[str, Any]) -> str:
        self._logger.debug("ADDING QUESTION")

        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["add_question"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'add_question', please try again with the correct parameters."

        question = resp["question"]

        questions = await cache.get("current_questions", [])
        questions.append(question)
        await cache.set("current_questions", questions)

        return "Question added. Begin to explore the codebase to find the answer."

    async def resolve_question(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["resolve_question"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'resolve_question', please try again with the correct parameters."

        question_id = resp["question_id"]
        answer = resp["answer"]

        questions = await cache.get("current_questions", [])

        if question_id >= len(questions) or question_id < 0:
            return "Invalid question id. Please provide a valid question id."

        self._logger.debug(
            f"resolve question {question_id} ({questions[question_id]}): {answer}"
        )

        questions = questions[:question_id] + questions[question_id + 1 :]
        await cache.set("current_questions", questions)

        if questions:
            return "Question resolved. Current questions:\n" + "\n".join(
                f"{i}: {q}" for i, q in enumerate(questions)
            )
        else:
            raise StopAsyncIteration("All questions resolved.")

    async def add_memory(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["add_memory"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'add_memory', please try again with the correct parameters."

        memory = resp["memory"]
        self._logger.debug(f"ADDING MEMORY {memory}")
        await self.send_aci_status("Adding memory")

        memories = await cache.get("bug_finding_memories", [])
        memories.append(memory)
        await cache.set("bug_finding_memories", memories)

        return "Memory added. Continue to explore the codebase to answer the questions."

    async def bug_recurse_edit(self, resp: dict[str, Any]) -> str:
        """
        "Specialization" of recurse for full scan bug finding. Drops into edit mode instead of driver, and accumulates each call of this into a list of commits.
        """
        subtask = resp["subtask"]
        cache = self.cache

        if "judge" in self.tool_executors:
            judge = self.tool_executors["judge"]
            if not await judge.process(cache, Semaphore(), subtask=subtask):  # type: ignore
                return "This task is not something that should be done. Remember that you should not perform very large refactorings or changes to the codebase, only fix logic errors, obviously incorrect behavior, or user-facing security vulnerabilities."

        self.recursion_depth += 1
        self._recursive_task = subtask

        if self.recursion_depth > 1:
            self._logger.info("AGENT RECURSION LIMIT REACHED, SKIPPING RECURSE")
            self.recursion_depth -= 1

            return "Recursion depth limit has been reached please try completing the task by yourself."

        self._logger.info(f"GOING RECURSIVE {subtask}")
        await self.send_aci_status("Creating a PR for a bug")

        executor = self.tool_executors["recurse"]

        # NOP Switch to force recursive subtask into viewer state
        file = await cache.get("active_file")

        await self.manipulate(SwitchAction(file=file), file)

        cur_mode = self._mode
        await self._switch_to_mode(ACIMode.EDIT, subtask)

        try:
            _response = await executor.process(  # type: ignore
                self.cache,
                Semaphore(),
                subtask=subtask
                + ". DO NOT write tests or attempt to build the project, only edit the code.",
            )
        except Exception:
            self._logger.warning(
                "Error running recursive bug fix edit task", exc_info=True
            )

        modified_files = await cache.get("output_modified_files", {})
        await cache.set(
            "commits",
            await cache.get("commits")
            + [{"message": subtask, "files": modified_files}],
        )

        self.recursion_depth -= 1
        self._recursive_task = None
        self.finalized = False
        self._attempted_finalize = False
        self._mode = cur_mode

        # Reset viewer_state to have the non recursive task in its most recent state
        file = await cache.get("active_file")

        await self.manipulate(SwitchAction(file=file), file)

        return "Edit task completed. Continue to explore the codebase to answer the questions."

    async def bug_recurse_fuzz(self, resp: dict[str, Any]) -> str:
        target = resp["target"]
        cache = self.cache

        self.recursion_depth += 1

        if self.recursion_depth > 1:
            self._logger.info("AGENT RECURSION LIMIT REACHED, SKIPPING RECURSE")
            self.recursion_depth -= 1

            return "Recursion depth limit has been reached please try completing the task by yourself."

        self._logger.info(f"GOING RECURSIVE FUZZ {target}")

        viewer_history = await cache.get("viewer_history", [])
        await cache.set("viewer_history", [])

        executor = self.tool_executors["fuzz"]

        cur_mode = self._mode
        await self._switch_to_mode(ACIMode.FUZZ_GEN, target)

        try:
            result = await executor.process(  # type: ignore
                self.cache,
                Semaphore(),
                target_symbol=target,
            )
        except Exception:
            self._logger.warning("Error running fuzzer executor", exc_info=True)
            result = {"status": "error"}

        self.recursion_depth -= 1
        self._recursive_task = None
        self.finalized = False
        self._attempted_finalize = False
        self._mode = cur_mode

        await cache.set("viewer_history", viewer_history)

        # Reset viewer_state to have the non recursive task in its most recent state
        file = await cache.get("active_file")

        await self.manipulate(SwitchAction(file=file), file)

        if result.get("status") == "error":
            return "An error occurred while trying to run the fuzzer: " + result.get(
                "message", "unknown internal failure"
            )

        return (
            "Fuzzing completed. Here is the run output:\n\n"
            + result.get("result", "")
            + "\nIf this appears to show a valid bug, report and fix it. Specifically mention that it was found by fuzzing."
        )

    async def run_fuzzer(self, resp: dict[str, Any]) -> str:
        cache = self.cache

        passed, failing_key = self.validate_llm_call(
            resp, self.tool_schemas["run_fuzzer"]["input_schema"]
        )

        if not passed:
            return f"{failing_key} is required for 'run_fuzzer', please try again with the correct parameters."

        await self.send_aci_status("Running a fuzzer")

        fuzzer = resp["code"]
        await cache.set("fuzz_code", fuzzer)

        feature_id = await cache.get("feature_id")
        feature = FeatureEntity.get(feature_id)
        assert feature is not None
        git_url = await cache.get("repo_url")

        try:
            fuzz_resp = await self._analysis_client.fuzz(
                git_url, feature_id, feature.name, fuzzer
            )
        except Exception:
            logging.exception("Error running fuzzer")
            return "An internal error occurred while trying to run the fuzzer. Call finalize and do not attempt to fuzz again."

        active_file = await cache.get("active_file")
        content = await self.manipulate(
            TestAction(test_output=fuzz_resp.output), active_file
        )

        await cache.set("fuzz_output", fuzz_resp.output)

        return content
