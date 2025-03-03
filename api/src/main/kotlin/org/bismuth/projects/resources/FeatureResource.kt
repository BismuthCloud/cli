package org.bismuth.projects.resources

import io.opentelemetry.instrumentation.annotations.SpanAttribute
import io.quarkus.security.identity.SecurityIdentity
import jakarta.enterprise.context.RequestScoped
import jakarta.inject.Inject
import jakarta.persistence.EntityManager
import jakarta.transaction.Transactional
import jakarta.ws.rs.*
import jakarta.ws.rs.core.MediaType
import jakarta.ws.rs.core.Response
import org.bismuth.auth.entities.OrganizationEntity
import org.bismuth.auth.entities.UserEntity
import org.bismuth.projects.entities.*
import org.bismuth.projects.services.DaneelClient
import org.eclipse.microprofile.rest.client.inject.RestClient
import org.jboss.logging.Logger

@RequestScoped
class FeatureResource {
    private val LOG: Logger = Logger.getLogger(javaClass)

    lateinit var organization: OrganizationEntity
    lateinit var project: ProjectEntity
    lateinit var feature: FeatureEntity

    @Inject lateinit var entityManager: EntityManager

    @Inject lateinit var securityIdentity: SecurityIdentity

    @GET
    @Path("/chat/sessions")
    @Produces(MediaType.APPLICATION_JSON)
    fun listChatSessions(): Response {
        val userId = UserEntity.findBy("username", securityIdentity.principal.name)!!.id!!
        val sessions =
                ChatSessionEntity.list(
                        "FROM ChatSessionEntity E WHERE E.feature.id = ?1 AND E.origin = ?2 ORDER BY E.updatedAt DESC",
                        feature.id!!,
                        "USER_CHAT:$userId"
                )

        return Response.ok(sessions).build()
    }

    data class ChatSessionCreateRequest(val name: String?)

    @POST
    @Transactional
    @Path("/chat/sessions")
    @Produces(MediaType.APPLICATION_JSON)
    @Consumes(MediaType.APPLICATION_JSON)
    fun createChatSessions(params: ChatSessionCreateRequest): Response {
        val userId = UserEntity.findBy("username", securityIdentity.principal.name)!!.id!!
        val session = ChatSessionEntity()
        session.feature = feature
        session.origin = "USER_CHAT:$userId"
        session.name = params.name
        session.persist()

        return Response.ok(session).build()
    }

    @PUT
    @Transactional
    @Path("/chat/sessions/{sessionId}")
    @Produces(MediaType.APPLICATION_JSON)
    @Consumes(MediaType.APPLICATION_JSON)
    fun updateChatSession(
            @PathParam("sessionId") @SpanAttribute("session") sessionId: Long,
            params: ChatSessionCreateRequest
    ): Response {
        if (params.name != null &&
                        ChatSessionEntity.find(
                                        "FROM ChatSessionEntity E WHERE E.feature.id = ?1 AND E.name = ?2 AND E.id != ?3",
                                        feature.id!!,
                                        params.name,
                                        sessionId
                                )
                                .firstResult() != null
        ) {
            return Response.status(Response.Status.CONFLICT)
                    .entity("A session with that name already exists")
                    .build()
        }
        val session =
                ChatSessionEntity.find(
                                "FROM ChatSessionEntity E WHERE E.feature.id = ?1 AND E.id = ?2",
                                feature.id!!,
                                sessionId
                        )
                        .firstResult()
                        ?: return Response.status(Response.Status.NOT_FOUND).build()
        session.name = params.name

        return Response.ok(session).build()
    }

    @DELETE
    @Transactional
    @Path("/chat/sessions/{sessionId}")
    @Consumes(MediaType.APPLICATION_JSON)
    fun deleteChatSession(
            @PathParam("sessionId") @SpanAttribute("session") sessionId: Long,
    ): Response {
        val session =
                ChatSessionEntity.find(
                                "FROM ChatSessionEntity E WHERE E.feature.id = ?1 AND E.id = ?2",
                                feature.id!!,
                                sessionId
                        )
                        .firstResult()
                        ?: return Response.status(Response.Status.NOT_FOUND).build()
        session.delete()
        return Response.ok().build()
    }

    @GET
    @Path("/chat/sessions/{sessionId}/list")
    @Produces(MediaType.APPLICATION_JSON)
    fun listChatMessages(
            @PathParam("sessionId") @SpanAttribute("session") sessionId: Long
    ): Response {
        val chatMessages =
                ChatMessageEntity.list(
                        "FROM ChatMessageEntity E WHERE E.session.feature.id = ?1 AND E.session.id = ?2 ORDER BY E.createdAt ASC",
                        feature.id!!,
                        sessionId,
                )
        chatMessages.forEach { m ->
            m.content = m.content.replace(Regex("\n<CURRENT_LOCATOR>(.*?)</CURRENT_LOCATOR>\n"), "")
        }

        return Response.ok(chatMessages).build()
    }

    data class FeedbackRequest(
            val messageId: Long,
            val upvote: Boolean?,
            val explanation: String?,
    )

    @GET
    @Path("")
    @Produces(MediaType.APPLICATION_JSON)
    fun get(): Response {
        return Response.ok(feature).build()
    }

    @RestClient
    lateinit var daneelClient: DaneelClient

    @DELETE
    @Path("")
    @Transactional
    fun delete(): Response {
        feature = entityManager.merge(feature)
        daneelClient.deleteCodegraph(feature.id!!)
        feature.delete()
        return Response.ok().build()
    }

    @POST
    @Consumes(MediaType.APPLICATION_JSON)
    @Path("/chat/feedback")
    @Transactional
    fun feedback(request: FeedbackRequest): Response {
        val chatMessage =
                ChatMessageEntity.find(
                                "FROM ChatMessageEntity E WHERE E.session.feature.id = ?1 AND E.id = ?2",
                                feature.id!!,
                                request.messageId,
                        )
                        .firstResult()
                        ?: return Response.status(Response.Status.NOT_FOUND).build()
        chatMessage.feedbackUpvote = request.upvote
        request.explanation?.let { chatMessage.feedback = it }
        return Response.ok().build()
    }

    data class GenerationAcceptedRequest(
            val messageId: Long,
            val accepted: Boolean,
    )

    @POST
    @Consumes(MediaType.APPLICATION_JSON)
    @Path("/chat/accepted")
    @Transactional
    fun generationAccepted(request: GenerationAcceptedRequest): Response {
        val chatMessage =
                ChatMessageEntity.find(
                                "FROM ChatMessageEntity E WHERE E.session.feature.id = ?1 AND E.id = ?2",
                                feature.id!!,
                                request.messageId,
                        )
                        .firstResult()
                        ?: return Response.status(Response.Status.NOT_FOUND).build()
        chatMessage.accepted = request.accepted
        return Response.ok().build()
    }
}
