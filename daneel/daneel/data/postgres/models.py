import os
import threading
from datetime import datetime
from enum import Enum
from typing import Any, Callable, Dict, Generic, List, Optional, Type, TypeVar

import psycopg2.extras
from asimov.data.postgres.manager import DatabaseManager
from psycopg2.extras import Json

T = TypeVar("T", bound="DBModel")
R = TypeVar("R")


class LazyAttribute(Generic[R]):
    def __init__(self, func: Callable[[Any], R]):
        self.func = func
        self.lock = threading.Lock()

    def __get__(self, instance: Any, cls: Any) -> R:
        if instance is None:
            # When accessed from the class, return the descriptor itself
            return self  # type: ignore

        attr_name = f"_{self.func.__name__}"
        if not hasattr(instance, attr_name):
            with self.lock:
                if not hasattr(instance, attr_name):
                    value = self.func(instance)
                    setattr(instance, attr_name, value)
        return getattr(instance, attr_name)

    def invalidate(self, instance: Any) -> None:
        attr_name = f"_{self.func.__name__}"
        if hasattr(instance, attr_name):
            delattr(instance, attr_name)


class Column:
    def __init__(
        self, python_name: str, db_name: str, type: Any, nullable: bool = False
    ):
        self.python_name = python_name
        self.db_name = db_name
        self.type = type
        self.nullable = nullable


