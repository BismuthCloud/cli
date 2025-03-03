from pydantic import BaseModel


class TOMLBaseModel(BaseModel):
    model_config = {"extra": "ignore"}


class BismuthChatTOML(TOMLBaseModel):
    command_timeout: int = 60
    additional_files: list[str] = [".env", ".env.local", ".env.development"]
    block_globs: list[str] = [
        "**/.*/**",
        "venv/**",
        "**/__pycache__/**",
        "*.pyc",
        "**/node_modules/**",
        "**/target/**",
        "**/dist/**",
        "**/build/**",
        ".git/*",
        ".git/**/*",
    ]


class BismuthTOML(TOMLBaseModel):
    chat: BismuthChatTOML = BismuthChatTOML()
