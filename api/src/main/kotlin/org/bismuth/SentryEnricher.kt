package org.bismuth

import io.quarkus.arc.Arc
import io.quarkus.arc.Unremovable
import io.quarkus.security.identity.SecurityIdentity
import io.sentry.Hint
import io.sentry.SentryEvent
import io.sentry.SentryOptions.BeforeSendCallback
import io.sentry.protocol.User
import jakarta.enterprise.context.ApplicationScoped
import jakarta.inject.Inject
import org.jboss.logging.Logger

@ApplicationScoped
@Unremovable
class SentryEnricher : BeforeSendCallback {
    private val LOG: Logger = Logger.getLogger(javaClass)

    @Inject
    lateinit var securityIdentity: SecurityIdentity

    override fun execute(sentryEvent: SentryEvent, hint: Hint): SentryEvent? {
        try {
            if (Arc.container()?.requestContext()?.isActive == true) {
                val sentryUser = User()
                sentryUser.username = securityIdentity.principal.name
                sentryUser.email = securityIdentity.principal.name
                sentryEvent.user = sentryUser
            }
        } catch (e: Exception) {
            LOG.info("Exception while enriching sentry user: $e")
        }
        return sentryEvent
    }
}
