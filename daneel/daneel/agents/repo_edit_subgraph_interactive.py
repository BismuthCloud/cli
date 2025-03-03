import asyncio
import logging
import os
import textwrap
from asyncio import Semaphore
from typing import Any, List

from asimov.caches.cache import Cache
from asimov.graph import (
    AgentModule,
    ModuleConfig,
    ModuleType,
    Node,
    NodeConfig,
    SnapshotControl,
)
from asimov.services.inference_clients import ChatMessage, ChatRole, InferenceClient
from pydantic import PrivateAttr

from daneel.agents.common import (
    ResponseStateHandler,
    StreamingMiddleware,
    SubGraphEntryNode,
)
from daneel.constants import BIG_MODEL, MODEL, SMALL_MODEL
from daneel.data.file_rpc import FileRPC
from daneel.data.postgres.models import ChatMessageEntity
from daneel.executors.aci import ACI
from daneel.executors.aci.aci_interactive_driver_executor import ACIDriverExecutor
from daneel.executors.greeter_executor import GreeterExecutor
from daneel.executors.summary_executor import SummaryExecutor
from daneel.utils import extract_tagged_content


class EntryNode(SubGraphEntryNode):
    async def run(self, cache: Cache, semaphore: Semaphore) -> Any:
        system_prompt = textwrap.dedent(
            """

            """
        ).strip()

        await cache.set("system_prompt", system_prompt)

        return {"status": "success"}


class FinalizeMessageNode(AgentModule):
    _logger: logging.Logger = PrivateAttr()

    async def process(self, cache: Cache, semaphore: Semaphore) -> Any:
        self._logger = logging.getLogger(__name__).getChild(
            await cache.get("request_id")
        )

        full_message = await cache.get("generated_text", "")

        msg_ent = ChatMessageEntity(
            is_ai=True,
            content=full_message,
            session_id=await cache.get("msg_session_id"),
        ).persist()

        self._logger.info(f"Finalized message: {repr(full_message)}")

        return {
            "status": "success",
            "finished": True,
            "generated_text": full_message,
            "msg_id": msg_ent.id,
        }


class CommitMessageGeneratorNode(AgentModule):
    inference_client: InferenceClient
    file_rpc: FileRPC

    async def _process(self, cache: Cache, semaphore: Semaphore):
        change_log = await cache.get("change_log", [])
        input_text = "\n".join(change_log)

        commit_message = await self.inference_client.get_generation(
            [
                ChatMessage(
                    role=ChatRole.SYSTEM,
                    content="You are an AI that creates git commit messages that describe changes made in a chat message. The entire chat message will be wrapped in a <CHAT> tag.",
                ),
                ChatMessage(
                    role=ChatRole.USER,
                    content=textwrap.dedent(
                        f"""
                        {input_text}

                        Using the summaries of changes above produce an accurate commit message, please produce a single commit message for all changes between a single pair of <commit_message> tags.
                        """
                    ),
                ),
            ],
            max_tokens=256,
        )

        try:
            commit_message = extract_tagged_content(commit_message, "commit_message")[0]
        except IndexError as e:
            pass

        print("Generated commit message:", commit_message)
        await cache.set("commit_message", commit_message)

        return {"status": "success"}

    async def process(self, cache: Cache, semaphore: Semaphore):
        try:
            return await asyncio.wait_for(self._process(cache, semaphore), timeout=60)
        except asyncio.TimeoutError:
            logging.warning("timeout in inner commit message gen")
            change_log = await cache.get("change_log", [])
            await cache.set("commit_message", "\n".join(f"* {m}" for m in change_log))
            return {"status": "success"}


