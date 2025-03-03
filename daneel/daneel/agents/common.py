import json
import logging
import math
import pathlib
import re
from asyncio import Semaphore
from typing import Any, Awaitable, Callable, Dict, List, Optional

from asimov.asimov_base import AsimovBase
from asimov.caches.cache import Cache
from asimov.graph import Middleware, Node
from pydantic import Field, PrivateAttr

from daneel.data.postgres.models import ChatMessageEntity
from daneel.utils.websockets import (
    ChatMessage,
    ChatModifiedFile,
    ResponseState,
    ResponseStateEnum,
    WSMessage,
    WSMessageType,
    null_send_callback,
)


class CacheVariable(AsimovBase):
    name: str
    suffix: Optional[str] = None


class CacheResetNode(Node):
    reset_variables: List[CacheVariable] = Field(default_factory=list)

    async def run(self, cache: Cache, semaphore: Semaphore):
        groups: dict[str | None, list[CacheVariable]] = {}
        for var in self.reset_variables:
            if var.suffix in groups:
                groups[var.suffix].append(var)
            else:
                groups[var.suffix] = [var]

        for suffix, vars in groups.items():
            if suffix:
                async with cache.with_suffix(suffix):
                    for var in vars:
                        await cache.delete(var.name)
            else:
                for var in vars:
                    await cache.delete(var.name)

        return {"status": "success", "result": "success"}


class SyncFailureNode(Node):
    failure_message: str = (
        "I'm sorry, but there appears to be an issue with my external LLM provider right now. Please try again in a few minutes."
    )
    send_message_callback: Callable[[WSMessage], Awaitable[None]]
    _logger: logging.Logger = PrivateAttr()

    async def run(self, cache: Cache, semaphore: Semaphore) -> Dict[str, Any]:
        self._logger = logging.getLogger(__name__).getChild(
            await cache.get("request_id")
        )
        msg = self.failure_message

        if await cache.get("output_modified_files", {}):
            msg = await cache.get("generated_text", "")
            msg += "...\nSorry, I've run into an issue trying to complete the task. Here is what I have so far."

        me = ChatMessageEntity(
            is_ai=True,
            content=msg,
            session_id=await cache.get("msg_session_id"),
            request_id=await cache.get("request_id"),
        ).persist()

        msg = re.sub(r"\n<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n", "", msg)

        await self.send_message_callback(
            WSMessage(
                type=WSMessageType.CHAT,
                chat=ChatMessage(
                    message=json.dumps(
                        {
                            "done": True,
                            "generated_text": msg,
                            "commit_message": await cache.get("commit_message", None),
                            "output_modified_files": [
                                ChatModifiedFile(
                                    name=pathlib.Path(fn).name,
                                    projectPath=fn,
                                    content=content,
                                    deleted=content == "BISMUTH_DELETED_FILE",
                                ).model_dump(by_alias=True)
                                for fn, content in (
                                    await cache.get("output_modified_files", {})
                                ).items()
                            ],
                            "id": me.id,
                            "credits_used": math.ceil(
                                await cache.get("credits_used", 0.0)
                            ),
                        }
                    )
                ),
            )
        )
        self._logger.warning(f"Emitted failure message")

        return {"status": "success", "result": self.failure_message}


class SubGraphEntryNode(Node):
    async def run(self, cache: Cache, semaphore: Semaphore):
        return {"status": "success"}


