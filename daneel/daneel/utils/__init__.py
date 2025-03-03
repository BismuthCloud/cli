import os
import re
from difflib import SequenceMatcher
from itertools import islice
from typing import Optional

from asimov.services.inference_clients import (
    AnthropicInferenceClient,
    BedrockInferenceClient,
    GoogleGenAIInferenceClient,
    OpenRouterInferenceClient,
)

from daneel.constants import BIG_MODEL, MODEL_CONFIGURATION


def extract_tagged_content(text, tag):
    pattern = rf"<{tag}>\n?(.*)\n?</{tag}>"  # Optional newlines with \n?
    matches = re.findall(pattern, text, re.DOTALL)
    return matches


def iterate_in_batches(iterable, batch_size=4):
    iterator = iter(iterable)
    while True:
        batch = list(islice(iterator, batch_size))
        if not batch:
            break
        yield batch


def create_google_gemini_api_client(
    model: str, suite: str = "default", api_key: Optional[str] = None
):
    return GoogleGenAIInferenceClient(
        MODEL_CONFIGURATION["google"][suite][model],
        api_key or os.environ["GOOGLE_GEMINI_API_KEY"],
    )


def create_openrouter_inference_client(
    model: str, suite: str = "default", api_key: Optional[str] = None
):
    return OpenRouterInferenceClient(
        model=MODEL_CONFIGURATION["openrouter"][suite][model],
        api_key=api_key or os.environ["OPENROUTER_KEY"],
    )


def create_anthropic_inference_client(
    model: str, suite: str = "default", api_key: Optional[str] = None
):
    if suite.startswith("thinking"):
        return AnthropicInferenceClient(
            model=MODEL_CONFIGURATION["anthropic"]["thinking"][model],
            api_key=api_key or os.environ["ANTHROPIC_KEY"],
            thinking=(int(suite[len("thinking:") :]) if model == BIG_MODEL else None),
        )
    return AnthropicInferenceClient(
        model=MODEL_CONFIGURATION["anthropic"][suite][model],
        api_key=api_key or os.environ["ANTHROPIC_KEY"],
    )


def create_bedrock_inference_client(
    model: str,
    suite: str = "default",
):
    region_name = os.environ.get("AWS_REGION", "us-west-2")
    return BedrockInferenceClient(
        model=MODEL_CONFIGURATION["bedrock"][suite][model], region_name=region_name
    )


def filter_dict_keys(dict_list, allowed_keys):
    return [{k: d[k] for k in allowed_keys if k in d} for d in dict_list]


def mask_context_messages(msg: str):
    if "<CONTEXT>" in msg[:12]:
        return "<masked>Message masked and is no longer relevant to the conversation.</masked>"
    else:
        return msg


def strip_file_tags(lines):
    # Remove tags
    if lines and lines[0].startswith("<FILE"):
        lines = lines[1:]
    if lines and lines[-1].strip() == "</FILE>":
        lines = lines[:-1]

    # Remove any empty lines at the end
    while lines and not lines[-1].strip():
        lines = lines[:-1]

    return lines


def normalize_whitespace(text: str) -> str:
    """Normalize whitespace in text while preserving indentation and handling edge cases.

    Args:
        text: The input text to normalize

    Returns:
        Normalized text with consistent whitespace handling
    """
    if not text:
        return ""

    lines = text.splitlines()
    normalized_lines = []

    for line in lines:
        if not line.strip():
            # Preserve empty lines
            normalized_lines.append("")
            continue

        # Convert tabs to spaces (4 spaces per tab)
        line = line.expandtabs(4)

        # Count leading spaces
        leading_spaces = len(line) - len(line.lstrip())

        # Normalize the line content
        content = line.strip()

        # Reconstruct line with preserved indentation
        normalized_line = " " * leading_spaces + content
        normalized_lines.append(normalized_line)

    return "\n".join(normalized_lines)


def find_text_chunk(content, search_lines):
    if isinstance(content, str):
        content = content.splitlines()

    if isinstance(search_lines, str):
        search_lines = search_lines.splitlines()

    search_lines = strip_file_tags(search_lines)

    search_block = "\n".join(search_lines)
    content_block = "\n".join(content)

    matcher = SequenceMatcher(None, content_block, search_block, autojunk=False)
    match = matcher.find_longest_match(0, len(content_block), 0, len(search_block))

    if match.size == len(search_block):
        start_line = content_block.count("\n", 0, match.a) + 1
        end_line = start_line + len(search_lines) - 1

        return {"start": start_line, "end": end_line}

    print("\nNo exact match found, find text chunk.")
    return None
