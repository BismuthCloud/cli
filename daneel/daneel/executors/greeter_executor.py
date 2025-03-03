import textwrap
from asyncio import Semaphore
from typing import Any

from asimov.caches.cache import Cache
from asimov.graph import AgentModule
from asimov.services.inference_clients import ChatMessage, ChatRole, InferenceClient


class GreeterExecutor(AgentModule):
    inference_client: InferenceClient

    def greeter_prompt(self, task, past_messages: list[ChatMessage]):

        if past_messages == []:
            past_messages_text = "This is a new chat with no previous context."

        past_messages_text = ""

        for msg in past_messages:
            tmp = "<EARLIER_MESSAGE>\n"
            tmp += msg.content + "\n"
            tmp += "</EARLIER_MESSAGE>\n"

            past_messages_text += tmp

        return textwrap.dedent(
            f"""
    <CONTEXT>
    {past_messages_text}
    </CONTEXT>

    <MESSAGE>
    {task}
    </MESSAGE>


    You're a staff software engineer level AI and you're working with a user to help them with their problems. They've just sent you a message between MESSAGE tags above and it seems like they want you to make some changes to code.
    Your job in this instance is to simply tell them in your own way that you're happy to help and then state the problem back at them in a way someone might do in normal conversation, somewhat summarized, touching on the key asks and requirements.
    Don't speculate on how you'll fix the problem or ask any questions. These steps will be taken care of by other parts of the system. Assume all context is provided downstream as well such as files, classes etc.

    Later steps can now autonomously gather more context, so if it feels like there's not enough there just remark about running some commands to learn more about the problem.

    Please be concise as well.
    """
        )

    async def send_response_state(self, msg: str):
        response_state_handler = self.config.context.get("response_state_handler")

        if response_state_handler:
            await response_state_handler.send_response_state(msg)

    async def process(self, cache: Cache, semaphore: Semaphore) -> dict[str, Any]:
        # This is in case we have a failure in query rewrite or something it doesn't keep running this node.
        if await cache.get("greeter_ran", False):
            return {"status": "success", "result": ""}

        input_message = await cache.get("input_message")
        past_messages = await cache.get("formatted_chat_messages")

        prompt = self.greeter_prompt(input_message, past_messages)

        messages = [ChatMessage(role=ChatRole.USER, content=prompt)]

        output = ""

        async for token in self.inference_client.connect_and_listen(
            messages, max_tokens=1024
        ):
            output += token

            if self.container is not None:
                await self.container.apply_middlewares(
                    self.config.middlewares,
                    {"status": "success", "token": token},
                    cache,
                )

        await cache.set("generated_text", output.rstrip())
        await cache.set("prefill", output.rstrip())

        await cache.set("greeter_ran", True)

        await self.send_response_state("Planning")
        print("AFTER GREETER")

        return {"status": "success", "result": ""}