class StreamingMiddleware(Middleware):
    send_message_callback: Callable[[WSMessage], Awaitable] = Field(
        default=null_send_callback,
    )
    parallelism: int = 0
    # Buffer from the beginning of each line until we see a space (or another newline)
    # This lets us filter out internal stuff which we don't want to send to the client
    # e.g. <CURRENT_LOCATOR>, <BCODE>, etc.
    filtered_prefixes: tuple[str, ...] = ("<CURRENT_LOCATOR>", "<BCODE>", "</BCODE>")

    async def process(self, data: Dict[str, Any], cache: Cache) -> Dict[str, Any]:
        if "token" in data:
            line_buffer = await cache.get("line_buffer", "")
            line_buffer += data["token"]

            # AFAIK there's no guarantees about what's in a token, so a single "token"
            # might actually be something like "\nExample:\n<CURRENT_LOCATOR>..."
            # so we need to process each line separately
            # N.B. split instead of splitlines so we get an empty string at the end if the last line ends with a newline
            lines = line_buffer.split("\n")
            for i, line in enumerate(lines[:-1]):
                if line.startswith(self.filtered_prefixes):
                    continue
                # If there is a partial line buffered but with a space such that it would have already been sent,
                # strip off that beginning part so we don't double-send.
                if i == 0:
                    tok_line_len = len(data["token"].split("\n")[0])
                    if tok_line_len:
                        partial_line = line[:-tok_line_len]
                    else:
                        partial_line = line
                    if " " in partial_line:
                        line = line[len(partial_line) :]
                await self.send_message_callback(
                    WSMessage(
                        type=WSMessageType.CHAT,
                        chat=ChatMessage(
                            message=json.dumps(
                                {
                                    "token": {"text": line + "\n"},
                                    "clear_past_output": False,
                                    "id_at_analysis_open": None,
                                }
                            )
                        ),
                    ),
                )
            else:
                last_line = lines[-1]
                await cache.set("line_buffer", last_line)
                # 3 possible states:
                if " " not in last_line:
                    # Line has no space yet, so we need to buffer
                    pass
                elif (
                    " " in data["token"].split("\n")[-1]
                    and " " not in last_line[: -len(data["token"].split("\n")[-1])]
                ):
                    # We just added the first space to the line.
                    # Send through if no filtered prefixes
                    if not last_line.startswith(self.filtered_prefixes):
                        await self.send_message_callback(
                            WSMessage(
                                type=WSMessageType.CHAT,
                                chat=ChatMessage(
                                    message=json.dumps(
                                        {
                                            "token": {"text": last_line},
                                            "clear_past_output": False,
                                            "id_at_analysis_open": None,
                                        }
                                    )
                                ),
                            ),
                        )
                else:
                    # Line already has been checked, send tokens through as normal
                    await self.send_message_callback(
                        WSMessage(
                            type=WSMessageType.CHAT,
                            chat=ChatMessage(
                                message=json.dumps(
                                    {
                                        "token": {"text": data["token"]},
                                        "clear_past_output": False,
                                        "id_at_analysis_open": None,
                                    }
                                )
                            ),
                        ),
                    )
        elif "partial_message" in data:
            await cache.set("line_buffer", "")

            msg = data["partial_message"]
            msg = re.sub(r"<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n", "", msg)

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.CHAT,
                    chat=ChatMessage(
                        message=json.dumps(
                            {
                                "partial_message": msg,
                            }
                        )
                    ),
                ),
            )
        elif data.get("code_encountered", False):
            await cache.set("line_buffer", "")

            msg = await cache.get("generated_text", "")
            msg = re.sub(r"<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n", "", msg)

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.CHAT,
                    chat=ChatMessage(
                        message=json.dumps(
                            {
                                "partial_message": msg,
                            }
                        )
                    ),
                ),
            )
            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.RESPONSE_STATE,
                    responseState=ResponseState(
                        state="Writing code",
                        attempt=0,
                    ),
                ),
            )
        elif "finished" in data:
            await cache.set("line_buffer", "")

            text = data["generated_text"]
            text = re.sub(r"<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n", "", text)

            commit_msg = await cache.get("commit_message", None)

            print(
                "finalized modified files", await cache.get("output_modified_files", {})
            )

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.CHAT,
                    chat=ChatMessage(
                        message=json.dumps(
                            {
                                "done": True,
                                "generated_text": text,
                                "commit_message": commit_msg,
                                "output_modified_files": [
                                    ChatModifiedFile(
                                        name=pathlib.Path(fn).name,
                                        projectPath=fn,
                                        content=content,
                                        deleted=content == "BISMUTH_DELETED_FILE",
                                    ).model_dump(by_alias=True)
                                    for fn, content in (
                                        await cache.get("output_modified_files", {})
                                    ).items()
                                ],
                                "id": data["msg_id"],
                                "credits_used": math.ceil(
                                    await cache.get("credits_used", 0.0)
                                ),
                            }
                        )
                    ),
                ),
            )
        elif "error" in data:
            await cache.set("line_buffer", "")

            await self.send_message_callback(
                WSMessage(
                    type=WSMessageType.CHAT,
                    chat=ChatMessage(
                        message=json.dumps(
                            # TODO: i don't think cli handles this
                            {
                                "error": True,
                                "message": data.get(
                                    "error", "An unknown error occurred"
                                ),
                            }
                        )
                    ),
                ),
            )

        return data


class ResponseStateHandler(AsimovBase):
    send_message_callback: Callable[[WSMessage], Awaitable[None]] = Field(
        default=null_send_callback,
    )
    cache: Cache

    async def send_response_state(self, msg, attempt=0):
        await self.cache.set("last_response_state", ResponseStateEnum.DYNAMIC)

        await self.send_message_callback(
            WSMessage(
                type=WSMessageType.RESPONSE_STATE,
                response_state=ResponseState(
                    state=msg,
                    attempt=attempt,
                ),
            ),
        )