async def create_code_module(
    id: int,
    inference_client_factory,
    send_message_callback,
    recv_message_callback,
    cache,
    file_rpc,
):

    aci: ACI = await ACI.create(
        cache,
        inference_client_factory(BIG_MODEL),
        inference_client_factory,
        file_rpc,
        send_message_callback=send_message_callback,
        recv_message_callback=recv_message_callback,
        interactive_mode=True,
        initial_turns=int(os.environ.get("ACI_INTERACTIVE_MAX_TURNS", 40)),
    )
    driver = ACIDriverExecutor(
        name=f"CodeGenExec_{id}",
        type=ModuleType.EXECUTOR,
        inference_client_factory=inference_client_factory,
        config=ModuleConfig(
            timeout=3600,
        ),
        send_message_callback=send_message_callback,
        recv_message_callback=recv_message_callback,
        file_rpc=file_rpc,
        aci=aci,
    )

    return driver


async def setup_core_aci_subgraph(
    deps: List[str],
    cache: Cache,
    send_message_callback,
    recv_message_callback,
    inference_client_factory,
    file_rpc,
) -> List[Node]:
    PARALLELISM = int(os.environ.get("PARALLELISM", 1))

    core_node = Node(
        name="CoreNode",
        modules=[
            *[
                await create_code_module(
                    x,
                    inference_client_factory,
                    send_message_callback,
                    recv_message_callback,
                    cache,
                    file_rpc=file_rpc,
                )
                for x in range(PARALLELISM)
            ],
        ],
        node_config=NodeConfig(max_visits=10000, parallel=True),
        config=ModuleConfig(timeout=300),
        dependencies=deps,
        snapshot=SnapshotControl.ALWAYS,
        trace=True,
    )

    return [core_node]


async def setup_subgraph(
    deps: List[str],
    cache: Cache,
    send_message_callback,
    recv_message_callback,
    inference_client_factory,
    file_rpc,
) -> List[Node]:
    # Create streaming middleware
    streaming_middleware = StreamingMiddleware(
        send_message_callback=send_message_callback,
        parallelism=1,
    )

    response_state_handler = ResponseStateHandler(
        cache=cache, send_message_callback=send_message_callback
    )

    node_config = NodeConfig(max_visits=10000)

    entry_point = EntryNode(name="RepoEditSubGraph", dependencies=deps)

    greeter_executor = GreeterExecutor(
        name="GreetExec",
        type=ModuleType.EXECUTOR,
        config=ModuleConfig(
            context={
                "request_id": await cache.get("request_id"),
                "response_state_handler": response_state_handler,
            },
            middlewares=[streaming_middleware],
        ),
        inference_client=inference_client_factory(SMALL_MODEL),
    )

    core_nodes = await setup_core_aci_subgraph(
        [entry_point.name],
        cache,
        send_message_callback,
        recv_message_callback,
        inference_client_factory,
        file_rpc,
    )
    core_nodes[0].modules.append(greeter_executor)

    gen_commit_msg = CommitMessageGeneratorNode(
        name="CommitMessageGenerator",
        type=ModuleType.EXECUTOR,
        inference_client=inference_client_factory(MODEL),
        file_rpc=file_rpc,
    )

    summary_executor = SummaryExecutor(
        name="SummaryExec",
        type=ModuleType.EXECUTOR,
        config=ModuleConfig(middlewares=[streaming_middleware]),
        inference_client=inference_client_factory(MODEL),
    )

    gen_commit_msg_node = Node(
        name="CommitMessageGeneratorNode",
        modules=[gen_commit_msg, summary_executor],
        node_config=NodeConfig(max_visits=10000, max_retries=1, parallel=True),
        dependencies=[core_nodes[-1].name],
        snapshot=SnapshotControl.NEVER,
    )

    finalize_message = FinalizeMessageNode(
        name="FinalizeMessage",
        type=ModuleType.EXECUTOR,
        config=ModuleConfig(middlewares=[streaming_middleware]),
    )

    finalize_node = Node(
        name="FinalizeNode",
        modules=[finalize_message],
        node_config=node_config,
        dependencies=["CommitMessageGeneratorNode"],
        snapshot=SnapshotControl.ALWAYS,
        trace=True,
    )

    return [
        entry_point,
        *core_nodes,
        gen_commit_msg_node,
        finalize_node,
    ]
