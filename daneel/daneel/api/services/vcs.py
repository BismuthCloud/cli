import os
import pathlib
import random
import hashlib
from datetime import datetime
from typing import Optional, List, Dict, Any
from git import Repo, GitCommandError, Actor
from daneel.api.config import get_settings


class VCSServiceClient:
    def __init__(self):
        settings = get_settings()
        self.projects_dir = pathlib.Path(settings.vcs_projects_dir)
        self.COMMIT_AUTHOR = Actor("Bismuth", "committer@app.bismuth.cloud")

    def get_repo_path(self, project_hash: str) -> pathlib.Path:
        return self.projects_dir / project_hash

    def get_repo(self, project_hash: str) -> Repo:
        return Repo(self.get_repo_path(project_hash))

    def create_repo(self, project_hash: str):
        repo_path = self.get_repo_path(project_hash)
        repo = Repo.init(repo_path, initial_branch="main", bare=True)

        # Configure receivepack
        with repo.config_writer() as git_config:
            git_config.set_value("http", "receivepack", "true")

    def create_branch(self, project_hash: str, branch_name: str):
        if branch_name == "main":
            print("createBranch Output: Main branch skipping.")
            return

        repo = self.get_repo(project_hash)
        repo.create_head(branch_name, "main")

    def delete_repo(self, project_hash: str):
        import shutil

        repo_path = self.get_repo_path(project_hash)
        if repo_path.exists():
            shutil.rmtree(repo_path)

    def create_project_hash(self) -> str:
        bytes_data = random.randbytes(16)
        return hashlib.sha256(bytes_data).hexdigest()
