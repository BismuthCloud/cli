from pydantic import BaseModel
from fastapi import APIRouter, HTTPException, Depends
from daneel.data.graph_rag import GraphRag
from daneel.data.postgres.models import (
    OrganizationEntity,
    ProjectEntity,
)
from daneel.api.services.project import ProjectService
from daneel.api.services.vcs import VCSServiceClient
from daneel.api.config import get_settings
from daneel.api.auth import (
    get_organization_from_path,
    get_project_from_path,
)

router = APIRouter(dependencies=[Depends(get_organization_from_path)])

# Initialize services at module level
project_service = ProjectService()
vcs_service = VCSServiceClient()
settings = get_settings()


class CreateProjectRequest(BaseModel):
    name: str


@router.post("/")
def create_project(
    create: CreateProjectRequest,
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    # Check if project already exists
    existing = ProjectEntity.find_by(organization_id=organization.id, name=create.name)
    if existing:
        raise HTTPException(
            status_code=409, detail="A project with that name already exists"
        )

    project = project_service.create_project(
        name=create.name,
        organization=organization,
        vcs_service=vcs_service,
        initialize_repo=False,
    )
    return project


@router.get("/{project_id:int}")
def get_project(project: ProjectEntity = Depends(get_project_from_path)):
    return project


@router.delete("/{project_id:int}")
async def delete_project(project: ProjectEntity = Depends(get_project_from_path)):
    # Delete codegraphs
    for feature in project.features:
        try:
            graph = GraphRag(feature_id=feature.id)
            await graph.delete()
        except Exception as e:
            print("Failed to delete code graph")

    # Delete project
    ProjectEntity.delete(project.id)

    # Delete repository
    vcs_service.delete_repo(project.hash)

    return {"status": "success"}


@router.get("/list")
def list_projects(
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    projects = ProjectEntity.list(
        where="organizationId = %s",
        params=(organization.id,),
        order="createdAt ASC",
    )
    return {"projects": projects}
