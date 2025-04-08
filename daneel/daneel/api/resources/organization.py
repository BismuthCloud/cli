import logging
from fastapi import APIRouter, HTTPException, Depends
from typing import Any, Optional
from datetime import datetime
import aiohttp
import httpx
from pydantic import BaseModel
from daneel.data.postgres.models import (
    OrganizationEntity,
    UserEntity,
    ProjectEntity,
    ChatMessageEntity,
    ChatSessionEntity,
    GenerationTraceEntity,
    HourlyUsageEntity,
)
from daneel.api.config import get_settings
from daneel.api.auth import get_current_user, get_organization_from_path

logger = logging.getLogger(__name__)
router = APIRouter()


@router.get("")
def list_organizations(current_user: UserEntity = Depends(get_current_user)):
    return current_user.organizations


@router.get("/{organization_id:int}")
def get_organization(
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    return organization


@router.post("/{organization_id:int}")
def update_organization(
    name: str,
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    organization.name = name
    organization.update()
    return organization


@router.get("/{organization_id:int}/usage")
def get_usage(
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    # Get usage data
    usage_rows = HourlyUsageEntity.db_manager().execute_query(
        """
        SELECT 
            p.id as project_id,
            hu.item,
            COALESCE(SUM(hu.usage), 0) as total_usage
        FROM projects p
        INNER JOIN features f ON f.projectId = p.id
        LEFT JOIN hourly_usage hu ON hu.featureId = f.id
        WHERE p.organizationId = %s
        GROUP BY p.id, hu.item
        """,
        (organization.id,),
    )

    # Get all projects
    projects = ProjectEntity.list(
        where="organizationId = %s", params=(organization.id,)
    )
    projects_dict = {p.id: p for p in projects}

    # Group usage by project
    project_usage: dict[int, dict[str, int]] = {}
    for row in usage_rows:
        if row["item"]:
            project_id = row["project_id"]
            if project_id not in project_usage:
                project_usage[project_id] = {}
            project_usage[project_id][row["item"]] = row["total_usage"]

    return {"projects": projects_dict, "projectUsage": project_usage}


@router.get("/{organization_id:int}/members")
def get_members(
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    return organization.users


class InviteRequest(BaseModel):
    email: str


@router.post("/{organization_id:int}/members")
def add_member(
    invite: InviteRequest,
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    # Check if user is already a member
    if any(u.email == invite.email for u in organization.users):
        raise HTTPException(status_code=409, detail="User is already a member")

    # Find or create user
    user = UserEntity.find_by(email=invite.email)
    if not user:
        user = UserEntity(
            name="", email=invite.email, username=invite.email, pending=True
        )
        user.persist()

    organization.add_user(user)

    return organization.users


@router.delete("/{organization_id:int}/members/{user_id:int}")
def remove_member(
    user_id: int,
    organization: OrganizationEntity = Depends(get_organization_from_path),
):
    if len(organization.users) == 1:
        return organization.users

    user = UserEntity.get(user_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    organization.remove_user(user)

    # TODO

    return organization.users
