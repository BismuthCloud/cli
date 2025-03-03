package org.bismuth.auth.resources

import com.fasterxml.jackson.annotation.JsonView
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import io.opentelemetry.instrumentation.annotations.SpanAttribute
import io.quarkus.oidc.AccessTokenCredential
import io.quarkus.security.credential.PasswordCredential
import io.quarkus.security.identity.SecurityIdentity
import io.smallrye.common.annotation.Blocking
import jakarta.inject.Inject
import jakarta.persistence.EntityManager
import jakarta.transaction.Transactional
import jakarta.validation.Valid
import jakarta.validation.constraints.Email
import jakarta.ws.rs.*
import jakarta.ws.rs.container.ResourceContext
import jakarta.ws.rs.core.Context
import jakarta.ws.rs.core.MediaType
import jakarta.ws.rs.core.Response
import org.bismuth.auth.entities.APIKeyEntity
import org.bismuth.auth.entities.OrganizationEntity
import org.bismuth.auth.entities.UserEntity
import org.bismuth.auth.entities.UserOrgViews
import org.bismuth.projects.entities.ChatMessageEntity
import org.bismuth.projects.entities.ChatSessionEntity
import org.bismuth.projects.entities.FeatureEntity
import org.bismuth.projects.entities.ProjectEntity
import org.bismuth.projects.resources.FeatureResource
import org.bismuth.projects.resources.ProjectResource
import org.hibernate.Hibernate
import org.jboss.logging.Logger

@Path("/organizations")
class OrganizationResource {
    private val LOG: Logger = Logger.getLogger(javaClass)

    @Context
    @Inject
    lateinit var resourceContext: ResourceContext;

    @Inject
    lateinit var projectResource: ProjectResource;

    @Inject
    lateinit var featureResource: FeatureResource;

    @Inject
    lateinit var securityIdentity: SecurityIdentity

