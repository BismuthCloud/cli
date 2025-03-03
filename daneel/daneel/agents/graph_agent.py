import os

from asimov.caches.cache import Cache
from asimov.graph import Agent, NodeConfig

from daneel.agents.basic_chat_subgraph import (
    setup_subgraph as setup_basic_chat_subgraph,
)
from daneel.agents.repo_edit_subgraph_interactive import (
    setup_subgraph as setup_repo_edit_subgraph,
)
from daneel.data.postgres.models import ChatSessionEntity

node_config = NodeConfig(max_visits=40000)


async def setup_interactive_agent(
    cache: Cache,
    send_message_callback,
    recv_message_callback,
    inference_client_factory,
    file_rpc,
    session_id=None,
):
    agent = Agent(
        cache=cache,
        max_total_iterations=40000,
        auto_snapshot="AUTO_SNAPSHOT" in os.environ,
    )

    mode = "single"

    if session_id:
        chat_session = ChatSessionEntity.get(session_id)
        assert chat_session is not None
        session_context = chat_session.get_context()
        mode = session_context.get("mode", "single")

    if mode == "single":
        repo_edit_subgraph = await setup_repo_edit_subgraph(
            deps=[],
            cache=cache,
            send_message_callback=send_message_callback,
            recv_message_callback=recv_message_callback,
            inference_client_factory=inference_client_factory,
            file_rpc=file_rpc,
        )
        agent.add_multiple_nodes(repo_edit_subgraph)
    elif mode == "chat":
        basic_chat_subgraph = await setup_basic_chat_subgraph(
            deps=[],
            cache=cache,
            send_message_callback=send_message_callback,
            inference_client_factory=inference_client_factory,
        )
        agent.add_multiple_nodes(basic_chat_subgraph)

    return agent
