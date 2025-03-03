import textwrap
from asyncio import Semaphore
from typing import Any

from asimov.caches.cache import Cache
from asimov.graph import AgentModule
from asimov.services.inference_clients import ChatMessage, ChatRole, InferenceClient
from jinja2 import Template


class SummaryExecutor(AgentModule):
    inference_client: InferenceClient

    def recursive_execution_summary_prompt(self, changes, task):
        return Template(
            textwrap.dedent(
                """
You are an AI assistant specializing in code analysis and change documentation. Your task is to create a detailed changelog based on a set of file changes. This changelog will be used by another AI model (Claude) as part of a larger task, so it's crucial that your output is clear, structured, and easily parsable.

Here are the file changes you need to analyze:

<file_changes>
{{changes}}
</file_changes>

And here is the description of the task being performed:

<task_description>
{{task}}
</task_description>

Please follow these steps to complete the task:

1. Analyze the changes:
   Wrap your analysis in <change_analysis> tags, breaking down your analysis for each file:
   a. Extract the filename from the 'fn' attribute.
   b. Compare the content within the <before> and <after> tags.
   c. Identify and quote specific changes made (lines added, removed, or modified).
   d. Categorize changes as additions, deletions, or modifications.
   e. Assess the potential impact of each change on functionality or structure.
   f. Summarize the overall impact of changes for this file.
   g. Explicitly link the changes to the task description, noting how they contribute to the task's completion.

2. Create the changelog:
   Based on your analysis, compile a detailed changelog. Include the following sections:
   
   <changelog>
     <files_changed>
       List each modified file here
     </files_changed>
     
     <change_details>
       For each file:
       <file>
         <filename>The name of the file</filename>
         <changes>
           <addition>Any added lines or features</addition>
           <deletion>Any removed lines or features</deletion>
           <modification>Any modified lines or features</modification>
         </changes>
         <impact>Assessment of the change's impact on functionality or structure</impact>
         <task_relevance>How the changes contribute to the described task</task_relevance>
       </file>
     </change_details>
     
     <subtask_completion>
       Explicitly state whether the subtask described in the task_description was completed based on the changes made
     </subtask_completion>
     
     <next_steps>
       Specify whether the main task should resume or if additional work is needed
     </next_steps>
   </changelog>

3. Review and finalize:
   Before submitting your response, review your changelog to ensure it's comprehensive, accurate, and structured as specified.

Remember, your output should be detailed enough to serve as a standalone record of changes, but also structured in a way that another AI model can easily process and understand.
            """
            )
        ).render(changes=changes, task=task)

    def summary_prompt(self, task, changes: str, commands: str):
        return Template(
            textwrap.dedent(
                """
Your task is to review a set of file changes and provide a concise, well-formatted summary of these changes.

Here are the file changes you need to analyze, they are pre summarized because content length became an issue:

<file_changes>
{{changes}}
</file_changes>

And here are commands run as part of completing the task:
<commands>
{{commands}}
</commands>

Now, let's look at the task description provided by the user:

<task_description>
{{task}}
</task_description>

Please follow these steps to complete the task:

1. Analyze the file changes:
   Wrap your analysis inside <change_analysis> tags. For each file:
   a. Extract the filename from the 'fn' attribute.
   b. Compare the content within the <before> and <after> tags.
   c. Identify the specific changes made (e.g., lines added, removed, or modified).
   d. Quote specific changes for each file.
   e. Categorize changes as additions, deletions, or modifications.
   f. Consider the potential impact of each change on functionality or structure.
   g. Note any significant alterations in functionality or structure.

2. Analyze the commands run:
   a. Identify the purpose of each command, and whether it was successful.
   b. Note how the command contributes to the overall task.

3. Summarize the changes:
   Based on your analysis, create a concise summary of what was done. Include:
   - An overview of which files were modified.
   - A brief description of the most important changes.
   - Any patterns or themes you notice across multiple files.
   - How commands were used to complete the task.

4. Format your summary:
   Use appropriate markdown formatting to make your summary clear and readable. This may include:
   - Headings for different sections or files
   - Code fences for code snippets
   - Bullet points for lists of changes
   - Bold or italic text for emphasis

5. Review for conciseness:
   Before finalizing your summary, review it to ensure it's as concise as possible while still conveying all necessary information.

Here's an example of how your output might be structured (note that this is a generic example and your actual output should be based on the specific changes you analyze):

Summary of Changes

🗂️ Files modified:
- `file1.js`
- `file2.css`
- `file3.html`

🔑 Key Changes:
- **File1.js**: Updated API endpoint URL and added error handling
- **File2.css**: Adjusted responsive layout breakpoints
- **File3.html**: Added new form fields for user registration

Notable pattern: Improvements in error handling and user input validation across multiple files.

Detailed Changes:

📋 file1.js
```javascript
// Code snippet showing significant change
```
- Description of change and its impact

[Additional files and changes as needed]

Remember to be concise while providing a comprehensive overview of the changes. Your summary should help the user quickly understand what has been modified without needing to review each file individually.

Please be succint you only have 256 tokens to express yourself.
    """
            )
        ).render(task=task, changes=changes, commands=commands)

    async def process(
        self, cache: Cache, semaphore: Semaphore, **kwargs
    ) -> dict[str, Any]:
        input_message = await cache.get("input_message")
        input_message = kwargs.get("input_message", input_message)

        changes = await cache.get("change_log", [])

        if isinstance(changes, list):
            changes = "\n".join(changes)
        changes = kwargs.get("change_log", changes)

        command_history = await cache.get("command_history", [])
        commands = "\n".join(
            f"Command: `{command}`, return code: {code}"
            for command, code in command_history
        )

        if kwargs.get("input_message"):
            prompt = self.recursive_execution_summary_prompt(changes, input_message)
        else:
            prompt = self.summary_prompt(input_message, changes, commands)

        messages = [
            ChatMessage(
                role=ChatRole.SYSTEM,
                content="You are an AI assistant specialized in analyzing and summarizing code changes.",
            ),
            ChatMessage(role=ChatRole.USER, content=prompt),
        ]

        output = ""

        async for token in self.inference_client.connect_and_listen(
            messages, max_tokens=256
        ):
            output += token

            if not kwargs.get("input_message") and self.container is not None:
                await self.container.apply_middlewares(
                    self.config.middlewares,
                    {"status": "success", "token": token},
                    cache,
                )

        await cache.set("generated_text", output.rstrip())

        return {"status": "success", "result": output}
