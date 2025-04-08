from datetime import datetime
import random
import string
from typing import Optional
from daneel.api.config import get_settings
from fastapi import APIRouter, Depends, HTTPException, Response
from pydantic import BaseModel
from daneel.data.postgres.models import (
    UserEntity,
    APIKeyEntity,
)

from daneel.api.auth import get_current_user

router = APIRouter()


class APIKeyRequest(BaseModel):
    description: Optional[str] = None


@router.get("/enabled")
async def auth_enabled():
    settings = get_settings()
    return not settings.disable_auth


@router.get("/me")
async def me(current_user: UserEntity = Depends(get_current_user)):
    """Get current user information"""
    return {
        "id": current_user.id,
        "email": current_user.email,
        "username": current_user.username,
        "name": current_user.name,
        "organizations": current_user.organizations,
    }


@router.get("/apikey")
async def list_api_keys(current_user: UserEntity = Depends(get_current_user)):
    """List API keys for current user"""
    return [
        key for key in APIKeyEntity.list(where="userid = %s", params=(current_user.id,))
    ]


@router.post("/apikey")
async def create_api_key(
    request: APIKeyRequest, current_user: UserEntity = Depends(get_current_user)
):
    """Create new API key"""
    api_key = APIKeyEntity()
    api_key.user_id = current_user.id
    api_key.description = (
        request.description or f"Bismuth CLI {datetime.now().isoformat()}"
    )
    api_key.token = "BIS1-" + "".join(
        random.choices(string.ascii_letters + string.digits, k=32)
    )
    api_key.persist()
    return {"token": api_key.token}


@router.delete("/apikey/{key_id}")
async def delete_api_key(
    key_id: int, current_user: UserEntity = Depends(get_current_user)
):
    """Delete API key"""
    key = APIKeyEntity.get(key_id)
    if not key or key.user_id != current_user.id:
        raise HTTPException(status_code=404, detail="API key not found")
    APIKeyEntity.delete(key_id)
    return Response(status_code=200)
