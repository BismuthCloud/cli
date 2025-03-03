import os
import threading
import uuid
from datetime import datetime
from enum import Enum
from typing import Any, Dict, List, Optional, Type, TypeVar

import psycopg2.extras
from asimov.data.postgres.manager import DatabaseManager
from psycopg2.extras import Json

T = TypeVar("T", bound="DBModel")


class LazyAttribute:
    def __init__(self, func):
        self.func = func
        self.lock = threading.Lock()

    def __get__(self, instance, cls):
        if instance is None:
            return self
        attr_name = f"_{self.func.__name__}"
        if not hasattr(instance, attr_name):
            with self.lock:
                if not hasattr(instance, attr_name):
                    value = self.func(instance)
                    setattr(instance, attr_name, value)
        return getattr(instance, attr_name)


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
                col.type(row[col.db_name])
                if not isinstance(row[col.db_name], col.type)
                and row[col.db_name] is not None
                else row[col.db_name]
            )
            for col in cls.COLUMNS.values()
            if col.db_name in row
        }
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

    def to_dict(self) -> Dict[str, Any]:
        return {
            col.python_name: getattr(self, col.python_name)
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
        "is_ai": Column("is_ai", "isai", bool),
        "contains_code": Column("contains_code", "containscode", bool),
        "content": Column("content", "content", str),
        "user_id": Column("user_id", "userid", int, nullable=True),
        "message_llm_context": Column(
            "message_llm_context", "messagellmcontext", str, nullable=True
        ),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
        "code_block_spans": Column("code_block_spans", "codeblockspans", Json),
        "feedback_upvote": Column(
            "feedback_upvote", "feedbackupvote", bool, nullable=True
        ),
        "feedback": Column("feedback", "feedback", str, nullable=True),
        "session_id": Column("session_id", "sessionid", int),
        "request_id": Column("request_id", "requestid", str, nullable=True),
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
        "feature_id": Column("feature_id", "featureid", int),
        "origin": Column("origin", "origin", str),
        "name": Column("name", "name", str, nullable=True),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "context_storage": Column(
            "context_storage", "contextstorage", Json, nullable=False
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
        if not self.context_storage:
            return {}

        return self.context_storage.__dict__.get("adapted", {})

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
        "latest_saved_change": Column(
            "latest_saved_change", "latestsavedchange", str, nullable=True
        ),
        "project_id": Column("project_id", "projectid", int),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "function_uuid": Column(
            "function_uuid", "functionuuid", uuid.UUID, nullable=True
        ),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS features (
        id SERIAL PRIMARY KEY,
        name TEXT,
        latestsavedchange TEXT,
        projectid BIGINT,
        createdat TIMESTAMP,
        updatedat TIMESTAMP,
        functionuuid UUID
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        name: str = "",
        latest_saved_change: Optional[str] = None,
        project_id: Optional[int] = None,
        created_at: Optional[datetime] = None,
        updated_at: Optional[datetime] = None,
        function_uuid: Optional[uuid.UUID] = None,
    ):
        self.id = id
        self.name = name
        self.latest_saved_change = latest_saved_change
        self.project_id = project_id
        self.created_at = created_at or datetime.now()
        self.updated_at = updated_at or datetime.now()
        self.function_uuid = function_uuid

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
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
        "chat_message_id": Column(
            "chat_message_id", "chatmessageid", int, nullable=True
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
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
        "name": Column("name", "name", str),
        "hash": Column("hash", "hash", str),
        "organization_id": Column("organization_id", "organizationid", int),
        "clone_token": Column("clone_token", "internalclonetoken", str),
        "github_app_install_id": Column(
            "github_app_install_id", "githubappinstallid", int, nullable=True
        ),
        "github_repo": Column("github_repo", "githubrepo", str, nullable=True),
        "github_config": Column("github_config", "githubconfig", dict, nullable=True),
        "has_pushed": Column("has_pushed", "haspushed", bool),
        "atlassian_install_id": Column(
            "atlassian_install_id", "atlassianinstallid", int, nullable=True
        ),
        "bitbucket_repo": Column("bitbucket_repo", "bitbucketrepo", str, nullable=True),
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
        githubappinstallid BIGINT,
        githubrepo TEXT,
        githubconfig JSONB,
        haspushed BOOLEAN DEFAULT FALSE,
        atlassianinstallid BIGINT,
        bitbucketrepo TEXT
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
        github_app_install_id: Optional[int] = None,
        github_repo: Optional[str] = None,
        github_config: Optional[dict] = None,
        has_pushed: bool = False,
        atlassian_install_id: Optional[int] = None,
        bitbucket_repo: Optional[str] = None,
    ):
        self.id = id
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.name = name
        self.hash = hash
        self.organization_id = organization_id
        self.clone_token = clone_token
        self.github_app_install_id = github_app_install_id
        self.github_repo = github_repo
        self.github_config = github_config or {}
        self.has_pushed = has_pushed
        self.atlassian_install_id = atlassian_install_id
        self.bitbucket_repo = bitbucket_repo

    @LazyAttribute
    def organization(self):
        return OrganizationEntity.get(self.organization_id)

    @LazyAttribute
    def features(self):
        return FeatureEntity.list(where="projectid = %s", params=(self.id,))

    @LazyAttribute
    def github_app_install(self):
        if self.github_app_install_id is None:
            return None
        return GitHubAppInstallEntity.get(self.github_app_install_id)

    @LazyAttribute
    def atlassian_install(self):
        if self.atlassian_install_id is None:
            return None
        return AtlassianAppInstallEntity.get(self.atlassian_install_id)


class GitHubAppInstallEntity(DBModel):
    TABLE_NAME = "github_app_installs"
    COLUMNS = {
        "installation_id": Column("installation_id", "installationid", int),
        "organization_id": Column("organization_id", "orgid", int),
        "access_token": Column("access_token", "accesstoken", str),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
    }

    def __init__(
        self,
        installation_id: Optional[int] = None,
        organization_id: Optional[int] = None,
        access_token: Optional[str] = None,
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
    ):
        self.installation_id = installation_id
        self.organization_id = organization_id
        self.access_token = access_token
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()

    @LazyAttribute
    def organization(self):
        return OrganizationEntity.get(self.organization_id)

    @classmethod
    def get(
        cls, installation_id: int, cursor=None
    ) -> Optional["GitHubAppInstallEntity"]:
        return cls.find_by(installation_id=installation_id, cursor=cursor)


class FileEntity(DBModel):
    TABLE_NAME = "files"
    COLUMNS = {
        "id": Column("id", "id", int),
        "type": Column("type", "type", str),
        "hash": Column("hash", "hash", str),
        "name": Column("name", "name", str),
        "path_in_project": Column("path_in_project", "pathinproject", str),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
        "feature_id": Column("feature_id", "featureid", int),
    }

    CREATE_TABLE_SQL = """
    CREATE TABLE IF NOT EXISTS files (
        id SERIAL PRIMARY KEY,
        type TEXT,
        hash TEXT,
        name TEXT,
        pathinproject TEXT,
        updatedat TIMESTAMP,
        createdat TIMESTAMP,
        featureid BIGINT 
    )
    """

    def __init__(
        self,
        id: Optional[int] = None,
        type: str = "",
        hash: str = "",
        name: str = "",
        path_in_project: str = "",
        updated_at: Optional[datetime] = None,
        created_at: Optional[datetime] = None,
        feature_id: Optional[int] = None,
    ):
        self.id = id
        self.type = type
        self.hash = hash
        self.name = name
        self.path_in_project = path_in_project
        self.updated_at = updated_at or datetime.now()
        self.created_at = created_at or datetime.now()
        self.feature_id = feature_id

    @LazyAttribute
    def feature(self):
        return FeatureEntity.get(self.feature_id)


class APIKeyEntity(DBModel):
    TABLE_NAME = "api_keys"
    COLUMNS = {
        "id": Column("id", "id", int),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
        "user_id": Column("user_id", "userid", int),
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


class OrganizationEntity(DBModel):
    TABLE_NAME = "organizations"
    COLUMNS = {
        "id": Column("id", "id", int),
        "name": Column("name", "name", str),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "subscription_id": Column(
            "subscription_id", "subscriptionid", int, nullable=True
        ),
        "llm_config": Column("llm_config", "llmconfig", dict, nullable=True),
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
            if self.subscription_id
            else None
        )

    @LazyAttribute
    def users(self):
        return get_users_for_organization(self.id)


class SubscriptionType(Enum):
    INDIVIDUAL = "INDIVIDUAL"
    PROFESSIONAL = "PROFESSIONAL"
    TEAM = "TEAM"
    ENT = "ENT"


class SubscriptionEntity(DBModel):
    TABLE_NAME = "subscriptions"
    COLUMNS = {
        "id": Column("id", "id", int),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
        "customer_id": Column("customer_id", "customerid", str),
        "subscription_id": Column("subscription_id", "subscriptionid", str),
        "type": Column("type", "type", SubscriptionType),
        "expires_at": Column("expires_at", "expiresat", datetime),
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
        "updated_at": Column("updated_at", "updatedat", datetime),
        "created_at": Column("created_at", "createdat", datetime),
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
        "feature_id": Column("feature_id", "featureid", int),
        "org_id": Column("org_id", "orgid", int),
        "time": Column("time", "time", datetime),
        "item": Column("item", "item", str),
        "usage": Column("usage", "usage", int),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
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
        "chat_message_id": Column("chat_message_id", "chatmessageid", int),
        "created_at": Column("created_at", "createdat", datetime),
        "updated_at": Column("updated_at", "updatedat", datetime),
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


def add_user_to_organization(user_id: int, org_id: int):
    query = """
        INSERT INTO organization_users (orgid, userid)
        VALUES (%s, %s)
    """
    DBModel.db_manager().execute_query(query, (org_id, user_id))


def create_all_tables():
    ChatSessionEntity.create_table()
    ChatMessageEntity.create_table()
    FeatureEntity.create_table()
    GenerationAnalysisEntity.create_table()
    ProjectEntity.create_table()
    FileEntity.create_table()
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