class DBModel:
    TABLE_NAME: str
    COLUMNS: Dict[str, Column] = {}
    CREATE_TABLE_SQL: str

    @classmethod
    def db_manager(cls):
        db_manager = DatabaseManager(
            dsn=os.environ.get(
                "POSTGRES_DSN", "postgresql://quarkus:quarkus@localhost:5432/quarkus"
            )
        )

        return db_manager

    @classmethod
    def create_table(cls):
        cls.db_manager().execute_query(cls.CREATE_TABLE_SQL)

    @classmethod
    def from_db_row(cls: Type[T], row: dict) -> T:
        python_dict = {
            col.python_name: (
                col.type(row[col.db_name.lower()])
                if not isinstance(row[col.db_name.lower()], col.type)
                and row[col.db_name.lower()] is not None
                else row[col.db_name.lower()]
            )
            for col in cls.COLUMNS.values()
            if col.db_name.lower() in row
        }
        for col in cls.COLUMNS.values():
            if col.type == Json and isinstance(python_dict[col.python_name], Json):
                python_dict[col.python_name] = python_dict[col.python_name].adapted
        return cls(**python_dict)

    @classmethod
    def get(cls: Type[T], id: Any, cursor=None) -> Optional[T]:
        query = f"SELECT * FROM {cls.TABLE_NAME} WHERE id = %s"
        row = cls.db_manager().execute_and_fetch_one(query, params=(id,), cursor=cursor)
        if row is None:
            return None
        return cls.from_db_row(row)

    @classmethod
    def get_many(cls: Type[T], ids: List[int], cursor=None) -> List[T]:
        placeholders = ",".join(["%s"] * len(ids))
        query = f"SELECT * FROM {cls.TABLE_NAME} WHERE id IN ({placeholders})"
        rows = cls.db_manager().execute_query(query, params=tuple(ids), cursor=None)
        return [cls.from_db_row(row) for row in rows]

    @classmethod
    def list(
        cls: Type[T],
        where: Optional[str] = None,
        order=None,
        params: Optional[tuple] = None,
        cursor=None,
    ) -> List[T]:
        query = f"SELECT * FROM {cls.TABLE_NAME}"
        if where:
            query += f" WHERE {where}"

        if order:
            query += f" ORDER BY {order}"
        rows = cls.db_manager().execute_query(query, params=params, cursor=cursor)
        return [cls.from_db_row(row) for row in rows]

    @classmethod
    def find_by(cls: Type[T], **kwargs) -> Optional[T]:
        where = []
        values = []
        for column, value in kwargs.items():
            db_column = next(
                (
                    col.db_name
                    for col in cls.COLUMNS.values()
                    if col.python_name == column
                ),
                column,
            )
            where.append(f"{db_column} = %s")
            values.append(value)
        query = f"SELECT * FROM {cls.TABLE_NAME} WHERE {' AND '.join(where)}"
        row = cls.db_manager().execute_and_fetch_one(query, values)
        if row is None:
            return None
        return cls.from_db_row(row)

    def to_db_dict(self) -> Dict[str, Any]:
        db_dict = {
            col.db_name: getattr(self, col.python_name)
            for col in self.COLUMNS.values()
            if hasattr(self, col.python_name)
        }

        for col in self.COLUMNS.values():
            if col.type == Json and not isinstance(db_dict.get(col.db_name), Json):
                db_dict[col.db_name] = Json(db_dict[col.db_name])

        return db_dict

    def to_json_dict(self) -> Dict[str, Any]:
        return {
            col.db_name: getattr(self, col.python_name)
            for col in self.COLUMNS.values()
            if hasattr(self, col.python_name)
        }

    def update(self, cursor=None):
        db_dict = self.to_db_dict()
        if "updatedat" in db_dict:
            db_dict["updatedat"] = datetime.now()
        set_clause = ", ".join(f"{k} = %s" for k in db_dict.keys() if k != "id")
        values = [
            (v if not isinstance(v, Enum) else v.value)
            for k, v in db_dict.items()
            if k != "id"
        ]
        values.append(self.id)

        query = f"UPDATE {self.__class__.TABLE_NAME} SET {set_clause} WHERE id = %s"

        with self.__class__.db_manager().get_cursor() as cur:
            if cursor is not None:
                cur = cursor
            cur.execute(query, tuple(values))

    @classmethod
    def delete(cls, id: int, cursor=None):
        query = f"DELETE FROM {cls.TABLE_NAME} WHERE id = %s"
        with cls.db_manager().get_cursor() as cur:
            if cursor is not None:
                cur = cursor
            cur.execute(query, (id,))

    @classmethod
    def delete_many(cls, ids: List[int], cursor=None):
        if ids:
            placeholders = ",".join(["%s"] * len(ids))
            query = f"DELETE FROM {cls.TABLE_NAME} WHERE id IN ({placeholders})"
            with cls.db_manager().get_cursor() as cur:
                if cursor is not None:
                    cur = cursor
                cur.execute(query, tuple(ids))

    def persist(self):
        if hasattr(self, "id") and self.id is not None:
            self.update()
        else:
            self.save()
        return self

    @classmethod
    def insert_many(cls: Type[T], items: List[T], cursor=None):
        if not items:
            return
        columns = [c.db_name for c in cls.COLUMNS.values()]
        columns.remove("id")  # Remove id from insert, it's auto-generated
        query = f"INSERT INTO {cls.TABLE_NAME} ({', '.join(columns)}) VALUES %s"

        with cls.db_manager().get_cursor() as cur:
            if cursor is not None:
                cur = cursor
            psycopg2.extras.execute_values(
                cur,
                query,
                [[item.to_db_dict().get(c) for c in columns] for item in items],
                page_size=len(items),
            )

    def save(self, cursor=None):
        db_dict = self.to_db_dict()
        if "id" in db_dict:
            del db_dict["id"]  # Remove id from insert, it's auto-generated
        columns = ", ".join(db_dict.keys())
        placeholders = ", ".join(["%s"] * len(db_dict))
        query = f"INSERT INTO {self.__class__.TABLE_NAME} (id, {columns}) VALUES (nextval('{self.__class__.TABLE_NAME}_seq'), {placeholders}) RETURNING id"

        self.id = self.__class__.db_manager().execute_and_return_id(
            query, params=tuple(db_dict.values()), cursor=cursor
        )
        return self


