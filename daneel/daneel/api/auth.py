import base64
from typing import Annotated, Optional
from fastapi import Depends, HTTPException, Header
import httpx
import jwt
import os
import sentry_sdk

import logging
from daneel.api.config import get_settings
from daneel.data.postgres.models import (
    DBModel,
    SubscriptionEntity,
    UserEntity,
    APIKeyEntity,
    OrganizationEntity,
    ProjectEntity,
    FeatureEntity,
)

logger = logging.getLogger(__name__)

# Global variable to store the cached JWK set
_jwk_set = None


def _initialize_jwk_set():
    """Initialize the JWK set by fetching it once on startup"""
    global _jwk_set
    try:
        response = httpx.get(
            os.environ.get("KEYCLOAK_URL", "http://localhost:8543/realms/bismuth")
            + "/protocol/openid-connect/certs"
        )
        _jwk_set = jwt.PyJWKSet.from_dict(response.json())
    except Exception as e:
        logger.exception("Failed to fetch JWK set")
        _jwk_set = None


def ensure_registered(email: str) -> UserEntity:
    """Ensures user is registered and has an organization with subscription"""
    user = UserEntity.find_by(email=email)
    if user is None:
        settings = get_settings()

        userinfo_resp = httpx.get(
            os.environ.get("KEYCLOAK_URL", "http://localhost:8543/realms/bismuth")
            + "/protocol/openid-connect/userinfo",
            headers={"Authorization": f"Bearer {user.token}"},
        )
        userinfo_resp.raise_for_status()
        userinfo = userinfo_resp.json()
        print(userinfo)

        with DBModel.db_manager().get_cursor() as cursor:
            user = UserEntity(
                email=userinfo["email"],
                username=userinfo["preferred_username"],
                name=userinfo["name"],
            )
            user.persist(cursor=cursor)

            subscription = SubscriptionEntity(
                type="INDIVIDUAL",
                credits=settings.registration_default_credits,
            )
            subscription.persist(cursor=cursor)

            organization = OrganizationEntity(
                name=f"{user.name}'s Organization", subscription_id=subscription.id
            )
            organization.persist(cursor=cursor)

        organization.add_user(user)

    elif user.pending:
        # If user exists but is pending, update their information
        user.pending = False
        user.username = user.email
        user.name = user.email.split("@")[0]  # Basic name from email
        user.update()

    return user


def get_current_user(
    authorization: Annotated[str | None, Header()] = None,
) -> UserEntity:
    settings = get_settings()
    if settings.disable_auth:
        u = UserEntity.get(1)
        assert u is not None
        return u

    # First try OAuth2/JWT token
    if authorization.startswith("Bearer "):
        # Use the cached JWK set instead of fetching it every time
        global _jwk_set
        if _jwk_set is None:
            _initialize_jwk_set()
            if _jwk_set is None:
                raise HTTPException(
                    status_code=500, detail="Could not fetch JWT certificates"
                )

        try:
            token = authorization.split(" ", 1)[1]
            kid = jwt.get_unverified_header(token)["kid"]
            key = _jwk_set[kid]
            decoded_jwt = jwt.decode(token, key=key, algorithms=["RS256"])
            user = ensure_registered(decoded_jwt["email"])
            sentry_sdk.set_user(
                {
                    "id": user.id,
                    "username": user.username,
                    "email": user.email,
                }
            )
            return user
        except jwt.PyJWTError:
            raise HTTPException(status_code=401, detail="Invalid JWT token")

    # Then try API key via basic auth
    elif authorization.startswith("Basic "):
        try:
            # API key should be in the password field
            api_key = APIKeyEntity.find_by(
                token=base64.b64decode(authorization.split(" ", 1)[1]).split(b":")[1]
            )
            if api_key is None:
                raise HTTPException(status_code=401, detail="Invalid API key")
            user = UserEntity.get(api_key.user_id)
            if user is None:
                raise HTTPException(
                    status_code=401, detail="Invalid user associated with API key"
                )
            sentry_sdk.set_user(
                {
                    "id": user.id,
                    "username": user.username,
                    "email": user.email,
                }
            )
            return user
        except Exception:
            raise HTTPException(
                status_code=401, detail="Invalid API key authentication"
            )

    raise HTTPException(status_code=401, detail="No valid authentication provided")


def get_organization_access(
    organization_id: int,
    current_user: UserEntity = Depends(get_current_user),
) -> OrganizationEntity:
    """Dependency to check if user has access to an organization"""
    org = OrganizationEntity.get(organization_id)
    if not org or not any(o.id == org.id for o in current_user.organizations):
        raise HTTPException(status_code=404, detail="Organization not found")
    return org


def get_organization_from_path(
    organization_id: int, current_user: UserEntity = Depends(get_current_user)
):
    """Verify the user has access to the organization and return it."""
    if not any(org.id == organization_id for org in current_user.organizations):
        raise HTTPException(
            status_code=403, detail="Access denied to this organization"
        )

    org = OrganizationEntity.get(organization_id)
    if not org:
        raise HTTPException(status_code=404, detail="Organization not found")

    return org


def get_project_from_path(
    project_id: int,
    organization_id: int,
    current_user: UserEntity = Depends(get_current_user),
):
    """Verify the project belongs to the organization and return it."""
    # First check organization access
    if not any(org.id == organization_id for org in current_user.organizations):
        raise HTTPException(
            status_code=403, detail="Access denied to this organization"
        )

    project = ProjectEntity.find_by(id=project_id, organization_id=organization_id)
    if not project:
        raise HTTPException(status_code=404, detail="Project not found")

    return project


def get_feature_from_path(
    feature_id: int,
    project_id: int,
    organization_id: int,
    current_user: UserEntity = Depends(get_current_user),
):
    """Verify the feature belongs to the project and organization and return it."""
    # First check organization access
    if not any(org.id == organization_id for org in current_user.organizations):
        raise HTTPException(
            status_code=403, detail="Access denied to this organization"
        )

    # Check project belongs to organization
    project = ProjectEntity.find_by(id=project_id, organization_id=organization_id)
    if not project:
        raise HTTPException(status_code=404, detail="Project not found")

    # Get feature
    feature = FeatureEntity.find_by(id=feature_id, project_id=project_id)
    if not feature:
        raise HTTPException(status_code=404, detail="Feature not found")

    return feature
