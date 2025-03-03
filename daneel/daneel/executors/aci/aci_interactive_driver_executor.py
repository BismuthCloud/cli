import json
import textwrap
import traceback
from asyncio import Semaphore, create_task, sleep
from typing import TYPE_CHECKING, Any, Awaitable, Callable

import numpy as np
from asimov.caches.cache import Cache
from asimov.graph import AgentModule
from asimov.services.inference_clients import (
    ChatMessage,
    ChatRole,
    InferenceClient,
    RetriesExceeded,
)
from jinja2 import Template
from pydantic import Field

from daneel.constants import BIG_MODEL
from daneel.data.graph_rag.hybrid_search import ENABLE_EMBEDDING, Code
from daneel.services.tracing_inference_client import CreditsExhausted

if TYPE_CHECKING:
    from daneel.executors.aci import ACI

from daneel.data.file_rpc import FileRPC
from daneel.data.postgres.models import ChatSessionEntity
from daneel.utils import extract_tagged_content
from daneel.utils.tracing import trace_output
from daneel.utils.websockets import ACIMessage, WSMessage, WSMessageType

SYSTEM_PROMPT = """
You are a staff software engineer level AI tasked with reviewing and editing code files to solve specific problems.

Previously during this session you opened these files:
{{ opened_files }}

They may not be open now, but you can open them again.

You also previously edited these files:
{{ edited_files }}

And you created these files:
{{ created_files }}

Finally, you deleted these files:
{{ deleted_files }}

These files may have been modified or deleted since you last interacted with them, but may be useful for completing your task.
"""


