package org.bismuth.auth.resources

import com.fasterxml.jackson.annotation.JsonView
import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import io.quarkus.oidc.UserInfo
import io.quarkus.oidc.runtime.OidcAuthenticationMechanism
import io.quarkus.security.Authenticated
import io.quarkus.security.AuthenticationFailedException
import io.quarkus.security.credential.PasswordCredential
import io.quarkus.security.identity.AuthenticationRequestContext
import io.quarkus.security.identity.IdentityProvider
import io.quarkus.security.identity.IdentityProviderManager
import io.quarkus.security.identity.SecurityIdentity
import io.quarkus.security.identity.request.AuthenticationRequest
import io.quarkus.security.identity.request.UsernamePasswordAuthenticationRequest
import io.quarkus.security.runtime.QuarkusPrincipal
import io.quarkus.security.runtime.QuarkusSecurityIdentity
import io.quarkus.vertx.http.runtime.security.BasicAuthenticationMechanism
import io.quarkus.vertx.http.runtime.security.ChallengeData
import io.quarkus.vertx.http.runtime.security.HttpAuthenticationMechanism
import io.quarkus.vertx.http.runtime.security.HttpCredentialTransport
import io.smallrye.mutiny.Uni
import io.vertx.ext.web.RoutingContext
import jakarta.annotation.Priority
import jakarta.enterprise.context.ApplicationScoped
import jakarta.enterprise.inject.Alternative
import jakarta.inject.Inject
import jakarta.json.JsonValue
import jakarta.transaction.Transactional
import jakarta.transaction.UserTransaction
import jakarta.ws.rs.*
import jakarta.ws.rs.core.Response
import org.bismuth.auth.entities.*
import org.eclipse.microprofile.config.ConfigProvider
import org.eclipse.microprofile.config.inject.ConfigProperty
import org.hibernate.Hibernate
import org.jboss.logging.Logger
import org.keycloak.admin.client.Keycloak
import org.quartz.*
import java.net.URI
import java.time.OffsetDateTime
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

@Path("/auth")
class AuthResource {
    @Inject
    lateinit var userInfo: UserInfo

    @Inject
    lateinit var securityIdentity: SecurityIdentity

    @ConfigProperty(name = "quarkus.oidc.auth-server-url")
    lateinit var OIDCUrl: String

    @ConfigProperty(name = "oidc-public-url", defaultValue = " ")
    lateinit var OIDCUrlOverride: String

    @Path("/cli")
    @GET
    fun cliLoginRedirect(@QueryParam("port") port: Int) : Response {
        val oidcUrl = if (OIDCUrlOverride != " ") {
            OIDCUrlOverride
        } else {
            OIDCUrl
        }
        return Response.temporaryRedirect(URI("$oidcUrl/protocol/openid-connect/auth?client_id=cli&redirect_uri=http://localhost:$port/&scope=openid&response_type=code&response_mode=query&prompt=login")).build()
    }

    @Path("/me")
    @GET
    @Authenticated
    @Transactional
    @JsonView(UserOrgViews.UserView::class)
    fun me(): Response {
        val registeredUser = if (securityIdentity.credentials.any { it is PasswordCredential }) {
            UserEntity.findBy("username", securityIdentity.principal.name)!!
        } else {
            ensureRegistered()
        }
        Hibernate.initialize(registeredUser.organizations)
        return Response.ok(registeredUser).build()
    }

    @Path("/apikey")
    @GET
    @Authenticated
    @Transactional
    fun listApiKeys(): Response {
        val registeredUser = ensureRegistered()
        return Response.ok(APIKeyEntity.list("FROM APIKeyEntity E WHERE E.user = ?1", registeredUser)).build()
    }

    data class APIKeyRequest(val description: String?)

    @Path("/apikey")
    @POST
    @Authenticated
    @Transactional
    fun apiKey(request: APIKeyRequest): Response {
        val registeredUser = ensureRegistered()
        val token = APIKeyEntity()
        token.user = registeredUser
        token.description = request.description ?: "Bismuth CLI ${OffsetDateTime.now(ZoneOffset.UTC).format(DateTimeFormatter.ISO_DATE_TIME)}"
        token.token = "BIS1-" + (1..32)
            .map { (('a'..'z') + ('A'..'Z') + ('0'..'9')).random() }
            .joinToString("")
        token.persist()
        return Response.ok(token.token).build()
    }

    @Path("/apikey/{id}")
    @DELETE
    @Authenticated
    @Transactional
    fun deleteApiKey(@PathParam("id") id: Long): Response {
        val registeredUser = ensureRegistered()
        val token = APIKeyEntity.findById(id) ?: return Response.status(404).build()
        if (token.user != registeredUser) {
            return Response.status(403).build()
        }
        token.delete()
        return Response.ok().build()
    }

    @Inject
    lateinit var keycloak: Keycloak

