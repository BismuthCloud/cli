# Light wrapper around websocket ops for file accesses
import shutil
from pathlib import Path
from typing import Awaitable, Callable, List

from asimov.caches.cache import Cache
from git import Optional

from daneel.data.postgres.models import FeatureEntity
from daneel.utils.glob_match import path_matches
from daneel.utils.repo import clone_repo
from daneel.utils.websockets import (
    FileRPCListRequest,
    FileRPCListResponse,
    FileRPCReadRequest,
    FileRPCReadResponse,
    FileRPCSearchRequest,
    FileRPCSearchResponse,
    WSMessage,
    WSMessageType,
)


class FileRPC:
    _cache: Cache
    feature: FeatureEntity
    repo: Optional[Path]
    send: Callable[[WSMessage], Awaitable[None]]
    recv: Callable[[], Awaitable[WSMessage]]
    block_globs: list[str] = []
    use_pushed_only: bool = False

    def __init__(
        self,
        cache: Cache,
        feature: FeatureEntity,
        send: Callable[[WSMessage], Awaitable[None]],
        recv: Callable[[], Awaitable[WSMessage]],
        block_globs: list[str] = [],
        use_pushed_only: bool = False,
    ):
        self._cache = cache
        self.feature = feature
        self.repo = None
        self.send = send
        self.recv = recv
        self.block_globs = block_globs
        self.use_pushed_only = use_pushed_only
        if self.use_pushed_only and not self.feature.project.has_pushed:
            raise ValueError("use_pushed_only is set but no repo is available")

    def __del__(self):
        if self.repo:
            shutil.rmtree(self.repo)

    def _is_blocked(self, path: str) -> bool:
        return path_matches(path, self.block_globs)

    async def _ensure_cloned(self):
        if self.feature.project.has_pushed and not self.repo:
            self.repo = await clone_repo(self.feature)

    async def _list(self) -> list[str]:
        c = await self._cache.get("file_list_cache", None)
        if c:
            return c

        if self.use_pushed_only:
            assert self.repo is not None
            files = [
                str(f.relative_to(self.repo))
                for f in self.repo.glob("**/*")
                if f.is_file()
            ]
        else:
            await self.send(
                WSMessage(
                    type=WSMessageType.FILE_RPC,
                    file_rpc=FileRPCListRequest(),
                )
            )
            res = await self.recv()
            assert isinstance(res.file_rpc_response, FileRPCListResponse)
            files = res.file_rpc_response.files

        files = [f for f in files if not self._is_blocked(f)]
        await self._cache.set("file_list_cache", files)
        return files

    async def list(self, overlay_modified: bool = False) -> list[str]:
        await self._ensure_cloned()
        out = await self._list()
        if overlay_modified:
            modified = await self._cache.get("output_modified_files", {})

            out = list(set(out) | set(modified.keys()))

            return list(
                filter(lambda fn: modified.get(fn) != "BISMUTH_DELETED_FILE", out)
            )
        return out

    async def read(self, path: str, overlay_modified: bool = False) -> Optional[str]:
        await self._ensure_cloned()

        if self._is_blocked(path):
            return None

        if overlay_modified:
            modified = await self._cache.get("output_modified_files", {})
            if path in modified:
                return (
                    modified[path] if modified[path] != "BISMUTH_DELETED_FILE" else None
                )

        cache_miss = object()
        fcache = await self._cache.get(f"file_cache_{path}", cache_miss)
        if fcache is not cache_miss:
            return fcache

        if self.use_pushed_only:
            assert self.repo is not None
            try:
                content = (self.repo / path).read_text()
            except (FileNotFoundError, UnicodeDecodeError):
                content = None
        else:
            await self.send(
                WSMessage(
                    type=WSMessageType.FILE_RPC,
                    file_rpc=FileRPCReadRequest(path=path),
                )
            )
            res = await self.recv()
            assert isinstance(res.file_rpc_response, FileRPCReadResponse)
            content = res.file_rpc_response.content

        await self._cache.set(f"file_cache_{path}", content)
        return content

    async def search(
        self, query: str, overlay_modified: bool = False
    ) -> List[tuple[str, int, str]]:
        await self._ensure_cloned()

        if self.use_pushed_only:
            assert self.repo is not None
            results = []
            for f in self.repo.glob("**/*"):
                if f.is_file() and not self._is_blocked(str(f.relative_to(self.repo))):
                    try:
                        for i, line in enumerate(f.read_text().split("\n")):
                            if query in line:
                                results.append(
                                    (str(f.relative_to(self.repo)), i + 1, line)
                                )
                    except UnicodeDecodeError:
                        pass
        else:
            await self.send(
                WSMessage(
                    type=WSMessageType.FILE_RPC,
                    file_rpc=FileRPCSearchRequest(query=query),
                )
            )
            res = await self.recv()
            assert isinstance(res.file_rpc_response, FileRPCSearchResponse)
            results = res.file_rpc_response.results

        if overlay_modified:
            modified_files = await self._cache.get("output_modified_files", {})
            results = [
                (fn, ln, contents)
                for fn, ln, contents in results
                if fn not in modified_files
            ]
            for fn, content in modified_files.items():
                for i, line in enumerate(content.split("\n")):
                    if query in line:
                        results.append((fn, i + 1, line))

        return results

    async def cache(self, path: str, contents: Optional[str]):
        await self._cache.set(f"file_cache_{path}", contents)

    @property
    def has_pushed(self) -> bool:
        return self.repo is not None

    async def files_dict(self, overlay_modified: bool = False) -> dict[str, str]:
        await self._ensure_cloned()
        if not self.has_pushed:
            return {}
        assert self.repo is not None
        out = {}
        for f in self.repo.glob("**/*"):
            if f.is_file() and not self._is_blocked(str(f.relative_to(self.repo))):
                try:
                    out[str(f.relative_to(self.repo))] = f.read_text()
                except UnicodeDecodeError:
                    pass
        if overlay_modified:
            modified = await self._cache.get("output_modified_files", {})
            for fn, content in modified.items():
                if content != "BISMUTH_DELETED_FILE":
                    out[fn] = content
                else:
                    out.pop(fn, None)
        return out
