import asyncio
from contextlib import asynccontextmanager
from datetime import datetime
from enum import Enum
import functools
import json
import math
import pathlib
import textwrap
import aiohttp
import opentelemetry.instrumentation.aiohttp_client
import jwt
import os
import shutil
import tomllib
import logging
from uuid import uuid4
from typing import Dict, Optional
from keycloak import KeycloakAdmin, KeycloakOpenIDConnection

from daneel.data.graph_rag.code_indexing import ast_parse
from daneel.utils.glob_match import path_matches
from daneel.utils.repo import clone_repo

import opentelemetry.trace

from daneel.constants import MODEL
from daneel.data.file_rpc import FileRPC
from daneel.services.tracing_inference_client import (
    CreditsExhausted,
    TracingInferenceClient,
)
from daneel.services import billing
from daneel.utils import (
    create_anthropic_inference_client,
    create_bedrock_inference_client,
    create_openrouter_inference_client,
    create_google_gemini_api_client,
    mask_context_messages,
)

from pydantic import BaseModel, ValidationError

from daneel.data.graph_rag import GraphRag, hybrid_search
from fastapi import FastAPI, WebSocket, HTTPException, APIRouter
import opentelemetry.instrumentation.fastapi
from sse_starlette.sse import EventSourceResponse
from starlette.websockets import WebSocketDisconnect

from asimov.graph.tasks import Task, TaskStatus
from asimov.graph import Agent
from asimov.caches.cache import Cache
from asimov.caches.redis_cache import RedisCache
from asimov.services.inference_clients import (
    ChatMessage,
    ChatRole,
)

from daneel.data.bismuth_config import BismuthChatTOML, BismuthTOML
from daneel.utils.websockets import (
    AuthMessage,
    ChatMessage as WebsocketChatMessage,
    ChatModifiedFile,
    RunCommandResponse,
    WSMessage,
    WSMessageType,
)
from daneel.agents.graph_agent import setup_interactive_agent
from daneel.data.postgres.models import (
    APIKeyEntity,
    ChatMessageEntity,
    ChatSessionEntity,
    FeatureEntity,
    UserEntity,
    GenerationTraceEntity,
)
from daneel.services.code_analysis import SourceFile
from daneel.utils.tracing import request_id as tracing_request_id

opentelemetry.instrumentation.aiohttp_client.AioHttpClientInstrumentor().instrument()

if sentry_dsn := os.environ.get("SENTRY_DSN"):
    import sentry_sdk

    sentry_sdk.init(
        dsn=sentry_dsn,
        traces_sample_rate=0.0,
        profiles_sample_rate=0.0,
    )

logging.basicConfig(
    format="%(asctime)s - %(levelname)s - %(name)s - %(message)s", level=logging.DEBUG
)
# logging.getLogger("git").setLevel(logging.INFO)
logging.getLogger("botocore").setLevel(logging.WARNING)
logging.getLogger("aiobotocore").setLevel(logging.WARNING)
logging.getLogger("httpx").setLevel(logging.WARNING)
logging.getLogger("httpcore").setLevel(logging.WARNING)
logging.getLogger("sse_starlette").setLevel(logging.WARNING)
logging.getLogger("stripe").setLevel(logging.WARNING)
logger = logging.getLogger(__name__)

tracer = opentelemetry.trace.get_tracer(__name__)


class WSClosedException(Exception):
    def __str__(self):
        return f"{self.__class__.__name__}: {super().__str__()}"


class GenerationKillException(Exception):
    def __str__(self):
        return f"{self.__class__.__name__}: {super().__str__()}"


class AgentRuntimeException(Exception):
    pass


class BillingException(Exception):
    pass


async def trace_send_callback(cache: Cache, msg: WSMessage):
    if msg.file_rpc:
        import traceback

        traceback.print_stack()
        raise Exception("file rpc request in headless send callback. not good!")

    last = await cache.get("last_send", None)

    if msg.chat or msg == last:
        return False

    await cache.set("last_send", msg)

    GenerationTraceEntity(
        chat_message_id=await cache.get("user_msg_id"),
        state=msg.model_dump(mode="json", by_alias=True),
    ).persist()

    return True


