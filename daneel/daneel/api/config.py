from pathlib import Path
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    model_config = SettingsConfigDict(env_file=".env", env_file_encoding="utf-8")

    vcs_projects_dir: Path

    disable_auth: bool = True


def get_settings() -> Settings:
    s = Settings(_env_file=".env")
    s.vcs_projects_dir = s.vcs_projects_dir.expanduser().resolve()
    return s
