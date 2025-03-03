import json
import pytest
import pytest_asyncio

from asimov.caches.mock_redis_cache import MockRedisCache

from daneel.agents.common import StreamingMiddleware
from daneel.utils.websockets import WSMessage, WSMessageType


@pytest_asyncio.fixture
async def cache():
    cache = MockRedisCache()
    return cache


@pytest.mark.asyncio
async def test_streaming_filtering(cache):
    msgs: list[WSMessage] = []

    async def send_message_cb(wsmessage: WSMessage):
        msgs.append(wsmessage)

    middleware = StreamingMiddleware(send_message_callback=send_message_cb)
    await middleware.process({"token": "Chat "}, cache)
    await middleware.process({"token": "chat chat\n<CURRENT"}, cache)
    await middleware.process({"token": "_LOCATOR>"}, cache)
    await middleware.process({"token": "sample locator"}, cache)
    await middleware.process({"token": "sample locator 2"}, cache)
    await middleware.process({"token": "</CURRENT_LOCATOR>\nafter"}, cache)
    await middleware.process({"token": " but should still be blocked\n"}, cache)
    await middleware.process({"token": "<BCODE>\n```python\n"}, cache)

    assert len(msgs) == 3
    assert json.loads(msgs[0].chat.message)["token"]["text"] == "Chat "
    assert json.loads(msgs[1].chat.message)["token"]["text"] == "chat chat\n"
    assert msgs[2].type == WSMessageType.RESPONSE_STATE

    await middleware.process(
        {
            "partial_message": "Chat chat chat\n<BCODE>\n```python\nx = 1\n```\n</BCODE>\n"
        },
        cache,
    )

    assert len(msgs) == 4
    assert (
        json.loads(msgs[3].chat.message)["partial_message"]
        == "Chat chat chat\n<BCODE>\n```python\nx = 1\n```\n</BCODE>\n"
    )

    await middleware.process({"token": "Some more chat"}, cache)

    assert len(msgs) == 5
    assert json.loads(msgs[4].chat.message)["token"]["text"] == "Some more chat"