def headless_send_callback_gen(cache):
    return functools.partial(trace_send_callback, cache)


def headless_recv_callback_gen(cache):
    async def trace_recv_callback() -> WSMessage:
        send: WSMessage = await cache.get("last_send")
        if send.type == WSMessageType.RUN_COMMAND:
            return WSMessage(
                type=WSMessageType.RUN_COMMAND_RESPONSE,
                run_command_response=RunCommandResponse(
                    exit_code=-1,
                    output="Command running not supported in this environment. Do not attempt to run any more commands, just return now.",
                ),
            )
        else:
            raise Exception("headless recv callback called")

    return trace_recv_callback


async def get_inference_client_factory(organization, cache):
    providers = {
        "anthropic": create_anthropic_inference_client,
        "google": create_google_gemini_api_client,
        "openrouter": create_openrouter_inference_client,
        "bedrock": create_bedrock_inference_client,
    }

    if os.environ.get("PRIMARY_INFERENCE_PROVIDER"):
        primary_provider, primary_suite = os.environ[
            "PRIMARY_INFERENCE_PROVIDER"
        ].split(":", 1)
        if "FALLBACK_INFERENCE_PROVIDER" in os.environ:
            fallback_provider, fallback_suite = os.environ[
                "FALLBACK_INFERENCE_PROVIDER"
            ].split(":", 1)
        else:
            fallback_provider, fallback_suite = None, None

        return lambda model: TracingInferenceClient(
            providers[primary_provider](model, suite=primary_suite),
            cache,
            fallback_client=(
                providers[fallback_provider](model, suite=fallback_suite)
                if fallback_provider is not None
                else None
            ),
        )
    elif os.environ.get("ANTHROPIC_KEY"):
        return lambda model: TracingInferenceClient(
            create_anthropic_inference_client(model), cache
        )
    elif os.environ.get("GOOGLE_GEMINI_API_KEY"):
        return lambda model: TracingInferenceClient(
            create_google_gemini_api_client(model),
            cache,
        )
    elif os.environ.get("OPENROUTER_KEY"):
        return lambda model: TracingInferenceClient(
            create_openrouter_inference_client(model),
            cache,
        )
    else:
        config = organization.llm_config
        if not config or not config.get("key"):
            raise HTTPException(
                status_code=3003,
                detail="You must configure OpenRouter",
            )
        return lambda model: TracingInferenceClient(
            create_openrouter_inference_client(model, api_key=config["key"]), cache
        )