    fun ensureRegistered(): UserEntity {
        var registeredUser = UserEntity.findBy("email", userInfo.email)

        if (registeredUser == null) {
            registeredUser = UserEntity()
            registeredUser.email = userInfo.email
            registeredUser.username = registeredUser.email
            registeredUser.name = userInfo.name ?: userInfo.preferredUserName ?: registeredUser.email.split("@").first()

            val organization = OrganizationEntity()
            organization.name = registeredUser.name + "'s Organization"
            organization.subscription = SubscriptionEntity()
            organization.subscription.type = SubscriptionType.INDIVIDUAL
            organization.subscription.credits = 100
            organization.subscription.persist()
            organization.persist()

            registeredUser.organizations.add(organization)
            registeredUser.persist()

            if (userInfo.allProperties.associateBy { it.key }["email_verified"]!!.value != JsonValue.TRUE) {
                keycloak.realm("bismuth").users().get(userInfo.subject).sendVerifyEmail()
            }
        } else if (registeredUser.pending) {
            registeredUser.pending = false
            registeredUser.username = registeredUser.email
            registeredUser.name = userInfo.name ?: userInfo.preferredUserName ?: registeredUser.email.split("@").first()

            if (userInfo.allProperties.associateBy { it.key }["email_verified"]!!.value != JsonValue.TRUE) {
                keycloak.realm("bismuth").users().get(userInfo.subject).sendVerifyEmail()
            }
        }

        return registeredUser
    }
}

@ApplicationScoped
class DummyAuthenticationMechanism : HttpAuthenticationMechanism {
    override fun authenticate(
        context: RoutingContext,
        identityProviderManager: IdentityProviderManager
    ): Uni<SecurityIdentity> {
        return Uni.createFrom().nullItem()
    }

    override fun getChallenge(context: RoutingContext): Uni<ChallengeData> {
        return Uni.createFrom().nullItem()
    }

    override fun getCredentialTypes(): Set<Class<out AuthenticationRequest>> {
        return emptySet()
    }

    override fun getCredentialTransport(context: RoutingContext): Uni<HttpCredentialTransport> {
        return Uni.createFrom().nullItem()
    }
}

@Alternative
@Priority(1)
@ApplicationScoped
class CustomAuthMechanism : HttpAuthenticationMechanism {
    @Inject
    lateinit var oidc: OidcAuthenticationMechanism;

    @Inject
    lateinit var basic: BasicAuthenticationMechanism

    @Inject
    lateinit var dummy: DummyAuthenticationMechanism

    override fun authenticate(
        context: RoutingContext,
        identityProviderManager: IdentityProviderManager
    ): Uni<SecurityIdentity> {
        val auth = selectBetweenBasicAndOIDC(context)
        return auth.authenticate(context, identityProviderManager)
    }

    override fun getChallenge(context: RoutingContext): Uni<ChallengeData> {
        return selectBetweenBasicAndOIDCChallenge(context).getChallenge(context)
    }

    override fun getCredentialTypes(): Set<Class<out AuthenticationRequest>> {
        val credentialTypes: MutableSet<Class<out AuthenticationRequest>> =
            HashSet()
        credentialTypes.addAll(oidc.credentialTypes)
        credentialTypes.addAll(basic.credentialTypes)
        return credentialTypes
    }

    override fun getCredentialTransport(context: RoutingContext): Uni<HttpCredentialTransport> {
        return selectBetweenBasicAndOIDC(context).getCredentialTransport(context)
    }

    private fun selectBetweenBasicAndOIDC(context: RoutingContext): HttpAuthenticationMechanism {
        if (context.normalizedPath().startsWith("/git/")
            || context.normalizedPath().startsWith("/webhooks/")
            || context.normalizedPath().startsWith("/integrations/")
            ) {
            return dummy
        }
        return if (context.request().headers().contains("Authorization") && context.request().headers().get("Authorization").startsWith("Basic ")) {
            basic
        } else {
            oidc
        }
    }

    private fun selectBetweenBasicAndOIDCChallenge(context: RoutingContext): HttpAuthenticationMechanism {
        return selectBetweenBasicAndOIDC(context)
    }
}

@ApplicationScoped
class APIKeyIdentityProvider : IdentityProvider<UsernamePasswordAuthenticationRequest> {
    override fun getRequestType(): Class<UsernamePasswordAuthenticationRequest> {
        return UsernamePasswordAuthenticationRequest::class.java
    }

    @Transactional
    fun authBlocking(request: UsernamePasswordAuthenticationRequest): SecurityIdentity {
        val key = APIKeyEntity.findBy("token", request.password.password) ?: throw AuthenticationFailedException("")
        return QuarkusSecurityIdentity.builder()
            .setPrincipal(QuarkusPrincipal(key.user.username))
            .addCredential(request.password)
            .setAnonymous(false)
            .build()
    }

    override fun authenticate(
        request: UsernamePasswordAuthenticationRequest,
        context: AuthenticationRequestContext
    ): Uni<SecurityIdentity> {
        return context.runBlocking {
            authBlocking(request)
        }
    }
}
