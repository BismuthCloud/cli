package org.bismuth.projects.services

import jakarta.enterprise.context.ApplicationScoped
import jakarta.inject.Inject
import jakarta.transaction.Transactional
import org.bismuth.auth.entities.OrganizationEntity
import org.bismuth.projects.VCSServiceClient
import org.bismuth.projects.entities.FeatureEntity
import org.bismuth.projects.entities.ProjectEntity
import org.eclipse.jgit.api.Git
import org.eclipse.jgit.transport.URIish
import org.jboss.logging.Logger

@ApplicationScoped
class ProjectService {
    private val LOG: Logger = Logger.getLogger(javaClass)

    @Transactional
    fun createProject(
        name: String,
        organization: OrganizationEntity,
        vcsServiceClient: VCSServiceClient,
        initializeRepo: Boolean = true
    ): ProjectEntity {
        val project = ProjectEntity()
        project.name = name
        project.organization = organization
        project.hash = vcsServiceClient.createProjectHash()
        project.cloneToken = (1..32)
            .map { (('a'..'z') + ('A'..'Z') + ('0'..'9')).random() }
            .joinToString("")

        vcsServiceClient.createRepo(project.hash)
        project.persist()

        if (initializeRepo) {
            val feature = FeatureEntity()
            feature.name = "main"
            feature.project = project
            vcsServiceClient.createBranch(project.hash, feature.name)
            feature.persist()
        }

        return project
    }
}