class BismuthCoreMixin:
    def __init__(self):
        self.jwkset = None
        self.repos = {}
        self.agents: Dict[str, Agent] = {}

    async def initialize(self):
        async with aiohttp.ClientSession() as session:
            async with session.get(
                os.environ.get("KEYCLOAK_URL", "http://localhost:8543/realms/bismuth")
                + "/protocol/openid-connect/certs"
            ) as response:
                self.jwkset = jwt.PyJWKSet.from_dict(await response.json())

    async def setup_request(
        self,
        user: UserEntity,
        feature: FeatureEntity,
        session_id: int,
        request_id: str = None,
        send_message_callback_gen=headless_send_callback_gen,
        recv_message_callback_gen=headless_recv_callback_gen,
    ):
        cache = RedisCache(
            host=os.environ.get("REDIS_HOST", "localhost"), default_prefix=request_id
        )

        inference_client_factory = await get_inference_client_factory(
            feature.project.organization, cache
        )

        send_message_callback = send_message_callback_gen(cache)
        recv_message_callback = recv_message_callback_gen(cache)
        file_rpc = FileRPC(cache, feature, send_message_callback, recv_message_callback)

        # request_id is needed by setup_subgraph so set it here even though it will be blown away in bootstrap_request
        await cache.set("request_id", request_id)
        agent = await setup_interactive_agent(
            cache,
            send_message_callback,
            recv_message_callback,
            inference_client_factory,
            file_rpc,
            session_id=session_id,
        )

        # No setting cache state before this as we clear right at the top of this fn.
        await self.bootstrap_request(cache, user.id, feature.id, session_id, request_id)

        return cache, agent, file_rpc

    async def handle_auth(self, token: str, feature_id: str):
        logger.debug(f"Handling auth for feature_id: {feature_id}")
        if token.startswith("BIS1-"):
            logger.debug("Using API key authentication")
            user = APIKeyEntity.find_by(token=token).user
        else:
            try:
                logger.debug("Using JWT authentication")
                kid = jwt.decode(token, options={"verify_signature": False})["kid"]
                key = self.jwkset[kid]
                decoded_jwt = jwt.decode(token, key=key, algorithms=["RS256"])
                user = UserEntity.find_by(email=decoded_jwt["email"])
            except jwt.PyJWTError as e:
                logger.error(f"JWT Error: {str(e)}")
                raise HTTPException(
                    status_code=3000, detail="Invalid authentication token"
                )

        if user is None:
            logger.warning("User not found")
            raise HTTPException(status_code=3000, detail="Invalid authentication token")

        logger.debug(f"User authenticated: {user.id} ({user.email})")

        keycloak = KeycloakAdmin(
            connection=KeycloakOpenIDConnection(
                server_url=os.environ.get(
                    "KEYCLOAK_URL", "http://localhost:8543/realms/bismuth"
                ).replace("/realms/bismuth", ""),
                username="",
                password="",
                realm_name="bismuth",
                user_realm_name="bismuth",
                client_id=os.environ.get("KEYCLOAK_ADMIN_CLIENT_ID", "api"),
                client_secret_key=os.environ.get(
                    "KEYCLOAK_ADMIN_CLIENT_SECRET", "secret"
                ),
                verify=True,
            )
        )
        kc_user = await keycloak.a_get_user(await keycloak.a_get_user_id(user.email))
        if not kc_user.get("emailVerified"):
            logger.warning(f"Email not verified: {user.email}")
            raise HTTPException(
                status_code=3003,
                detail="You must verify your email before using Bismuth",
            )

        feature = FeatureEntity.get(feature_id)
        if feature is None:
            logger.warning(f"Feature not found: {feature_id}")
            raise HTTPException(status_code=4004, detail="Feature not found")

        feature_org = feature.project.organization
        if not any(feature_org.id == org.id for org in user.organizations):
            logger.warning(f"User {user.id} not in feature org {feature_org.id}")
            raise HTTPException(
                status_code=3003,
                detail="User is not a member of the feature's organization",
            )

        logger.debug(
            f"Authenticated user with token: {token} and feature_id: {feature_id}"
        )

        logger.debug("Authentication and setup complete")
        return user, feature

    async def bootstrap_request(
        self, cache, user_id, feature_id, session_id, request_id
    ):
        await cache.clear()

        await cache.set("user_id", user_id)
        await cache.set("feature_id", feature_id)
        await cache.set("request_id", request_id)
        await cache.set("msg_session_id", session_id)
        await cache.set(
            "search_vars",
            {
                "graph_top": 40,
                "search_top": 150,
                "rerank_top": 60,
                "bm25_weight": 0.3,
                "vector_weight": 0.7,
            },
        )

    @tracer.start_as_current_span(name="process_message")
    async def process_message(
        self,
        cache,
        agent,
        file_rpc: FileRPC,
        message: str,
        kill_q,
        modified_files: list[ChatModifiedFile] = [],
        bill_token_usage: bool = True,
    ):
        request_id = await cache.get("request_id")
        opentelemetry.trace.get_current_span().set_attribute("request_id", request_id)
        tracing_request_id.set(request_id)

        user_id = await cache.get("user_id")
        opentelemetry.trace.get_current_span().set_attribute("user_id", user_id)
        feature_id = await cache.get("feature_id")
        opentelemetry.trace.get_current_span().set_attribute("feature_id", feature_id)
        session_id = await cache.get("msg_session_id")
        opentelemetry.trace.get_current_span().set_attribute(
            "msg_session_id", session_id
        )

        await cache.set("input_message", message)
        await cache.set(
            "modified_files",
            {
                mf.project_path: mf.content if not mf.deleted else None
                for mf in modified_files
            },
        )

        logger.info(
            f"{request_id} is feature '{feature_id}' session '{session_id}' msg '{message}'"
        )

        session = ChatSessionEntity.get(session_id)
        session.updated_at = datetime.now()
        session.update()
        formatted_chat_messages = [
            ChatMessage(
                role=ChatRole.ASSISTANT if msg.is_ai else ChatRole.USER,
                content=mask_context_messages(msg.content),
            )
            for msg in session.chat_messages
        ]

        await cache.set("formatted_chat_messages", formatted_chat_messages)

        user_msg = ChatMessageEntity(
            is_ai=False,
            user_id=user_id,
            content=message,
            session_id=session_id,
            request_id=request_id,
        ).persist()
        await cache.set("user_msg_id", user_msg.id)

        for mf in modified_files:
            await file_rpc.cache(
                mf.project_path, mf.content if not mf.deleted else None
            )

        try:
            toml_contents = await file_rpc.read("bismuth.toml")
            if toml_contents:
                config = BismuthTOML.model_validate(tomllib.loads(toml_contents))
                file_rpc.block_globs = config.chat.block_globs
        except Exception as e:
            print(e)
            pass

        if bill_token_usage and not await billing.has_token_credits(
            session.feature.project.organization
        ):
            raise BillingException(
                "You have run out of credits. Please use the /refill command to purchase more."
            )

        if bill_token_usage:
            await cache.set(
                "remaining_token_credits",
                await billing.token_credits_remaining(
                    session.feature.project.organization
                ),
            )

        task = Task(
            type="chat",
            objective=message,
            params={
                "user_id": user_id,
                "feature_id": feature_id,
            },
        )

        credits_exhausted = False

        agent_t = asyncio.create_task(agent.run_task(task))
        kill_q_t = asyncio.create_task(kill_q.get())

        done, _ = await asyncio.wait(
            [agent_t, kill_q_t], return_when=asyncio.FIRST_COMPLETED
        )
        if done.pop() == agent_t:
            kill_q_t.cancel()

            if task.status in (TaskStatus.FAILED, TaskStatus.PARTIAL):
                err = await cache.get_message(agent.error_mailbox)
                logger.warning(f"Exception in {task}: {err}")
                # N.B. We skip billing below when we screw up internally
                if (
                    WSClosedException.__name__ not in err["error"]
                    and CreditsExhausted.__name__ not in err["error"]
                ):
                    raise AgentRuntimeException(err["error"])
                if CreditsExhausted.__name__ in err["error"]:
                    credits_exhausted = True
        else:
            agent_t.cancel()

        credits = math.ceil(await cache.get("credits_used", 0.0))
        logger.info(f"Message {user_msg.id} used {credits} credits")
        if bill_token_usage:
            await billing.mark_message(user_msg, credits)

        if credits_exhausted:
            raise BillingException(
                "You have run out of credits. Returning any partial work..."
            )


