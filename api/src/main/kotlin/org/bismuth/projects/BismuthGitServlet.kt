package org.bismuth.projects

import com.fasterxml.jackson.module.kotlin.jacksonObjectMapper
import io.quarkus.narayana.jta.QuarkusTransaction
import io.smallrye.mutiny.coroutines.asFlow
import jakarta.enterprise.inject.spi.CDI
import jakarta.persistence.EntityManager
import jakarta.servlet.http.HttpServletRequest
import kotlinx.coroutines.runBlocking
import org.bismuth.projects.entities.FeatureEntity
import org.bismuth.projects.entities.ProjectEntity
import org.bismuth.projects.services.DaneelClient
import org.bismuth.projects.services.IngestProgressEvent
import org.bismuth.projects.services.IngestProgressStatus
import org.eclipse.jgit.http.server.resolver.AsIsFileService
import org.eclipse.jgit.http.server.resolver.DefaultReceivePackFactory
import org.eclipse.jgit.lib.Repository
import org.eclipse.jgit.transport.PostReceiveHook
import org.eclipse.jgit.transport.ReceiveCommand
import org.eclipse.jgit.transport.ReceivePack
import org.eclipse.jgit.transport.resolver.FileResolver
import org.eclipse.microprofile.config.ConfigProvider
import org.eclipse.microprofile.rest.client.RestClientBuilder
import org.jboss.logging.Logger
import java.io.File
import java.net.URI
import java.util.*

class BismuthGitAuthFilter() : jakarta.servlet.Filter {
    override fun doFilter(
        request: jakarta.servlet.ServletRequest,
        response: jakarta.servlet.ServletResponse,
        chain: jakarta.servlet.FilterChain
    ) {
        val httpRequest = request as HttpServletRequest
        val httpResponse = response as jakarta.servlet.http.HttpServletResponse

        val authHeader = httpRequest.getHeader("Authorization")
        if (authHeader == null || !authHeader.startsWith("Basic ")) {
            httpResponse.status = jakarta.servlet.http.HttpServletResponse.SC_UNAUTHORIZED
            httpResponse.setHeader("WWW-Authenticate", "Basic realm=\"Bismuth Git\"")
            return
        }

        val project = ProjectEntity.find(
            "FROM ProjectEntity E WHERE E.hash = ?1",
            httpRequest.pathInfo.split('/')[1].removeSuffix(".git")
        ).firstResult()
        if (project == null) {
            httpResponse.status = jakarta.servlet.http.HttpServletResponse.SC_FORBIDDEN
            return
        }

        val userPass =
            Base64.getDecoder().decode(authHeader.removePrefix("Basic "))
        if (userPass.toString(Charsets.UTF_8).split(':')[1] != project.cloneToken) {
            httpResponse.status = jakarta.servlet.http.HttpServletResponse.SC_FORBIDDEN
            return
        }
        chain.doFilter(request, response)
    }
}

class BismuthPostReceiveHook(private val project: ProjectEntity) : PostReceiveHook {
    val LOG: Logger = Logger.getLogger(javaClass)

    override fun onPostReceive(rp: ReceivePack, commands: MutableCollection<ReceiveCommand>) {
        val daneelClient = RestClientBuilder.newBuilder().baseUri(
            URI(
                ConfigProvider.getConfig()
                    .getValue("quarkus.rest-client.daneel.uri", String::class.java)
            )
        ).build(DaneelClient::class.java)
        val cdi = CDI.current()

        var cgFeatureId: Long? = null

        QuarkusTransaction.requiringNew().run {
            val entityManager = cdi.select(EntityManager::class.java).get()

            rp.messageOutputStream.write(
                "Project: ${project.name}\n".toByteArray()
            )
            for (command in commands) {
                val project = entityManager.find(ProjectEntity::class.java, project.id)
                project.hasPushed = true

                when (command.type) {
                    ReceiveCommand.Type.CREATE -> {
                        val feature = FeatureEntity()
                        feature.project = project
                        feature.name = command.refName.removePrefix("refs/heads/")
                        feature.persist()

                        cgFeatureId = feature.id!!

                        rp.messageOutputStream.write(
                            "Created: ${feature.name}\n".toByteArray()
                        )
                    }

                    ReceiveCommand.Type.DELETE -> {
                        val feature = FeatureEntity.find(
                            "FROM FeatureEntity E WHERE E.project.id = ?1 AND E.name = ?2",
                            project.id!!,
                            command.refName.removePrefix("refs/heads/")
                        ).singleResult()
                        feature.delete()
                        // FileEntities and ScheduledJobEntities are cascade deleted
                        rp.messageOutputStream.write(
                            "Deleted: ${feature.name}\n".toByteArray()
                        )
                    }

                    ReceiveCommand.Type.UPDATE, ReceiveCommand.Type.UPDATE_NONFASTFORWARD -> {
                        val feature = FeatureEntity.find(
                            "FROM FeatureEntity E WHERE E.project.id = ?1 AND E.name = ?2",
                            project.id!!,
                            command.refName.removePrefix("refs/heads/")
                        ).singleResult()
                        cgFeatureId = feature.id!!
                        rp.messageOutputStream.write(
                            "Updated: ${command.refName}\n".toByteArray()
                        )
                    }
                }
            }
        }
        rp.messageOutputStream.flush()
        cgFeatureId?.let{
            try {
                runBlocking {
                    val spinner = listOf("|", "/", "-", "\\")
                    var i = 0
                    daneelClient.generateCodegraphStream(it).asFlow().collect { event ->
                        if (event.isEmpty()) {
                            return@collect
                        }
                        val progress = jacksonObjectMapper().readValue(event, IngestProgressEvent::class.java)
                        val msg = when (progress.status) {
                            IngestProgressStatus.IN_PROGRESS -> {
                                if (progress.progress == null) {
                                    i += 1
                                    "${progress.step}... ${spinner[i % spinner.size]}\r"
                                } else {
                                    "${progress.step}... ${(progress.progress * 100).toInt()}%\r"
                                }
                            }
                            IngestProgressStatus.COMPLETED -> {
                                "${progress.step}... Done\n"
                            }
                            IngestProgressStatus.ERROR -> {
                                "${progress.step}... Error :(\n"
                            }
                        }
                        rp.messageOutputStream.write(
                            msg.toByteArray()
                        )
                        rp.messageOutputStream.flush()
                    }
                }
            } catch (e: Exception) {
                LOG.info("exception calling into codegraph gen: $e")
            }
        }
    }
}

class ReceivePackFactory : DefaultReceivePackFactory() {
    override fun create(req: HttpServletRequest?, db: Repository?): ReceivePack {
        // TODO: remove (transitional for old repos)
        db?.config?.setBoolean("http", null, "receivepack", true)
        db?.config?.save()

        val pack = super.create(req, db)
        val project = ProjectEntity.find(
            "FROM ProjectEntity E WHERE E.hash = ?1",
            req!!.pathInfo.split('/')[1]
        ).singleResult()
        pack.postReceiveHook = BismuthPostReceiveHook(project)
        return pack
    }
}

class BismuthGitServlet() : org.eclipse.jgit.http.server.GitServlet() {
    init {
        var projectsDir = ConfigProvider.getConfig().getValue("vcs.projects-dir", String::class.java)
        projectsDir = projectsDir.replaceFirst("^~".toRegex(), System.getProperty("user.home"))
        setRepositoryResolver(FileResolver(File(projectsDir), true))
        setAsIsFileService(AsIsFileService.DISABLED)
        setReceivePackFactory(ReceivePackFactory())
    }
}
