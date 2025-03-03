import enum
from typing import Literal, Union

from asimov.asimov_base import AsimovBase


class ScrollDownAction(AsimovBase):
    type: Literal["SCROLL_DOWN"] = "SCROLL_DOWN"
    scroll: int


class ScrollUpAction(AsimovBase):
    type: Literal["SCROLL_UP"] = "SCROLL_UP"
    scroll: int


class SystemAnalysisAction(AsimovBase):
    type: Literal["SYSTEM_ANALYSIS"] = "SYSTEM_ANALYSIS"
    system_analysis_output: str


class EditAction(AsimovBase):
    type: Literal["EDIT"] = "EDIT"
    lines_to_replace: str
    replace_text: str
    file: str


class SwitchAction(AsimovBase):
    type: Literal["SWITCH"] = "SWITCH"
    file: str


class StartAction(AsimovBase):
    type: Literal["START"] = "START"


class JumpAction(AsimovBase):
    type: Literal["JUMP"] = "JUMP"
    line: int


class TestAction(AsimovBase):
    type: Literal["TEST"] = "TEST"
    test_output: str


class CreateAction(AsimovBase):
    type: Literal["CREATE"] = "CREATE"
    content: str
    file: str


class OpenAction(AsimovBase):
    type: Literal["OPEN"] = "OPEN"
    content: str
    file: str


class FinalizeAction(AsimovBase):
    type: Literal["FINALIZE"] = "FINALIZE"


ACIAction = Union[
    ScrollDownAction,
    ScrollUpAction,
    SystemAnalysisAction,
    EditAction,
    SwitchAction,
    StartAction,
    JumpAction,
    TestAction,
    CreateAction,
    OpenAction,
    FinalizeAction,
]


class ACIMode(enum.Enum):
    CONSTRAINED = ("CONSTRAINED",)
