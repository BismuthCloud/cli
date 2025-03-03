from enum import Enum
from typing import Any, Dict, List, Literal, Optional, Union

from asimov.asimov_base import AsimovBase
from pydantic import ConfigDict, Field


class ResponseStateEnum(Enum):
    WRITING = 0
    ANALYZING = 1
    TESTING = 2
    DYNAMIC = 3


class WSMessageType(Enum):
    AUTH = "AUTH"
    PING = "PING"
    CHAT = "CHAT"
    RESPONSE_STATE = "RESPONSE_STATE"
    RUN_COMMAND = "RUN_COMMAND"
    RUN_COMMAND_RESPONSE = "RUN_COMMAND_RESPONSE"
    FILE_RPC = "FILE_RPC"
    FILE_RPC_RESPONSE = "FILE_RPC_RESPONSE"
    ACI = "ACI"
    KILL_GENERATION = "KILL_GENERATION"
    USAGE = "USAGE"
    SWITCH_MODE = "SWITCH_MODE"
    SWITCH_MODE_RESPONSE = "SWITCH_MODE_RESPONSE"
    PIN_FILE = "PIN_FILE"
    PIN_FILE_RESPONSE = "PIN_FILE_RESPONSE"


class WSSerializeBase(AsimovBase):
    model_config = ConfigDict(populate_by_name=True)


class AuthMessage(WSSerializeBase):
    token: str
    feature_id: int = Field(alias="featureId")
    session_id: int = Field(alias="sessionId")


class ChatModifiedFile(WSSerializeBase):
    name: str
    project_path: str = Field(alias="projectPath")
    content: str
    deleted: bool = False


class ChatMessage(WSSerializeBase):
    message: str
    modified_files: List[ChatModifiedFile] = Field(
        default_factory=list, alias="modifiedFiles"
    )
    request_type_analysis: bool = Field(default=False, alias="requestTypeAnalysis")


class ResponseState(WSSerializeBase):
    state: str
    attempt: int


class RunCommandMessage(WSSerializeBase):
    output_modified_files: List[ChatModifiedFile] = Field(default_factory=list)
    command: str


class PinFileMessage(WSSerializeBase):
    path: str


class RunCommandResponse(WSSerializeBase):
    exit_code: int
    output: str
    modified_files: List[ChatModifiedFile] = Field(default_factory=list)


class ACIMessage(WSSerializeBase):
    class Action(Enum):
        START = "START"
        STATUS = "STATUS"
        CREATE = "CREATE"
        SCROLL = "SCROLL"
        SWITCH = "SWITCH"
        CLOSE = "CLOSE"
        EDIT = "EDIT"
        TEST = "TEST"
        END = "END"

    action: "ACIMessage.Action"
    status: str
    files: Optional[list[str]] = None
    scroll_position: Optional[int] = None
    active_file: Optional[str] = None
    new_contents: Optional[str] = None
    test_output: Optional[str] = None
    changed_range: Optional[tuple[int, int]] = None


class FileRPCListRequest(WSSerializeBase):
    action: Literal["LIST"] = "LIST"


class FileRPCListResponse(WSSerializeBase):
    action: Literal["LIST"] = "LIST"
    files: List[str]


class FileRPCReadRequest(WSSerializeBase):
    action: Literal["READ"] = "READ"
    path: str


class FileRPCReadResponse(WSSerializeBase):
    action: Literal["READ"] = "READ"
    content: Optional[str]


class FileRPCSearchRequest(WSSerializeBase):
    action: Literal["SEARCH"] = "SEARCH"
    query: str


class FileRPCSearchResponse(WSSerializeBase):
    action: Literal["SEARCH"] = "SEARCH"
    results: list[tuple[str, int, str]]


class WSMessage(WSSerializeBase):
    type: WSMessageType
    auth: Optional[AuthMessage] = None
    ping: Optional[None] = None
    chat: Optional[ChatMessage] = None
    pin: Optional[PinFileMessage] = None
    response_state: Optional[ResponseState] = Field(default=None, alias="responseState")
    run_command: Optional[RunCommandMessage] = None
    run_command_response: Optional[RunCommandResponse] = Field(
        default=None, alias="runCommandResponse"
    )
    file_rpc: Optional[
        Union[FileRPCListRequest, FileRPCReadRequest, FileRPCSearchRequest]
    ] = Field(discriminator="action", default=None)
    file_rpc_response: Optional[
        Union[FileRPCListResponse, FileRPCReadResponse, FileRPCSearchResponse]
    ] = Field(discriminator="action", default=None)
    aci: Optional[ACIMessage] = None
    usage: Optional[int] = None


class WebSocketSession:
    def __init__(self, websocket, user_id: int):
        self.websocket = websocket
        self.user_id = user_id
        self.properties: Dict[str, Any] = {}

    async def send(self, message):
        await self.websocket.send(message)


async def null_send_callback(msg: WSMessage):
    pass


async def null_recv_callback() -> WSMessage:
    raise NotImplementedError("null_recv_callback called")
