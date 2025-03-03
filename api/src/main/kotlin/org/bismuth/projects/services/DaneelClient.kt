package org.bismuth.projects.services

import io.smallrye.mutiny.Multi
import jakarta.ws.rs.*
import jakarta.ws.rs.core.MediaType
import jakarta.ws.rs.core.Response
import org.eclipse.microprofile.rest.client.inject.RegisterRestClient

enum class IngestProgressStatus{
    IN_PROGRESS,
    COMPLETED,
    ERROR
}
class IngestProgressEvent(
    val step: String,
    val status: IngestProgressStatus,
    val progress: Float?,
)

@RegisterRestClient(configKey = "daneel")
interface DaneelClient {
    @POST
    @Path("/api/codegraph")
    @Produces(MediaType.SERVER_SENT_EVENTS)
    // idk why but returning a Multi<IngestProgressEvent> fails to deserialize but manually doing it works
    fun generateCodegraphStream(@QueryParam("feature_id") featureId: Long): Multi<String>

    @POST
    @Path("/api/codegraph")
    @Produces(MediaType.SERVER_SENT_EVENTS)
    fun generateCodegraph(@QueryParam("feature_id") featureId: Long): String

    @DELETE
    @Path("/api/codegraph/{feature_id}")
    fun deleteCodegraph(@PathParam("feature_id") featureId: Long)

    @POST
    @Path("/api/bug_ci")
    fun bugCICallback(
        @QueryParam("feature_id") featureId: Long,
        @QueryParam("pr_num") prNum: Long,
    )

    @POST
    @Path("/api/bug_full_scan")
    fun bugFullScanTrigger(
        @QueryParam("feature_id") featureId: Long,
    )

    @POST
    @Path("/api/process/github")
    fun processGithub(
        @QueryParam("feature_id") featureId: Long,
        @QueryParam("issue_id") issueId: String,
        @QueryParam("user_id") userId: Long,
        @QueryParam("message") message: String,
        @QueryParam("jira_api_base") jiraApiBase: String? = null,
        @QueryParam("jira_token") jiraToken: String? = null,
    )

    @POST
    @Path("/api/process/bitbucket")
    fun processBitbucket(
        @QueryParam("feature_id") featureId: Long,
        @QueryParam("issue_id") issueId: String,
        @QueryParam("user_id") userId: Long,
        @QueryParam("message") message: String,
        @QueryParam("jira_api_base") jiraApiBase: String? = null,
        @QueryParam("jira_token") jiraToken: String? = null,
    )

    @GET
    @Path("/api/public/v1/search")
    fun v1Search(
        @QueryParam("feature_id") featureId: Long,
        @QueryParam("query") query: String,
        @QueryParam("top") top: Int,
    ): Response

    data class V1Location(
        val file: String,
        val line: Int,
    )

    data class V1GenerateRequest(
        var feature_id: Long?,
        var user_id: Long?,
        val message: String,
        val local_changes: Map<String, String>?,
        val start_locations: List<V1Location>?,
        val session: String?,
    )

    @POST
    @Path("/api/public/v1/generate")
    fun v1Generate(req: V1GenerateRequest): Response

    data class V1SummarizeRequest(
        var feature_id: Long?,
        var user_id: Long?,
        val diff: String,
    )

    @POST
    @Path("/api/public/v1/summarize")
    fun v1Summarize(req: V1SummarizeRequest): Response
}