package org.bismuth.projects.resources

import io.quarkus.security.identity.SecurityIdentity
import io.smallrye.mutiny.Uni
import jakarta.enterprise.context.RequestScoped
import jakarta.inject.Inject
import jakarta.transaction.Transactional
import jakarta.ws.rs.*
import jakarta.ws.rs.core.MediaType
import jakarta.ws.rs.core.Response
import org.bismuth.auth.entities.OrganizationEntity
import org.bismuth.projects.VCSServiceClient
import org.bismuth.projects.entities.ProjectEntity
import org.bismuth.projects.services.DaneelClient
import org.bismuth.projects.services.ProjectService
import org.eclipse.microprofile.rest.client.inject.RestClient
import org.jboss.logging.Logger
import java.time.Duration

@RequestScoped
class ProjectResource() {
    private val LOG: Logger = Logger.getLogger(javaClass)

    lateinit var organization: OrganizationEntity

    @Inject
    private lateinit var vcsServiceClient: VCSServiceClient

    @Inject
    private lateinit var projectService: ProjectService

    @Inject
    lateinit var securityIdentity: SecurityIdentity

    data class CreateProjectParams(
        val name: String?,
        val installationId: Long?,
        val repo: String?,
    )

    @POST
    @Produces(MediaType.APPLICATION_JSON)
    @Consumes(MediaType.APPLICATION_JSON)
    @Path("")
    @Transactional
    fun create(params: CreateProjectParams): Response {
        if (params.name != null) {
            if (ProjectEntity.count("FROM ProjectEntity E WHERE E.organization = ?1 AND E.name = ?2", organization, params.name) > 0) {
                return Response.status(Response.Status.CONFLICT).entity("A project with that name already exists").build()
            }
            val project = projectService.createProject(params.name, organization, vcsServiceClient)
            return Response.status(Response.Status.CREATED).entity(project).build()
        } else {
            return Response.status(Response.Status.BAD_REQUEST).build()
        }
    }

    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{id}")
    fun get(@PathParam("id") id: Long): Response {
        val project: ProjectEntity =
            ProjectEntity.find(
                "FROM ProjectEntity E WHERE E.id = ?1 AND E.organization.id = ?2",
                id,
                organization.id!!
            ).firstResult()
                ?: return Response.status(Response.Status.NOT_FOUND).build()
        return Response.ok(project).build()
    }

    @RestClient
    lateinit var daneelClient: DaneelClient

    @DELETE
    @Transactional
    @Path("/{id}")
    fun delete(@PathParam("id") id: Long): Uni<Response> {
        val project: ProjectEntity =
            ProjectEntity.find(
                "FROM ProjectEntity E WHERE E.id = ?1 AND E.organization.id = ?2",
                id,
                organization.id!!
            ).firstResult()
                ?: return Uni.createFrom().item { Response.status(Response.Status.NOT_FOUND).build() }

        for (feature in project.features) {
            daneelClient.deleteCodegraph(feature.id!!)
        }

        project.delete()

        return Uni.createFrom().item {
            vcsServiceClient.deleteRepo(project.hash)
        }.onFailure().retry().withBackOff(Duration.ofMillis(200)).atMost(5).onFailure().recoverWithItem {
            error ->
                LOG.error("Failed to delete repo", error)
        }.map {
            Response.ok().build()
        }
    }

    data class ListProjectsResponse(val projects: List<ProjectEntity>)

    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/list")
    fun list(): Response {
        val projects = ProjectEntity.list("FROM ProjectEntity E WHERE E.organization.id = ?1", organization.id!!)
        return Response.ok().entity(ListProjectsResponse(projects)).build()
    }
}