class ChatMessageEntity(DBModel):
    TABLE_NAME = "chat_messages"
    COLUMNS = {
        "id": Column("id", "id", int),
        "is_ai": Column("is_ai", "isAI", bool),
        "contains_code": Column("contains_code", "containsCode", bool),
        "content": Column("content", "content", str),
        "user_id": Column("user_id", "userId", int, nullable=True),
        "message_llm_context": Column(
            "message_llm_context", "messageLLMContext", str, nullable=True
        ),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "created_at": Column("created_at", "createdAt", datetime),
        "feedback_upvote": Column(
            "feedback_upvote", "feedbackUpvote", bool, nullable=True
        ),
        "feedback": Column("feedback", "feedback", str, nullable=True),
        "session_id": Column("session_id", "sessionId", int),
        "request_id": Column("request_id", "requestId", str, nullable=True),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS chat_messages (
        id SERIAL PRIMARY KEY,
        isai BOOLEAN,
        containscode BOOLEAN,
        content TEXT,
        userid BIGINT,
        messagellmcontext TEXT,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        codeblockspans JSONB,
        feedbackupvote BOOLEAN,
        feedback TEXT,
        sessionid BIGINT,
        requestid TEXT
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        is_ai: bool = False,
        contains_code: bool = False,
        content: str = "",
        user_id: Optional[int] = None,
        message_llm_context: Optional[str] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        code_block_spans: List[dict] = [],
        feedback_upvote: Optional[bool] = None,
        feedback: Optional[str] = None,
        session_id: Optional[int] = None,
        request_id: Optional[str] = None,
    ):
        self.id = id
        self.is_ai = is_ai
        self.contains_code = contains_code
        self.content = content
        self.user_id = user_id
        self.message_llm_context = message_llm_context
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.code_block_spans = code_block_spans
        self.feedback_upvote = feedback_upvote
        self.feedback = feedback
        self.session_id = session_id
        self.request_id = request_id

    @LazyAttribute
    def user(self):
        return UserEntity.get(self.user_id) if self.user_id else None

    @LazyAttribute
    def session(self):
        return ChatSessionEntity.get(self.session_id)


class ChatSessionEntity(DBModel):
    TABLE_NAME = "chat_sessions"
    COLUMNS = {
        "id": Column("id", "id", int),
        "feature_id": Column("feature_id", "featureId", int),
        "origin": Column("origin", "origin", str),
        "name": Column("name", "name", str, nullable=True),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "context_storage": Column(
            "context_storage", "contextStorage", Json, nullable=False
        ),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS chat_sessions (
        id        SERIAL    PRIMARY KEY,
        featureId BIGINT    NOT NULL,
        origin    TEXT      NOT NULL,
        name      TEXT,
        createdAt TIMESTAMP,
        updatedAt TIMESTAMP,
        contextStorage JSONB
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        feature_id: Optional[int] = None,
        origin: str = "",
        name: Optional[str] = None,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
        context_storage: Optional[Dict] = None,
    ):
        self.id = id
        self.feature_id = feature_id
        self.origin = origin
        self.name = name
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()
        self.context_storage = context_storage

    @LazyAttribute
    def feature(self):
        return FeatureEntity.get(self.feature_id)

    @LazyAttribute
    def chat_messages(self):
        return ChatMessageEntity.list(
            where="sessionid = %s", order="id ASC", params=(self.id,)
        )

    def get_context(self) -> Dict[str, Any]:
        """Returns the stored context as a dict, returns empty dict if None or on error."""
        return self.context_storage or {}

    def set_context(self, context: Dict[str, Any]) -> None:
        """Stores the provided dict as JSON."""
        self.context_storage = context
        self.update()

    def update_context(self, key: str, value: Any) -> None:
        """Updates a single key in the context."""
        current_context = self.get_context()
        current_context[key] = value
        self.set_context(current_context)

    def clear_context(self) -> None:
        """Sets context_storage to None."""
        self.context_storage = {}
        self.update()


class FeatureEntity(DBModel):
    TABLE_NAME = "features"
    COLUMNS = {
        "id": Column("id", "id", int),
        "name": Column("name", "name", str),
        "project_id": Column("project_id", "projectId", int),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS features (
        id SERIAL PRIMARY KEY,
        name TEXT,
        projectid BIGINT,
        createdat TIMESTAMP,
        updatedat TIMESTAMP
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        name: str = "",
        project_id: Optional[int] = None,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
    ):
        self.id = id
        self.name = name
        self.project_id = project_id
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()

    @LazyAttribute
    def project(self):
        return ProjectEntity.get(self.project_id)

    @LazyAttribute
    def sessions(self):
        return ChatSessionEntity.list(
            where="featureid = %s", order="id ASC", params=(self.id,)
        )


class GenerationAnalysisEntity(DBModel):
    TABLE_NAME = "generation_analysis"
    COLUMNS = {
        "id": Column("id", "id", int),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "created_at": Column("created_at", "createdAt", datetime),
        "chat_message_id": Column(
            "chat_message_id", "chatMessageId", int, nullable=True
        ),
        "generation": Column("generation", "generation", str),
        "mypy": Column("mypy", "mypy", str, nullable=True),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS generation_analysis (
        id SERIAL PRIMARY KEY,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        chatmessageid BIGINT,
        generation TEXT,
        mypy TEXT
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        chat_message_id: Optional[int] = None,
        generation: str = "",
        mypy: Optional[str] = None,
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.chat_message_id = chat_message_id
        self.generation = generation
        self.mypy = mypy


class ProjectEntity(DBModel):
    TABLE_NAME = "projects"
    COLUMNS = {
        "id": Column("id", "id", int),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "created_at": Column("created_at", "createdAt", datetime),
        "name": Column("name", "name", str),
        "hash": Column("hash", "hash", str),
        "organization_id": Column("organization_id", "organizationId", int),
        "clone_token": Column("clone_token", "internalCloneToken", str),
        "has_pushed": Column("has_pushed", "hasPushed", bool),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS projects (
        id SERIAL PRIMARY KEY,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        name TEXT,
        hash TEXT,
        organizationid BIGINT,
        internalclonetoken TEXT,
        haspushed BOOLEAN DEFAULT FALSE,
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        name: str = "",
        hash: str = "",
        organization_id: Optional[int] = None,
        clone_token: str = "",
        has_pushed: bool = False,
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.name = name
        self.hash = hash
        self.organization_id = organization_id
        self.clone_token = clone_token
        self.has_pushed = has_pushed

    @LazyAttribute
    def organization(self):
        return OrganizationEntity.get(self.organization_id)

    @LazyAttribute
    def features(self):
        return FeatureEntity.list(where="projectid = %s", params=(self.id,))

    def to_json_dict(self) -> Dict[str, Any]:
        d = super().to_json_dict()
        d["cloneToken"] = d["internalCloneToken"]
        del d["internalCloneToken"]
        d["features"] = [f.to_json_dict() for f in self.features]
        return d


class APIKeyEntity(DBModel):
    TABLE_NAME = "api_keys"
    COLUMNS = {
        "id": Column("id", "id", int),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "created_at": Column("created_at", "createdAt", datetime),
        "user_id": Column("user_id", "userId", int),
        "token": Column("token", "token", str),
        "description": Column("description", "description", str),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS api_keys (
        id SERIAL PRIMARY KEY,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        userid BIGINT,
        token TEXT,
        description TEXT
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        user_id: Optional[int] = None,
        token: str = "",
        description: str = "",
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.user_id = user_id
        self.token = token
        self.description = description

    @LazyAttribute
    def user(self):
        return UserEntity.get(self.user_id)

    def to_json_dict(self) -> Dict[str, Any]:
        d = super().to_json_dict()
        del d["token"]
        return d


class OrganizationEntity(DBModel):
    TABLE_NAME = "organizations"
    COLUMNS = {
        "id": Column("id", "id", int),
        "name": Column("name", "name", str),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "subscription_id": Column("subscription_id", "subscriptionId", int),
        "llm_config": Column("llm_config", "llmConfig", dict, nullable=True),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS organizations (
        id SERIAL PRIMARY KEY,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        name TEXT,
        subscriptionid BIGINT,
        llmConfig JSON
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        name: str = "",
        subscription_id: Optional[int] = None,
        llm_config: Optional[dict] = None,
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.name = name
        self.subscription_id = subscription_id
        self.llm_config = llm_config

    @LazyAttribute
    def subscription(self):
        return (
            SubscriptionEntity.get(self.subscription_id)
            if self.subscription_id is not None
            else None
        )

    @LazyAttribute
    def users(self):
        return get_users_for_organization(self.id)

    def add_user(self, user: "UserEntity") -> None:
        query = "INSERT INTO organization_users (orgid, userid) VALUES (%s, %s)"
        DBModel.db_manager().execute_query(query, (self.id, user.id))
        OrganizationEntity.users.invalidate(self)  # type: ignore

    def remove_user(self, user: "UserEntity") -> None:
        query = "DELETE FROM organization_users WHERE orgid = %s AND userid = %s"
        DBModel.db_manager().execute_query(query, (self.id, user.id))
        OrganizationEntity.users.invalidate(self)  # type: ignore

    def to_json_dict(self) -> Dict[str, Any]:
        d = super().to_json_dict()
        d["subscription"] = self.subscription
        del d["subscriptionId"]
        return d


class SubscriptionType(Enum):
    INDIVIDUAL = "INDIVIDUAL"
    PROFESSIONAL = "PROFESSIONAL"
    TEAM = "TEAM"
    ENT = "ENT"


class SubscriptionEntity(DBModel):
    TABLE_NAME = "subscriptions"
    COLUMNS = {
        "id": Column("id", "id", int),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "customer_id": Column("customer_id", "customerId", str),
        "subscription_id": Column("subscription_id", "subscriptionId", str),
        "type": Column("type", "type", SubscriptionType),
        "expires_at": Column("expires_at", "expiresAt", datetime),
        "credits": Column("credits", "credits", int),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS subscriptions (
        id SERIAL PRIMARY KEY,
        createdat TIMESTAMP,
        updatedat TIMESTAMP,
        customerid TEXT,
        subscriptionid TEXT,
        type TEXT DEFAULT 'INDIVIDUAL',
        expiresat TIMESTAMP,
        credits INTEGER DEFAULT 0
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
        customer_id: Optional[str] = None,
        subscription_id: Optional[str] = None,
        type: Optional[str] = None,
        expires_at: Optional[datetime] = None,
        credits: int = 0,
    ):
        self.id = id
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()
        self.customer_id = customer_id
        self.subscription_id = subscription_id
        self.type = type
        self.expires_at = expires_at
        self.credits = credits


class UserEntity(DBModel):
    TABLE_NAME = "users"
    COLUMNS = {
        "id": Column("id", "id", int),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "created_at": Column("created_at", "createdAt", datetime),
        "email": Column("email", "email", str),
        "username": Column("username", "username", str),
        "name": Column("name", "name", str),
        "pending": Column("pending", "pending", bool),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS users (
        id SERIAL PRIMARY KEY,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        email TEXT NOT NULL,
        username TEXT NOT NULL UNIQUE,
        name TEXT,
        pending BOOLEAN NOT NULL
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        email: str = "",
        username: str = "",
        name: str = "",
        pending: bool = False,
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.email = email
        self.username = username
        self.name = name
        self.pending = pending

    @LazyAttribute
    def organizations(self):
        return get_organizations_for_user(self.id)


class HourlyUsageEntity(DBModel):
    TABLE_NAME = "hourly_usage"
    CREATE_TABLE_SQL = """
    create table if not exists hourly_usage
    (
        id             bigint    not null,
        featureId      bigint    not null,
        orgId          bigint    not null,
        time           timestamptz not null,
        item           text      not null,
        usage          bigint    not null,
        createdAt      timestamp not null,
        updatedAt      timestamp not null,
        primary key (id),
        unique (time, item, featureId)
    );
    """
    COLUMNS = {
        "id": Column("id", "id", int),
        "feature_id": Column("feature_id", "featureId", int),
        "org_id": Column("org_id", "orgId", int),
        "time": Column("time", "time", datetime),
        "item": Column("item", "item", str),
        "usage": Column("usage", "usage", int),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
    }

    def __init__(
        self,
        id: Optional[int] = None,
        feature_id: Optional[int] = None,
        org_id: Optional[int] = None,
        time: Optional[datetime] = None,
        item: str = "",
        usage: int = 0,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
    ):
        self.id = id
        self.feature_id = feature_id
        self.org_id = org_id
        self.time = time
        self.item = item
        self.usage = usage
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()

    @LazyAttribute
    def feature(self):
        return FeatureEntity.get(self.feature_id)

    @LazyAttribute
    def organization(self):
        return OrganizationEntity.get(self.org_id)

    @classmethod
    def account_usage(
        cls, feature: FeatureEntity, time: datetime, item: str, usage: int
    ):
        query = "INSERT INTO hourly_usage (id, featureId, orgId, time, item, usage, createdAt, updatedAt) VALUES (nextval('hourly_usage_seq'), %s, %s, %s, %s, %s, NOW(), NOW()) ON CONFLICT (time, item, featureId) DO UPDATE SET usage = hourly_usage.usage + EXCLUDED.usage, updatedAt = NOW()"
        cls.db_manager().execute_query(
            query, (feature.id, feature.project.organization_id, time, item, usage)
        )


class GenerationTraceEntity(DBModel):
    TABLE_NAME = "generation_traces"
    CREATE_TABLE_SQL = """
    CREATE TABLE generation_traces (
        id BIGINT NOT NULL,
        chatMessageId BIGINT NOT NULL,
        createdAt timestamp not null,
        updatedAt timestamp not null,
        state JSONB NOT NULL,
        PRIMARY KEY (id),
        CONSTRAINT fk_chat_message FOREIGN KEY (chatMessageId) REFERENCES chat_messages ON DELETE CASCADE
    );
    """
    COLUMNS = {
        "id": Column("id", "id", int),
        "chat_message_id": Column("chat_message_id", "chatMessageId", int),
        "created_at": Column("created_at", "createdAt", datetime),
        "updated_at": Column("updated_at", "updatedAt", datetime),
        "state": Column("state", "state", Json),
    }

    def __init__(
        self,
        id: Optional[int] = None,
        chat_message_id: Optional[int] = None,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
        state: dict = {},
    ):
        self.id = id
        self.chat_message_id = chat_message_id
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()
        self.state = state

    @LazyAttribute
    def chat_message(self):
        return ChatMessageEntity.get(self.chat_message_id)


# Helper functions


def get_organizations_for_user(user_id: int) -> List[OrganizationEntity]:
    query = """
        SELECT o.* FROM organizations o
        JOIN organization_users ou ON o.id = ou.orgid
        WHERE ou.userid = %s
    """
    rows = DBModel.db_manager().execute_query(query, (user_id,))
    return [OrganizationEntity.from_db_row(row) for row in rows]


def get_users_for_organization(org_id: int) -> List[UserEntity]:
    query = """
        SELECT u.* FROM users u
        JOIN organization_users ou ON u.id = ou.userid
        WHERE ou.orgid = %s
    """
    rows = DBModel.db_manager().execute_query(query, (org_id,))
    return [UserEntity.from_db_row(row) for row in rows]


def create_all_tables():
    ChatSessionEntity.create_table()
    ChatMessageEntity.create_table()
    FeatureEntity.create_table()
    GenerationAnalysisEntity.create_table()
    ProjectEntity.create_table()
    APIKeyEntity.create_table()
    OrganizationEntity.create_table()
    SubscriptionEntity.create_table()
    UserEntity.create_table()
    GenerationTraceEntity.create_table()

    # Create the many-to-many relationship table
    DBModel.db_manager().execute_query(
        """
        CREATE TABLE IF NOT EXISTS organization_users (
            orgid BIGINT,
            userid BIGINT,
            PRIMARY KEY (orgid, userid),
            FOREIGN KEY (orgid) REFERENCES organizations (id),
            FOREIGN KEY (userid) REFERENCES users (id)
        )
    """
    )
