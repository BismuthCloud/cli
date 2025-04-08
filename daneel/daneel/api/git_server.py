import asyncio
from enum import Enum
import subprocess
from typing import Annotated
from fastapi import APIRouter, Depends, HTTPException, Header, Request, Response
from fastapi.security import HTTPBasic, HTTPBasicCredentials
from git import Repo

from daneel.api.config import get_settings
from daneel.data.postgres.models import FeatureEntity, ProjectEntity

security = HTTPBasic()


def validate_git_auth(
    hash: str,
    credentials: Annotated[HTTPBasicCredentials, Depends(security)],
) -> ProjectEntity:
    p = ProjectEntity.find_by(hash=hash)
    if p is None or p.clone_token != credentials.password:
        raise HTTPException(status_code=403)
    return p


router = APIRouter(dependencies=[Depends(validate_git_auth)])
REPO_DIR = get_settings().vcs_projects_dir

# Based on https://github.com/meyer1994/gitserver


class Service(Enum):
    receive = "git-receive-pack"
    upload = "git-upload-pack"


@router.get("/{hash}/info/refs")
def info(hash: str, service: Service):
    path = REPO_DIR / hash

    out = subprocess.check_output(
        [service.value, "--stateless-rpc", "--advertise-refs", str(path)],
    )

    data = b"# service=" + service.value.encode()
    datalen = len(data) + 4
    data = (b"%04x" % datalen) + data + b"0000" + out

    return Response(
        content=data,
        media_type=f"application/x-{service.value}-advertisement",
    )


@router.post("/{hash}/{service}")
async def service(
    hash: str,
    service: Service,
    req: Request,
    project: ProjectEntity = Depends(validate_git_auth),
):
    path = REPO_DIR / hash

    proc = await asyncio.create_subprocess_exec(
        service.value,
        "--stateless-rpc",
        str(path),
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
    )
    out, _ = await proc.communicate(await req.body())

    resp = Response(
        content=out,
        media_type=f"application/x-{service.value}-result",
    )
    if service == Service.receive:
        project.has_pushed = True
        project.update()

        repo = Repo(path)
        have_features = {f.name: f for f in project.features}
        repo_features = {b.name for b in repo.branches}
        to_add = repo_features - set(have_features.keys())
        to_delete = set(have_features.keys()) - repo_features
        for feature in to_add:
            FeatureEntity(
                name=feature,
                project_id=project.id,
            ).persist()
        for feature in to_delete:
            FeatureEntity.delete(have_features[feature].id)

    return resp
