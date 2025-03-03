import textwrap
from asyncio import Semaphore
from typing import Any, List

from asimov.caches.cache import Cache
from asimov.graph import (
    AgentModule,
    FlowControlConfig,
    FlowControlModule,
    FlowDecision,
    ModuleConfig,
    ModuleType,
    Node,
    NodeConfig,
)

from daneel.agents.common import StreamingMiddleware, SubGraphEntryNode
from daneel.constants import MODEL
from daneel.data.postgres.models import ChatMessageEntity
from daneel.executors.graph_chat_executor import ChatExecutor


class EntryNode(SubGraphEntryNode):
    async def run(self, cache: Cache, semaphore: Semaphore) -> Any:
        system_prompt = textwrap.dedent(
            """
            You are a helpful and hardworking, rockstar software developer AI named Bismuth and your job is to build software applications for different users.
            Be kind and thorough implementing their entire task. This user isn't asking you for a task, just having a friendly chat. 
            If the user has a question about how to run code you produce, direct them to use the /docs command.
            You will be provided context from the user's project between <CONTEXT> and </CONTEXT> if said context exists, this will help you understand the current state of the files for editing. The code in these files will have each line prepended with the line number for easy referencing. Additionally you may optionally be provided reflections on past attempts at solving the problem between <MEMORY> and </MEMORY> these reflections will help you overcome your past errors in attempting to answer normal questions the user may have.
            """
        ).strip()
        await cache.set("system_prompt", system_prompt)
        return {"status": "success"}


class FinalizeMessageNode(AgentModule):
    async def process(self, cache: Cache, semaphore: Semaphore) -> Any:
        full_message = await cache.get("prefill")

        msg_ent = ChatMessageEntity(
            is_ai=True,
            content=full_message,
            session_id=await cache.get("msg_session_id"),
            request_id=await cache.get("request_id"),
        ).persist()
        print("persisted message", msg_ent.id)

        return {
            "status": "success",
            "finished": True,
            "generated_text": full_message,
            "msg_id": msg_ent.id,
        }


def create_flow_control(name: str):
    flow_config = FlowControlConfig(
        decisions=[
            FlowDecision(
                next_node="FinalizeNode_Basic",
                condition="chat_finished == true",
                condition_variables=["chat_finished"],
                cleanup_on_jump=True,
            ),
        ],
        default="ChatNode_Basic",
        cleanup_on_default=False,
    )

    return FlowControlModule(
        name=name,
        type=ModuleType.FLOW_CONTROL,
        config=ModuleConfig(),
        flow_config=flow_config,
    )


async def setup_subgraph(
    deps: List[str], cache: Cache, send_message_callback, inference_client_factory
) -> List[Node]:
    node_config = NodeConfig(max_visits=10000)
    streaming_middleware = StreamingMiddleware(
        send_message_callback=send_message_callback,
    )

    entrypoint = EntryNode(
        name="BasicChatSubGraph",
        dependencies=deps,
        node_config=NodeConfig(max_visits=10000),
    )

    chat_executor = ChatExecutor(
        name="ChatExec_Basic",
        type=ModuleType.EXECUTOR,
        config=ModuleConfig(middlewares=[streaming_middleware]),
        inference_client=inference_client_factory(MODEL),
    )

    chat_node = Node(
        name="ChatNode_Basic",
        modules=[chat_executor],
        node_config=node_config,
        dependencies=["BasicChatSubGraph"],
    )

    flow_control_node = Node(
        name="FlowControlNode_Basic",
        modules=[create_flow_control("ChatFlow_Basic")],
        node_config=node_config,
        dependencies=["ChatNode_Basic"],
    )

    finalize_message = FinalizeMessageNode(
        name="FinalizeMessage_Basic",
        type=ModuleType.EXECUTOR,
        config=ModuleConfig(middlewares=[streaming_middleware]),
    )

    finalize_node = Node(
        name="FinalizeNode_Basic",
        modules=[finalize_message],
        node_config=node_config,
        dependencies=["FlowControlNode_Basic"],
    )

    return [entrypoint, chat_node, flow_control_node, finalize_node]
