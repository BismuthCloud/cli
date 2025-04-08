from typing import Optional
from urllib.parse import urlparse
from daneel.data.postgres.models import (
    FeatureEntity,
    ProjectEntity,
    OrganizationEntity,
)
from daneel.api.services.vcs import VCSServiceClient


class ProjectService:
    def __init__(self):
        self.vcs_service = VCSServiceClient()

    def create_project(
        self,
        name: str,
        organization: OrganizationEntity,
        vcs_service: VCSServiceClient,
        initialize_repo: bool = True,
    ) -> ProjectEntity:
        project = ProjectEntity(
            name=name,
            organization_id=organization.id,
            hash=vcs_service.create_project_hash(),
            clone_token=self._generate_clone_token(),
        )

        vcs_service.create_repo(project.hash)
        project.persist()

        if initialize_repo:
            feature = FeatureEntity(name="main", project_id=project.id)
            vcs_service.create_branch(project.hash, feature.name)
            feature.persist()

        return project

    def _generate_clone_token(self) -> str:
        import random
        import string

        return "".join(random.choices(string.ascii_letters + string.digits, k=32))
