import json
import textwrap
from asyncio import Semaphore
from typing import AsyncGenerator

from asimov.caches.cache import Cache
from asimov.graph import AgentModule
from asimov.services.inference_clients import ChatMessage, ChatRole, InferenceClient

from daneel.utils import mask_context_messages


class ChatExecutor(AgentModule):
    basic: bool = False
    inference_client: InferenceClient

    async def process(self, cache: Cache, semaphore: Semaphore) -> AsyncGenerator:
        system_prompt = await cache.get("system_prompt")

        input_message = await cache.get("input_message")
        formatted_chat_messages = await cache.get("formatted_chat_messages", [])
        locators = await cache.get("locators", [])
        messages = await cache.get("chat_message_log", formatted_chat_messages[-5:])

        locator_context = "<LOCATORS>\n"
        if locators:
            for locator in locators:
                locator_context += f"{json.dumps(locator)}\n"
        locator_context += "\n</LOCATORS>\n"

        print(locator_context)

        if not messages:
            messages = [
                *formatted_chat_messages,
                # ChatMessage(
                #     role=ChatRole.USER,
                #     content=f"<CONTEXT>\n{repo_context}\n</CONTEXT>\n",
                # ),
                # ChatMessage(
                #     role=ChatRole.ASSISTANT,
                #     content=textwrap.dedent(
                #         """
                #         Thank you for taking the time to provide the detailed context. This will help ensure I can give you the most relevant and effective coding assistance. I'm ready to help!
                #         """
                #     ),
                # ),
                ChatMessage(
                    role=ChatRole.USER,
                    content=f"{locator_context}\n{system_prompt}\nWRAP YOUR MARKDOWN CODE BLOCK IN \n<BCODE>\n</BCODE>\n USER MESSAGE: {input_message}",
                ),
            ]
            await cache.set("chat_message_log", messages)
        elif await cache.get("in_error_correction", False):
            await cache.set("in_error_correction", False)
            await cache.set("chat_finished", False)  # Necessary?
            await cache.set("prefill", "")
            feedback_context = await cache.get("failed_tests_decision_context", "")

            for message in messages:
                message.content = mask_context_messages(message.content)

            messages.append(
                # ChatMessage(
                #     role=ChatRole.USER,
                #     content=textwrap.dedent(
                #         f"""
                #         <CONTEXT>
                #         {repo_context}
                #         </CONTEXT>
                #         """
                #     ),
                # ),
                # ChatMessage(
                #     role=ChatRole.ASSISTANT,
                #     content=textwrap.dedent(
                #         """
                #         Thank you for taking the time to provide the detailed context. This will help ensure I can give you the most relevant and effective coding assistance. I'm ready to help!
                #         """
                #     ),
                # ),
                ChatMessage(
                    role=ChatRole.USER,
                    content=textwrap.dedent(
                        f"""
                            <FEEDBACK>
                            {feedback_context}
                            </FEEDBACK>

                            {locator_context}

                            {system_prompt}
                            """
                    ),
                ),
            )
        else:
            messages.append(
                ChatMessage(
                    role=ChatRole.USER,
                    content=f"{locator_context}\n{system_prompt}\nWRAP YOUR MARKDOWN CODE BLOCK IN \n<BCODE>\n</BCODE>\n USER MESSAGE: {input_message}",
                ),
            )

        prefill = await cache.get("prefill", "")

        if prefill:
            messages.append(
                ChatMessage(
                    role=ChatRole.ASSISTANT,
                    content=prefill,
                )
            )

        async for token in self.inference_client.connect_and_listen(
            messages, max_tokens=8192, temperature=0.0
        ):
            prefill += token

            await cache.set("prefill", prefill)
            await cache.set("current_token", token)

            yield {"status": "success", "token": token}

        await cache.set("chat_finished", True)
