import asyncio
import logging
import os
import tempfile
from pathlib import Path

from git import Repo

from daneel.data.postgres.models import FeatureEntity

GIT_HOST = os.environ.get("GIT_HOST", "localhost:8080")


def get_clone_url(feature: FeatureEntity) -> str:
    return f"http://git:{feature.project.clone_token}@{GIT_HOST}/git/{feature.project.hash}"


async def clone_repo(feature: FeatureEntity) -> Path:
    repo_path = tempfile.mkdtemp(prefix=f"git-{feature.id}")
    logging.debug(
        f"Cloning project: {feature.project.hash} ({feature.project.id}) to {repo_path}"
    )

    os.environ["GIT_LFS_SKIP_SMUDGE"] = "1"
    await asyncio.to_thread(
        Repo.clone_from,
        get_clone_url(feature),
        repo_path,
        branch=feature.name,
        depth=1,
    )
    del os.environ["GIT_LFS_SKIP_SMUDGE"]

    return Path(repo_path)