class ACIDriverExecutor(AgentModule):
    inference_client_factory: Callable[[str], InferenceClient]
    send_message_callback: Callable[[WSMessage], Awaitable[None]]
    recv_message_callback: Callable[[], Awaitable[WSMessage]]
    file_rpc: FileRPC
    aci: Any = Field(default=None)
    human_layer_enabled: bool = Field(default=False)

    async def _get_key_facts(
        self, cache: Cache, inference_client: InferenceClient, input_message: str
    ):
        past_messages = await cache.get("formatted_chat_messages", [])

        kept_text = ""

        for msg in past_messages:
            kept_text += msg.content + "\n"

        key_text = ""

        if kept_text:
            key_text += kept_text + "\n"

        key_text += input_message

        msg = textwrap.dedent(
            f"""
        <text>
        {key_text}
        </text

        <query>
        {input_message}
        </query>

        You are a friendly AI who is very good at picking out key facts and actions from a set of text. Given the text about between the text tags pick out the key facts as they relate to things like actions taken, programming, facts about a codebase, errors, user commentary, comments, issues etc.
        Also you've been provided a query which is the current user request, really think about how the key facts relate to that query and try to pick things important to it specifically.

        Think step by step and put your thinking in thinking tags.

        Put your output between output tags each fact should be its own bullet in the output.
        """
        )

        res = await inference_client.get_generation(
            [ChatMessage(role=ChatRole.USER, content=msg)],
            max_tokens=4096,
            temperature=0.0,
        )

        print("HERE KEY FACTS")
        print(res)

        key_facts = extract_tagged_content(res, "output")

        session_id = await cache.get("msg_session_id")
        chat_session = ChatSessionEntity.get(session_id)
        assert chat_session is not None
        session_context = chat_session.get_context()

        session_context["key_facts"] = key_facts

    async def process(self, cache: Cache, semaphore: Semaphore, **kwargs) -> Any:
        await cache.set("locators", [])
        task_config = {
            "code_model": BIG_MODEL,
            "command_execution_enabled": False,
            "build_lint_test_execution_enabled": False,
            "human_assistance_enabled": False,
            "run_lint_test_build_once": True,
            "get_additional_context": True,
        }

        await cache.set("planned_task_config", task_config)

        inference_client = self.inference_client_factory(str(task_config["code_model"]))

        aci: "ACI" = self.aci
        input_task = await cache.get("input_message")

        # Facilitate recurse
        input_task = kwargs.get("subtask", input_task)

        create_task(self._get_key_facts(cache, inference_client, input_task))

        starting_context: dict[str, list[str]] = await cache.get(
            "unmodified_context", {}
        )
        context_weights: dict[str, float] = {fn: 0.0 for fn in starting_context}

        repo_files = await self.file_rpc.list(overlay_modified=True)

        test_context: dict[str, list[str]] = {}

        session_id = await cache.get("msg_session_id")
        chat_session = ChatSessionEntity.get(session_id)
        assert chat_session is not None
        session_context = chat_session.get_context()
        mode = session_context.get("mode", "single")

        if facts := session_context.get("key_facts", None):
            expanded_message = textwrap.dedent(
                f"""
                These are key facts from our previous conversation, use them to inform yourself on the current task:
                <key_facts>
                {facts}
                </key_facts>

                Current Task:
                <current_task>
                {input_task}
                <current_task>
            """
            )

            await cache.set("input_message", expanded_message)

        # Empty starting_context values just opens to the top
        for fn in session_context.get("opened_files", []):
            if fn in repo_files:
                starting_context[fn] = []

        if await cache.get("aci_seed_tests", True):
            for key in repo_files:
                for fn, c in starting_context.items():
                    part = fn.split("/")[-1].split(".")[0]
                    if "test_" + part in key or part + "_test" in key:
                        content = await self.file_rpc.read(key, overlay_modified=True)
                        if content is None:
                            continue
                        print(f"Adding test {key} to open files.")
                        test_context[key] = []

        starting_context.update(test_context)

        if ENABLE_EMBEDDING:
            try:
                session_context.setdefault("opened_files", [])
                session_context.setdefault("created_files", [])
                session_context.setdefault("edited_files", [])

                files_to_filter = set(
                    session_context["opened_files"]
                    + session_context["created_files"]
                    + [x["file"] for x in session_context["edited_files"]]
                )
                contents = [
                    (fn, c)
                    for fn, c in [
                        (fn, await self.file_rpc.read(fn, overlay_modified=True))
                        for fn in files_to_filter
                    ]
                    if c is not None
                ]
                embeddings = [
                    e
                    async for e in Code.embed(
                        [content for _, content in contents],
                        input_type="RETRIEVAL_DOCUMENT",
                    )
                ]
                task_embed = await anext(
                    Code.embed([input_task], input_type="RETRIEVAL_QUERY")
                )
                assert task_embed is not None
                irrelevant_files = []
                for (fn, _), embed in zip(contents, embeddings):
                    if embed is not None and (
                        np.dot(embed, task_embed)
                        / (np.linalg.norm(embed) * np.linalg.norm(task_embed))
                        < 0.3
                    ):
                        irrelevant_files.append(fn)

                session_context["opened_files"] = [
                    fn
                    for fn in session_context["opened_files"]
                    if fn not in irrelevant_files
                ]
                session_context["created_files"] = [
                    fn
                    for fn in session_context["created_files"]
                    if fn not in irrelevant_files
                ]
                session_context["edited_files"] = [
                    x
                    for x in session_context["edited_files"]
                    if x["file"] not in irrelevant_files
                ]
                starting_context = {
                    fn: c
                    for fn, c in starting_context.items()
                    if fn not in irrelevant_files
                }

            except Exception as e:
                print("Failed to filter files by embedding")
                traceback.print_exc()

        if await cache.get("aci_seed_search", True):
            resp = {"query": input_task}
            response = await aci.symbol_search(resp)
            search_results = json.loads(response)
            print(
                "Seeding ACI with search results in files",
                {res["file"] for res in search_results[:10]},
            )

            for res in search_results[:10]:
                if res["file"] in repo_files:
                    starting_context.setdefault(res["file"], []).append(res["content"])
                    context_weights[res["file"]] = max(
                        context_weights.get(res["file"], 0.0), res["weight"]
                    )

        pinned_files_context: dict[str, str] = {}
        for fn in session_context.get("pinned_files", []):
            if fn in repo_files:
                starting_context[fn] = []

                context = await self.file_rpc.read(fn, overlay_modified=True)
                if context is not None:
                    pinned_files_context[fn] = context

        aci.set_pinned_files(pinned_files_context)

        if aci.recursion_depth == 0:
            for fn in starting_context:
                if fn not in context_weights:
                    context_weights[fn] = 0.0
            ordered_context = {
                fn: starting_context[fn]
                for fn, _ in sorted(
                    context_weights.items(), key=lambda x: x[1], reverse=True
                )
            }

            await aci.set_starting_context(ordered_context)
            aci.set_input_task(input_task)

        prompt, tools, _ = await aci.prompt_and_toolset_for_current_mode()

        history = [
            ChatMessage(
                role=ChatRole.SYSTEM,
                content=Template(SYSTEM_PROMPT).render(**session_context),
                cache_marker=True,
            ),
            ChatMessage(role=ChatRole.USER, content=prompt, cache_marker=True),
        ]

        # Called by tool_chain with incremental tool call data as it gets it from streaming inference
        async def incremental_status_middleware(resp):
            if resp["type"] == "tool_use":
                file = resp["input"].get("file")
                if not file:
                    return

                if resp["name"] == "create_file":
                    await self.send_message_callback(
                        WSMessage(
                            type=WSMessageType.ACI,
                            aci=ACIMessage(
                                action=ACIMessage.Action.STATUS,
                                status=f"Creating {file}...",
                            ),
                        )
                    )
                elif resp["name"] == "edit_file":
                    await self.send_message_callback(
                        WSMessage(
                            type=WSMessageType.ACI,
                            aci=ACIMessage(
                                action=ACIMessage.Action.STATUS,
                                status=f"Editing {file}...",
                            ),
                        )
                    )

        result = ""

        while True:
            greeter_ran = await cache.get("greeter_ran", False)
            if greeter_ran:
                break

            await sleep(0.1)

        try:
            result = await inference_client.tool_chain(
                history,
                tools=tools,
                temperature=0.3,
                max_iterations=aci.initial_turns * 3,
                max_tokens=8192,
                tool_choice="any",
                middlewares=[incremental_status_middleware],
                mode_swap_callback=aci.prompt_and_toolset_for_current_mode,
            )

            finalized = aci.finalized
        except RetriesExceeded:
            print("Retries exceeded!")
            finalized = False
        except CreditsExhausted:
            print("Credits exhausted!")
            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.ACI,
                    aci=ACIMessage(action=ACIMessage.Action.END, status=""),
                )
            )
            raise
        except Exception as e:
            print("Unhandled exception in tool chain")
            traceback.print_exc()
            finalized = False

        if not finalized and aci.recursion_depth == 0:
            print("Root ACI exited without finalizing. Manually finalizing...")
            try:
                await aci.finalize({})
            except StopAsyncIteration:
                pass
            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.ACI,
                    aci=ACIMessage(action=ACIMessage.Action.END, status=""),
                )
            )
            return {"status": "success", "result": "ACI session finished"}

        if aci.recursion_depth == 0:
            print("Closing ACI session")
            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.ACI,
                    aci=ACIMessage(action=ACIMessage.Action.END, status=""),
                )
            )
        else:
            print("Exiting recursive ACI session")

        await trace_output(result, "aci_result")
        return {"status": "success", "result": result}
