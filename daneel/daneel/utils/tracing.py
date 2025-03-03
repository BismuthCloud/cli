import contextvars
import inspect
import json
import os
import pathlib
import threading
from typing import Optional

import aioboto3  # type: ignore
from pydantic_core import to_jsonable_python

request_id = contextvars.ContextVar("request_id", default="UNKNOWN")
cnt_lock = threading.Lock()
counts: dict[str, int] = {}


async def trace_output(data: str | list | dict | None, name_hint: Optional[str] = None):
    out_dir = os.environ.get("DANEEL_TRACE", "")
    if not out_dir:
        return

    req_id = request_id.get()

    if name_hint:
        name = name_hint
    else:
        caller = inspect.stack()[1]
        name = f"{caller.filename}:{caller.function}:{caller.lineno}"

    with cnt_lock:
        count = counts.get(req_id, 0)
        counts[req_id] = count + 1

    serialized = ""
    if isinstance(data, str):
        serialized = data
    elif isinstance(data, (dict, list)):
        serialized = json.dumps(data, indent=4, default=to_jsonable_python)
    elif data is None:
        serialized = "(none)"
    else:
        raise ValueError(f"Unsupported tracing data type: {type(data)}")

    if out_dir == "s3":
        session = aioboto3.Session()
        async with session.client("s3") as s3:
            await s3.put_object(
                Bucket="bismuth-traces",
                Key=f"{req_id}/{str(count).zfill(3)}_{name}.txt",
                Body=serialized.encode("utf-8"),
            )
    else:
        out_dir_p = pathlib.Path(out_dir) / req_id
        out_dir_p.mkdir(parents=True, exist_ok=True)
        fn = out_dir_p / f"{str(count).zfill(3)}_{name}.txt"

        with open(fn, "w") as f:
            f.write(serialized)
