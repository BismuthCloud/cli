from typing import Optional
from unittest.mock import AsyncMock

import pytest
import pytest_asyncio
from asimov.caches.cache import Cache
from asimov.caches.mock_redis_cache import MockRedisCache
from asimov.services.inference_clients import (
    ChatMessage,
    ChatRole,
    InferenceClient,
    NonRetryableException,
)

from daneel.data.file_rpc import FileRPC
from daneel.data.postgres.models import ChatSessionEntity
from daneel.executors.aci import ACI
from daneel.executors.aci.aci_types import EditAction


@pytest_asyncio.fixture
async def cache():
    """Base cache fixture with minimal setup"""
    cache = MockRedisCache()
    await cache.set("input_message", "test message")
    await cache.set("request_id", "test_request_id")
    await cache.set("feature_id", 123)
    await cache.set("msg_session_id", 234)
    return cache


class MockInferenceClient(InferenceClient):
    """Mock implementation of InferenceClient for testing"""

    model: str = "mock"

    def __init__(self, return_value: str = "") -> None:
        """Initialize with optional return value for all methods"""
        self.return_value = return_value
        super().__init__()

    async def connect_and_listen(
        self,
        messages: list[ChatMessage],
        max_tokens: int = 8192,
        top_p: float = 0.9,
        temperature: float = 0.5,
    ) -> str:
        """Mock implementation that returns the configured value"""
        return self.return_value

    async def get_generation(
        self, messages: list[ChatMessage], max_tokens=4096, top_p=0.9, temperature=0.5
    ) -> str:
        """Mock implementation that returns the configured value"""
        return self.return_value

    async def _tool_chain_stream(
        self, messages: list[ChatMessage], max_tokens=4096, top_p=0.9, temperature=0.5
    ) -> str:
        """Mock implementation that returns the configured value"""
        return self.return_value


class MockToolChainInferenceClient(InferenceClient):
    model: str = "tool_chain"

    def __init__(self, calls):
        self.calls = calls
        super().__init__()

    async def get_generation(
        self, messages, max_tokens=4096, top_p=0.9, temperature=0.5
    ):
        raise NotImplementedError()

    def connect_and_listen(self, messages, max_tokens=4096, top_p=0.9, temperature=0.5):
        raise NotImplementedError()

    async def _tool_chain_stream(
        self,
        serialized_messages,
        tools,
        system: Optional[str] = None,
        max_tokens=1024,
        top_p=0.9,
        temperature=0.5,
        tool_choice="any",
        middlewares=[],
    ):
        if not self.calls:
            raise NonRetryableException("No more calls to make")

        c = self.calls.pop(0)
        return [
            {
                "type": "tool_use",
                "id": "tool_use_id",
                "name": c["name"],
                "input": c["input"],
            }
        ]


class MockFileRPC(FileRPC):
    """Mock implementation of FileRPC for testing"""

    _cache: Cache
    files: dict[str, str]
    repo = None

    def __init__(self, cache: Cache, files: dict[str, str]):
        self._cache = cache
        self.files = files

    def __del__(self):
        pass

    async def read(self, path: str, overlay_modified: bool = False) -> Optional[str]:
        if overlay_modified:
            modified = await self._cache.get("output_modified_files", {})
            if path in modified:
                return (
                    modified[path] if modified[path] != "BISMUTH_DELETED_FILE" else None
                )
        return self.files.get(path)


async def make_aci(cache: Cache, inference_client: InferenceClient) -> ACI:
    async def recv_message():
        raise NotImplementedError()

    inference_client_factory = AsyncMock()
    inference_client_factory.return_value = inference_client

    # Add test data to cache
    test_file_content = "\n".join(f"line {i}" for i in range(200))

    file_rpc = MockFileRPC(
        cache, {"test.py": test_file_content, "other.py": "other file content"}
    )

    await cache.set("locators", [])
    await cache.set(
        "planned_task_config",
        {
            "command_execution_enabled": False,
            "build_lint_test_execution_enabled": False,
            "human_assistance_enabled": False,
            "run_lint_test_build_once": False,
        },
    )

    aci: ACI = await ACI.create(
        cache,
        inference_client,
        inference_client_factory=inference_client_factory,
        file_rpc=file_rpc,
        send_message_callback=AsyncMock(),
        recv_message_callback=recv_message,
        interactive_mode=True,
    )
    await aci.set_starting_context({"test.py": ["line 100"], "other.py": []})

    return aci


