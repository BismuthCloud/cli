from typing import Optional
from fastapi import APIRouter, Depends, HTTPException, Response
from pydantic import BaseModel
from daneel.data.postgres.models import (
    ChatMessageEntity,
    ChatSessionEntity,
    FeatureEntity,
    UserEntity,
)
from daneel.api.auth import (
    get_current_user,
    get_organization_from_path,
    get_project_from_path,
    get_feature_from_path,
)

router = APIRouter(
    prefix="/{feature_id:int}",
    tags=["features"],
    dependencies=[
        Depends(get_organization_from_path),
        Depends(get_project_from_path),
        Depends(get_feature_from_path),
    ],
)


class ChatSessionCreateRequest(BaseModel):
    name: Optional[str] = None


class FeedbackRequest(BaseModel):
    message_id: int
    upvote: Optional[bool] = None
    explanation: Optional[str] = None


class GenerationAcceptedRequest(BaseModel):
    message_id: int
    accepted: bool


class BugReport(BaseModel):
    message: str


@router.get("/chat/sessions")
async def list_chat_sessions(
    feature: FeatureEntity = Depends(get_feature_from_path),
    current_user: UserEntity = Depends(get_current_user),
):
    query = """
        SELECT * FROM chat_sessions 
        WHERE featureid = %s AND origin = %s 
        ORDER BY updatedat DESC
    """
    rows = ChatSessionEntity.db_manager().execute_query(
        query, (feature.id, f"USER_CHAT:{current_user.id}")
    )
    return [ChatSessionEntity.from_db_row(row) for row in rows]


@router.post("/chat/sessions")
async def create_chat_session(
    params: ChatSessionCreateRequest,
    feature: FeatureEntity = Depends(get_feature_from_path),
    current_user: UserEntity = Depends(get_current_user),
):
    session = ChatSessionEntity(
        feature_id=feature.id, origin=f"USER_CHAT:{current_user.id}", name=params.name
    )
    session.persist()
    return session


@router.put("/chat/sessions/{session_id}")
async def update_chat_session(
    session_id: int,
    params: ChatSessionCreateRequest,
    feature: FeatureEntity = Depends(get_feature_from_path),
):
    if params.name:
        query = """
            SELECT * FROM chat_sessions 
            WHERE featureid = %s AND name = %s AND id != %s
        """
        existing = ChatSessionEntity.db_manager().execute_and_fetch_one(
            query, (feature.id, params.name, session_id)
        )
        if existing:
            raise HTTPException(
                status_code=409, detail="A session with that name already exists"
            )

    query = """
        SELECT * FROM chat_sessions 
        WHERE featureid = %s AND id = %s
    """
    row = ChatSessionEntity.db_manager().execute_and_fetch_one(
        query, (feature.id, session_id)
    )

    if not row:
        raise HTTPException(status_code=404, detail="Session not found")

    session = ChatSessionEntity.from_db_row(row)
    session.name = params.name
    session.update()
    return session


@router.delete("/chat/sessions/{session_id}")
async def delete_chat_session(
    session_id: int,
    feature: FeatureEntity = Depends(get_feature_from_path),
):
    session = ChatSessionEntity.find_by(id=session_id, feature_id=feature.id)

    if not session:
        raise HTTPException(status_code=404, detail="Session not found")

    ChatSessionEntity.delete(session_id)
    return Response(status_code=200)


@router.get("/chat/sessions/{session_id}/list")
async def list_chat_messages(
    session_id: int,
    feature: FeatureEntity = Depends(get_feature_from_path),
):
    query = """
        SELECT m.* FROM chat_messages m
        JOIN chat_sessions s ON m.sessionid = s.id
        WHERE s.featureid = %s AND s.id = %s
        ORDER BY m.createdat ASC
    """
    rows = ChatMessageEntity.db_manager().execute_query(query, (feature.id, session_id))

    messages = [ChatMessageEntity.from_db_row(row) for row in rows]
    for message in messages:
        message.content = message.content.replace(
            "\n<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n", ""
        )

    return messages