    data class UpdateOrgRequest(val name: String)

    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Path("")
    @JsonView(UserOrgViews.OrganizationView::class)
    fun listOrganizations(
    ): Response {
        val user = getUser() ?: return Response.status(401).entity("Denied.").build()
        return Response.ok(user.organizations).build()
    }

    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{organizationId}")
    @JsonView(UserOrgViews.OrganizationView::class)
    fun getOrganization(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long
    ): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            Response.ok(org).build()
        }
    }

    @POST
    @Consumes(MediaType.APPLICATION_JSON)
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{organizationId}")
    @Transactional
    @JsonView(UserOrgViews.OrganizationView::class)
    fun updateOrg(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        request: UpdateOrgRequest
    ): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            org.name = request.name
            Hibernate.initialize(org.users)
            Response.ok(org).build()
        }
    }

    @GET
    @Path("/{organizationId}/llm-configuration")
    fun getLLMConfiguration(@PathParam("organizationId") @SpanAttribute("organization") orgId: Long): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            Response.ok(org.llmConfig).build()
        }
    }

    @POST
    @Path("/{organizationId}/llm-configuration")
    @Transactional
    fun updateLLMConfiguration(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        configuration: Map<String, String>
    ): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            org.llmConfig = configuration
            Response.ok().build()
        }
    }

    @Path("/{organizationId}/projects")
    fun projects(@PathParam("organizationId") @SpanAttribute("organization") orgId: Long): Any {
        return validateInOrg(orgId) {
            val projectResource = resourceContext.initResource(projectResource)
            projectResource.organization = OrganizationEntity.findById(orgId)!!
            projectResource
        }
    }

    @Inject
    lateinit var entityManager: EntityManager

    data class UsageSummary(
        val projectUsage: Map<String, Map<String, Long>>
    )

    data class UsageRowDTO(
        val projectName: java.lang.String,
        val item: java.lang.String?,
        val usage: java.lang.Long,
    )

    data class GenerationDTO(
        val message: ChatMessageEntity,
        val session: ChatSessionEntity,
        val project: ProjectEntity,
        val states: Array<java.lang.String?>,
        val isActive: java.lang.Boolean,
    )

    data class GenerationOrigin(
        val text: String,
        val link: String?,
    )
    data class GenerationInfo(
        val active: Boolean,
        val statuses: List<String>,
        val message: ChatMessageEntity,
        val session: ChatSessionEntity,
        val origin: GenerationOrigin,
        val project: ProjectEntity,
    )

    @GET
    @Path("/{organizationId}/generations")
    @Produces(MediaType.APPLICATION_JSON)
    fun getGenerations(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long
    ): Any {
        return validateInOrg(orgId) {
            // Get all chat messages and their latest generation traces for the org
            val query = """
                SELECT 
                    cm as message,
                    cs as session,
                    p as project,
                    array_agg((
                        SELECT cast(gte.state as text)
                    )) WITHIN GROUP (order by gte.createdAt) as states,
                    (
                        SELECT COUNT(cm2) = 0
                        FROM ChatMessageEntity cm2
                        WHERE cm2.session = cm.session
                        AND cm2.createdAt > cm.createdAt
                    ) as isActive
                FROM GenerationTraceEntity gte
                JOIN gte.chatMessage cm
                JOIN cm.session cs
                JOIN cs.feature f
                JOIN f.project p
                WHERE p.organization.id = :orgId
                AND cm.isAI = false
                GROUP BY cm, session, project
            """.trimIndent()

            val results = entityManager.createQuery(query, GenerationDTO::class.java)
                .setParameter("orgId", orgId)
                .resultList
                .map { row ->
                    var statuses = row.states.filterNotNull().mapNotNull {
                        val state = jacksonObjectMapper().readTree(it.toString())
                        when (state["type"].asText()) {
                            "RESPONSE_STATE" -> state["responseState"]["state"].asText()
                            "ACI" -> if (state["aci"]["status"] != null) {
                                state["aci"]["status"].asText()
                            } else {
                                null
                            }
                            "CHAT" -> {
                                val msg = jacksonObjectMapper().readTree(state["chat"]["message"].asText())
                                if (msg["done"] != null) {
                                    "Finished"
                                } else {
                                    null
                                }
                            }
                            else -> null
                        }
                    }
                    if (statuses.isEmpty()) {
                        statuses = listOf("Starting...")
                    }
                    statuses = statuses.zipWithNext().filter{ it.first != it.second }.map{ it.first } + statuses.last()

                    val origin = when (row.session.origin.split(':')[0]) {
                        "USER_CHAT" -> {
                            val name = row.session.name ?: "session-${row.session.id}"
                            GenerationOrigin("Interactive Chat ($name)", "#${row.project.id}/${row.session.feature.id}/${row.session.id}")
                        }
                        else -> GenerationOrigin(row.session.origin, null)
                    }

                    GenerationInfo(
                        active = row.isActive.booleanValue(),
                        statuses = statuses,
                        message = row.message,
                        session = row.session,
                        origin = origin,
                        project = row.project,
                    )
                }

            Response.ok(results).build()
        }
    }

    @GET
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{organizationId}/usage")
    fun usage(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long
    ): Any {
        return validateInOrg(orgId) {
            val organization = OrganizationEntity.findById(orgId)!!
            val rows = entityManager.createQuery("SELECT project.name AS projectName, usage.item AS item, COALESCE(SUM(usage.usage), 0) AS usage FROM ProjectEntity project INNER JOIN FeatureEntity feature ON feature.project = project LEFT JOIN HourlyUsage usage ON usage.feature = feature WHERE project.organization.id = :orgId GROUP BY projectName, item", UsageRowDTO::class.java)
                .setParameter("orgId", orgId)
                .resultList
            val summary = UsageSummary(rows.groupBy { it.projectName.toString() }.mapValues { (_, items) -> items.filter {it.item != null}.associate { Pair(it.item.toString(), it.usage.toLong()) } })
            Response.ok(summary).build()
        }
    }

    data class AddMemberRequest(
        @Email
        val email: String
    )

    @POST
    @Consumes(MediaType.APPLICATION_JSON)
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{organizationId}/members")
    @Transactional
    @JsonView(UserOrgViews.OrganizationView::class)
    fun addMember(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        @Valid request: AddMemberRequest
    ): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            if (org.users.any { it.email == request.email }) {
                return@validateInOrg Response.status(Response.Status.CONFLICT).build()
            }
            var user = UserEntity.findBy("email", request.email)
            if (user == null) {
                user = UserEntity()
                user.name = ""
                user.email = request.email
                user.username = request.email
                user.pending = true
                user.persist()
            }

            org.users.add(user)
            Response.ok(org.users).build()
        }
    }

    @DELETE
    @Consumes(MediaType.APPLICATION_JSON)
    @Produces(MediaType.APPLICATION_JSON)
    @Path("/{organizationId}/members/{userId}")
    @Transactional
    @JsonView(UserOrgViews.OrganizationView::class)
    fun removeMember(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        @PathParam("userId") userId: Long
    ): Any {
        return validateInOrg(orgId) {
            val org = OrganizationEntity.findById(orgId)!!
            if (org.users.count() == 1) {
                return@validateInOrg Response.ok().build()
            }
            org.users.remove(UserEntity.findById(userId)!!)
            Response.ok(org.users).build()
        }
    }

    data class FeatureCreateParams (
        val name: String,
    )

    @Path("/{organizationId}/projects/{projectId}/features")
    @POST
    @Transactional
    fun createFeature(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        @PathParam("projectId") @SpanAttribute("project") projectId: Long,
        params: FeatureCreateParams
    ): Any {
        return validateInOrg(orgId) {
            val feature = FeatureEntity()
            feature.project = ProjectEntity.findById(projectId)!!
            feature.name = params.name
            feature.persist()
            Response.ok(feature).build()
        }
    }

    // TODO: featureId is pulled out at this level instead of being a prefix in FeatureResource
    // so we only have to do this auth check and load once.
    // Hibernate supports "proper" multi-tenancy with `@TenantId` and discriminator config
    // which we should probably use as well, but need to ensure that a tenantid on the outer org
    // will be automatically joined into all from a feature.
    @Path("/{organizationId}/projects/{projectId}/features/{featureId}")
    fun features(
        @PathParam("organizationId") @SpanAttribute("organization") orgId: Long,
        @PathParam("projectId") @SpanAttribute("project") projectId: Long,
        @PathParam("featureId") @SpanAttribute("feature") featureId: Long
    ): Any {
        return validateInOrg(orgId) {
            val featureResource = resourceContext.initResource(featureResource)

            val feature =
                FeatureEntity.find(
                    "FROM FeatureEntity E LEFT JOIN FETCH E.project LEFT JOIN FETCH E.project.organization WHERE E.id = ?1 AND E.project.id = ?2 AND E.project.organization.id = ?3",
                    featureId,
                    projectId,
                    orgId
                ).firstResult()
                    ?: return@validateInOrg AuthDenialResource()
            featureResource.feature = feature
            featureResource.project = feature.project
            featureResource.organization = feature.project.organization

            featureResource
        }
    }

    @Blocking
    fun validateInOrg(orgId: Long, closure: () -> Any): Any {
        val user = getUser() ?: return AuthDenialResource()
        return if (user.organizations.any { it.id == orgId }) {
            closure.invoke()
        } else {
            AuthDenialResource()
        }
    }

    fun getUser(): UserEntity? {
        securityIdentity.getCredential(AccessTokenCredential::class.java)?.let {
            return@getUser UserEntity.find("FROM UserEntity E WHERE E.username = ?1", securityIdentity.principal.name).firstResult()
        }
        securityIdentity.getCredential(PasswordCredential::class.java)?.let {
            val key = APIKeyEntity.find("FROM APIKeyEntity E JOIN FETCH E.user WHERE E.token = ?1", it.password).firstResult() ?: return null
            return@getUser key.user
        }
        return null
    }
}