@pytest.mark.asyncio
async def test_manipulate_edit_action(cache):
    """Test EditAction functionality across different scenarios"""
    aci = await make_aci(cache, MockInferenceClient())

    # Set up initial state with both files open
    await cache.set("viewer_open_files", ["test.py", "other.py"])
    await cache.set("active_file", "test.py")

    # Test case 1: Edit in middle of Python file
    edit_action = EditAction(
        file="test.py",
        lines_to_replace="line 100",
        replace_text="modified line 100",
    )
    result = await aci.manipulate(edit_action, "test.py")

    # Verify cache updates
    output_modified_files = await cache.get("output_modified_files", {})
    assert "test.py" in output_modified_files
    assert "modified line 100" in output_modified_files["test.py"]

    # Test case 2: Edit at start of file
    edit_action = EditAction(
        file="test.py",
        lines_to_replace="line 0",
        replace_text="new first line",
    )
    result = await aci.manipulate(edit_action, "test.py")

    # Verify edit at start
    assert "new first line" in (await cache.get("output_modified_files", {}))["test.py"]

    # Test case 3: Edit in different file type (other.py)
    edit_action = EditAction(
        file="other.py",
        lines_to_replace="other file content",
        replace_text="updated content",
    )
    result = await aci.manipulate(edit_action, "other.py")

    # Verify edit in other file
    assert (
        "updated content" in (await cache.get("output_modified_files", {}))["other.py"]
    )

    # Test case 4: Edge case - empty replacement
    edit_action = EditAction(
        file="test.py",
        lines_to_replace="line 150",
        replace_text="",
    )
    result = await aci.manipulate(edit_action, "test.py")

    # Verify empty replacement
    assert "line 150" not in (await cache.get("output_modified_files", {}))["test.py"]

    # Test case 5: Multiple line replacement
    edit_action = EditAction(
        file="test.py",
        lines_to_replace="line 180\nline 181",
        replace_text="new line 180\nnew line 181",
        step="Test multi-line edit",
        id="test_005",
    )
    result = await aci.manipulate(edit_action, "test.py")

    # Verify multi-line edit
    contents = (await cache.get("output_modified_files", {}))["test.py"]
    assert "new line 180" in contents
    assert "new line 181" in contents


@pytest.mark.asyncio
async def test_tool_chain(cache, mocker):
    mocker.patch.object(ChatSessionEntity, "get", return_value=ChatSessionEntity())
    mocker.patch.object(ChatSessionEntity, "update")

    client = MockToolChainInferenceClient(
        [
            {"name": "switch_to_editing_mode", "input": {"goal": "edit test.py"}},
            {
                "name": "edit_file",
                "input": {
                    "thoughts": "empty",
                    "file_id": 0,
                    "step": "making a change",
                    "file": "test.py",
                    "lines_to_replace": "line 100",
                    "replace_text": "modified line 100",
                    "id": "test_001",
                },
            },
            {"name": "switch_to_driver_mode", "input": {"results": "did the thing"}},
            {"name": "finalize", "input": {}},
        ]
    )

    aci = await make_aci(cache, client)
    aci.set_input_task("input task")

    prompt, tools, _ = await aci.prompt_and_toolset_for_current_mode()
    out = await client.tool_chain(
        [
            ChatMessage(role=ChatRole.USER, content="input task"),
        ],
        tools=tools,
        mode_swap_callback=aci.prompt_and_toolset_for_current_mode,
    )

    contents = (await cache.get("output_modified_files", {}))["test.py"]
    assert "modified line 100" in contents