class IngestProgressStatus(Enum):
    IN_PROGRESS = "IN_PROGRESS"
    COMPLETED = "COMPLETED"
    ERROR = "ERROR"


class IngestStep(str, Enum):
    ANALYZE_CODE = "Analyzing code"
    BUILD_GRAPH = "Building code graph"


class IngestProgressEvent(BaseModel):
    step: IngestStep
    status: IngestProgressStatus
    progress: Optional[float] = None  # For steps that have percentage progress


class BismuthAPI(BismuthCoreMixin):
    def __init__(self):
        super().__init__()
        self.router = APIRouter()
        self.router.add_api_route(
            "/api/codegraph", self.api_codegraph, methods=["POST"]
        )
        self.router.add_api_route(
            "/api/codegraph/{feature_id}", self.delete_codegraph, methods=["DELETE"]
        )
        self.router.add_api_route(
            "/healthcheck", lambda: {"status": "ok"}, methods=["GET"]
        )
        self.router.add_websocket_route("/", self.websocket_endpoint)

    async def read_repo(
        self, feature: FeatureEntity
    ) -> tuple[pathlib.Path, dict[str, str]]:
        repo = await clone_repo(feature)
        files = {}
        for file in repo.rglob("*"):
            if not file.is_file():
                continue
            if ".git/" in str(file):
                continue
            if file.suffix not in SourceFile.EXT_MAP:
                continue
            try:
                files[str(file.relative_to(repo))] = file.read_text()
            except UnicodeDecodeError:
                logger.info(f"decode error on {file}")

        return repo, files

    async def _api_codegraph(self, feature_id: int):

        feature = FeatureEntity.get(feature_id)

        repo_path, repo_files = await self.read_repo(feature)

        try:
            graph = GraphRag(
                feature_id=feature_id,
            )

            block_globs = []
            if "bismuth.toml" in repo_files:
                try:
                    config = BismuthTOML.model_validate(
                        tomllib.loads(repo_files["bismuth.toml"])
                    )
                    block_globs = config.chat.block_globs
                except Exception as e:
                    print(e)
                    block_globs = BismuthChatTOML().block_globs
            else:
                block_globs = BismuthChatTOML().block_globs

            nodes, contents, deferred_edges = ast_parse(
                {
                    file_name: content for file_name, content in repo_files.items()
                    if not path_matches(file_name, block_globs) and len(content) < 1_000_000
                }
            )

            with tracer.start_as_current_span("insert nodes"):
                logger.debug(f"inserting {len(nodes)} nodes")
                q = asyncio.Queue()

                async def do_insert(nodes, contents):
                    def progress_cb(progress):
                        q.put_nowait(progress)

                    try:
                        await graph.bulk_insert(
                            nodes, contents, progress_cb=progress_cb
                        )
                    finally:
                        await q.put(None)

                inserter = asyncio.create_task(do_insert(nodes, contents))
                while True:
                    progress = await q.get()
                    if progress is None:
                        break
                    yield IngestProgressEvent(
                        step=IngestStep.BUILD_GRAPH,
                        status=IngestProgressStatus.IN_PROGRESS,
                        progress=progress,
                    )
                await inserter
                if inserter.exception():
                    raise inserter.exception()
                logger.debug(f"inserted nodes")
                yield IngestProgressEvent(
                    step=IngestStep.BUILD_GRAPH,
                    status=IngestProgressStatus.COMPLETED,
                    progress=100.0,
                )

            nodes = {node.symbol: node for node in graph._graph.digraph.nodes()}
            edges = []
            for deferred_edge in deferred_edges:
                edges.append(
                    (
                        nodes[deferred_edge[0]].id,
                        nodes[deferred_edge[1]].id,
                        {
                            "type": deferred_edge[2],
                        },
                    )
                )

            with tracer.start_as_current_span("insert edges"):
                await asyncio.to_thread(graph._graph.add_edges, edges)
                await asyncio.to_thread(
                    graph._graph.add_edges,
                    [(b, a, d | {"reverse": True}) for a, b, d in edges],
                )
                logger.debug(f"inserted {len(edges)} edges")

            with tracer.start_as_current_span("save graph"):
                await asyncio.to_thread(graph.save)

        except Exception as e:
            logger.exception(e)

        finally:
            logger.debug(f"Cleaning up {repo_path}")
            shutil.rmtree(repo_path)

    async def api_codegraph(self, feature_id: int):
        async def transform(feature_id):
            async for event in self._api_codegraph(feature_id):
                yield event.model_dump_json()

        return EventSourceResponse(transform(feature_id))

    async def delete_codegraph(self, feature_id: int):
        """Delete all code graph data for a feature."""
        feature = FeatureEntity.get(feature_id)
        if feature is None:
            raise HTTPException(status_code=404, detail="Feature not found")

        try:
            graph = GraphRag(feature_id=feature_id)
            await graph.delete()
        except Exception as e:
            logger.exception("Failed to delete code graph")

        return {"status": "ok"}

    async def websocket_endpoint(self, websocket: WebSocket):
        logger.debug("WebSocket connection attempt")
        await websocket.accept()
        logger.debug("WebSocket connection accepted")
        try:
            logger.debug("Waiting for authentication message")
            raw_message = WSMessage.model_validate_json(await websocket.receive_text())

            logger.debug(f"Received message: {raw_message}")

            try:
                auth_data = AuthMessage.model_validate(raw_message.auth)
                logger.debug(f"Validated auth message: {auth_data}")
            except Exception as e:
                logger.error(f"Failed to validate auth message: {e}")
                await websocket.close(code=4000)
                return

            no_intermediate = False
            try:
                user, feature = await self.handle_auth(
                    auth_data.token, auth_data.feature_id
                )

                request_id = str(uuid4().int & (1 << 64) - 1)
                logger.debug(
                    f"Request setup complete. User: {user.id}, Feature: {feature.id}"
                )
            except HTTPException as e:
                logger.error(
                    f"HTTP Exception during setup: {e.status_code} - {e.detail}"
                )
                await websocket.send_json({"error": str(e.detail)})
                await websocket.close(code=e.status_code)
                return
            except Exception as e:
                logger.exception(f"Unexpected error during setup")
                await websocket.send_json({"error": "Internal server error"})
                await websocket.close(code=500)
                return

            logger.debug("Setup successful, entering main loop")

            while True:
                data = await websocket.receive_text()
                message = WSMessage.model_validate_json(data)

                def send_message_wrapper(cache):
                    async def send_message(message: WSMessage):
                        user_msg_id = await cache.get("user_msg_id", None)
                        if user_msg_id:
                            GenerationTraceEntity(
                                chat_message_id=user_msg_id,
                                state=message.model_dump(mode="json", by_alias=True),
                            ).persist()
                        # logger.debug(f"WSMessage: {message}")
                        try:
                            await websocket.send_bytes(
                                message.model_dump_json(by_alias=True)
                            )
                            credits = math.ceil(await cache.get("credits_used", 0.0))
                            await websocket.send_bytes(
                                WSMessage(
                                    type=WSMessageType.USAGE,
                                    usage=credits,
                                ).model_dump_json(by_alias=True)
                            )
                        except Exception as e:
                            logger.warning(f"Failed to send message: {e}")
                            if user_msg_id:
                                logger.info(f"Cleaning up chat message {user_msg_id}")
                                ChatMessageEntity.delete(user_msg_id)
                            raise WSClosedException(e)

                    return send_message

                recv_q = asyncio.Queue()
                kill_q = asyncio.Queue()

                async def recv_message_worker():
                    while True:
                        data = await websocket.receive_text()
                        try:
                            message = WSMessage.model_validate_json(data)
                        except ValidationError as e:
                            logger.error(f"Invalid incoming message message: {e}")
                            await websocket.send_json(
                                {
                                    "error": "Invalid message. Perhaps you need to update the CLI?"
                                }
                            )
                            await websocket.close(code=4000)
                            return

                        if message.type == WSMessageType.KILL_GENERATION:
                            content = "Cancelled by user."

                            modified_files = await cache.get(
                                "output_modified_files", {}
                            )

                            if modified_files:
                                content += " Returning current work."

                            ChatMessageEntity(
                                is_ai=True,
                                content=content,
                                session_id=await cache.get("msg_session_id"),
                                request_id=await cache.get("request_id"),
                            ).persist()

                            await send_message_wrapper(cache)(
                                WSMessage(
                                    type=WSMessageType.CHAT,
                                    chat=WebsocketChatMessage(
                                        message=json.dumps(
                                            {
                                                "done": True,
                                                "generated_text": content,
                                                "output_modified_files": [
                                                    ChatModifiedFile(
                                                        name=pathlib.Path(fn).name,
                                                        project_path=fn,
                                                        content=content,
                                                        deleted=(
                                                            content
                                                            == "BISMUTH_DELETED_FILE"
                                                        ),
                                                    ).model_dump(by_alias=True)
                                                    for fn, content in modified_files.items()
                                                ],
                                                "id": await cache.get("user_msg_id"),
                                                "credits_used": math.ceil(
                                                    await cache.get("credits_used", 0.0)
                                                ),
                                            }
                                        )
                                    ),
                                )
                            )
                            await kill_q.put(message)
                            return
                        else:
                            await recv_q.put(message)

                def recv_message_wrapper(cache):
                    async def recv_message():
                        return await recv_q.get()

                    return recv_message

                if message.type == WSMessageType.CHAT:
                    recv_worker = asyncio.create_task(recv_message_worker())
                    cache, agent, file_rpc = await self.setup_request(
                        user,
                        feature,
                        auth_data.session_id,
                        request_id,
                        send_message_wrapper,
                        recv_message_wrapper,
                    )
                    try:
                        await self.process_message(
                            cache,
                            agent,
                            file_rpc,
                            message.chat.message,
                            kill_q,
                            message.chat.modified_files,
                        )
                    except Exception as e:
                        msg = "I'm sorry an error occurred."
                        if isinstance(e, BillingException):
                            msg = str(e)
                        elif isinstance(e, AgentRuntimeException):
                            logger.info(
                                "There was an error and I'm sending the default message."
                            )
                        else:
                            logger.exception(e)

                        ChatMessageEntity(
                            is_ai=True,
                            content=msg,
                            session_id=await cache.get("msg_session_id"),
                            request_id=await cache.get("request_id"),
                        ).persist()

                        await send_message_wrapper(cache)(
                            WSMessage(
                                type=WSMessageType.CHAT,
                                chat=WebsocketChatMessage(
                                    message=json.dumps(
                                        {
                                            "done": True,
                                            "generated_text": msg,
                                            "output_modified_files": [
                                                ChatModifiedFile(
                                                    name=pathlib.Path(fn).name,
                                                    projectPath=fn,
                                                    content=content,
                                                    deleted=content
                                                    == "BISMUTH_DELETED_FILE",
                                                ).model_dump(by_alias=True)
                                                for fn, content in (
                                                    await cache.get(
                                                        "output_modified_files", {}
                                                    )
                                                ).items()
                                            ],
                                            "id": await cache.get("user_msg_id"),
                                            "credits_used": math.ceil(
                                                await cache.get("credits_used", 0.0)
                                            ),
                                        }
                                    )
                                ),
                            )
                        )

                    recv_worker.cancel()
                    await asyncio.wait([recv_worker])
                elif message.type == WSMessageType.SWITCH_MODE:
                    no_intermediate = True
                    session_id = auth_data.session_id
                    chat_session = ChatSessionEntity.get(session_id)
                    assert chat_session is not None
                    session_context = chat_session.get_context()

                    if session_context.get("mode", "single") == "single":
                        session_context["mode"] = "chat"
                    else:
                        session_context["mode"] = "single"

                    chat_session.set_context(session_context)

                    logger.info(f"Switched mode to {session_context['mode']}")

                    await websocket.send_bytes(
                        WSMessage(
                            type=WSMessageType.SWITCH_MODE_RESPONSE, chat=None
                        ).model_dump_json(by_alias=True)
                    )
                elif message.type == WSMessageType.PIN_FILE:
                    session_id = auth_data.session_id
                    chat_session = ChatSessionEntity.get(session_id)
                    assert chat_session is not None
                    session_context = chat_session.get_context()

                    pinned_files: list[str] = session_context.get("pinned_files", [])

                    file_path = message.pin.path

                    if file_path in pinned_files:
                        pinned_files.remove(file_path)
                    else:
                        pinned_files.append(file_path)

                    session_context["pinned_files"] = pinned_files

                    chat_session.set_context(session_context)

                    await websocket.send_bytes(
                        WSMessage(
                            type=WSMessageType.PIN_FILE_RESPONSE, chat=None
                        ).model_dump_json(by_alias=True)
                    )
                else:
                    logger.warning(f"Unknown message type: {message.type}")

        except HTTPException as e:
            logger.warning(f"HTTP Exception during loop: {e.status_code} - {e.detail}")
            await websocket.send_json({"error": str(e.detail)})
        except WebSocketDisconnect:
            logger.info("WebSocket disconnected")
        finally:
            if not no_intermediate:
                chat_session = ChatSessionEntity.get(auth_data.session_id)
                if not chat_session.name and chat_session.chat_messages:
                    inference_client = (
                        await get_inference_client_factory(
                            feature.project.organization, None
                        )
                    )(MODEL)
                    try:
                        name = await inference_client.get_generation(
                            [
                                ChatMessage(
                                    role=ChatRole.SYSTEM,
                                    content="You are an AI that creates summary names for chat sessions in the form of URL slugs - words separated by dashes. The entire chat session will be wrapped in <CHAT> tags, with <USER> and <ASSISTANT> wrapping individual messages between a user and the AI assistant respectively.",
                                ),
                                ChatMessage(
                                    role=ChatRole.USER,
                                    content=textwrap.dedent(
                                        f"""
                                        Here is a chat between a user and an AI assistant:
                                        <CHAT>
                                        {"\n".join(f"<{'ASSISTANT' if msg.is_ai else 'USER'}>{msg.content}</{'ASSISTANT' if msg.is_ai else 'USER'}" for msg in chat_session.chat_messages)}
                                        </CHAT>
                                        Summarize the above chat in the form of a URL slug. The slug should contain no more than 5 words, and the words should be separated by dashes (-).
                                        Respond only with the slug.
                                        """
                                    ),
                                ),
                            ],
                            max_tokens=50,
                            temperature=0.1,
                        )
                        if ChatSessionEntity.list(
                            where="featureid = %s AND name = %s",
                            params=(feature.id, name),
                        ):
                            name += f"-{chat_session.id}"
                        chat_session.name = name
                        chat_session.update()
                    except Exception as e:
                        logger.exception(f"Error making session name")
                elif not chat_session.name and not chat_session.chat_messages:
                    ChatSessionEntity.delete(chat_session.id)


bismuth_api = BismuthAPI()


@asynccontextmanager
async def lifespan(app):
    try:
        hybrid_search.create_tables()
    except Exception:
        pass

    await bismuth_api.initialize()
    yield


app = FastAPI(lifespan=lifespan)
app.include_router(bismuth_api.router)

# / is used for health check
opentelemetry.instrumentation.fastapi.FastAPIInstrumentor().instrument_app(
    app, excluded_urls="/healthcheck"
)

if "OTEL_EXPORTER_OTLP_ENDPOINT" in os.environ:
    import opentelemetry.sdk.resources
    import opentelemetry.sdk.trace.export
    import opentelemetry.exporter.otlp.proto.grpc.trace_exporter

    resource = opentelemetry.sdk.resources.Resource(
        attributes={"service.name": "daneel"}
    )
    provider = opentelemetry.sdk.trace.TracerProvider(resource=resource)
    span_processor = opentelemetry.sdk.trace.export.BatchSpanProcessor(
        opentelemetry.exporter.otlp.proto.grpc.trace_exporter.OTLPSpanExporter()
    )
    provider.add_span_processor(span_processor)
    opentelemetry.trace.set_tracer_provider(provider)

if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="0.0.0.0", port=8765)
