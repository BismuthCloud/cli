package org.bismuth.projects

import io.opentelemetry.instrumentation.annotations.SpanAttribute
import io.opentelemetry.instrumentation.annotations.WithSpan
import jakarta.enterprise.context.ApplicationScoped
import org.apache.commons.io.FileUtils
import org.eclipse.jgit.api.Git
import org.eclipse.jgit.api.errors.ConcurrentRefUpdateException
import org.eclipse.jgit.lib.*
import org.eclipse.jgit.patch.Patch
import org.eclipse.jgit.patch.PatchApplier
import org.eclipse.jgit.revwalk.RevTree
import org.eclipse.jgit.revwalk.RevWalk
import org.eclipse.jgit.storage.file.FileRepositoryBuilder
import org.eclipse.jgit.treewalk.TreeWalk
import org.eclipse.microprofile.config.inject.ConfigProperty
import org.jboss.logging.Logger
import java.io.File
import java.io.IOException
import java.nio.file.Paths
import java.security.MessageDigest
import kotlin.random.Random


@ApplicationScoped
class VCSServiceClient {
    private val LOG: Logger = Logger.getLogger(javaClass)

    @ConfigProperty(name = "vcs.projects-dir")
    lateinit var projectsDir: String

    companion object {
        const val NULL_FILE_PATCH_MARKER: String = "0000000"
        val COMMIT_AUTHOR =
            PersonIdent("Bismuth", "committer@app.bismuth.cloud")
    }

    fun createGitPatch(
        diffPatch: String, filePath: String, oldHash: String, newHash: String
    ): String {
        val stringBuilder = StringBuilder()
        stringBuilder.append("diff --git a/$filePath b/$filePath\n")
        if (oldHash == NULL_FILE_PATCH_MARKER) {
            stringBuilder.append("new file mode 100644\n")
        }
        if (newHash == NULL_FILE_PATCH_MARKER) {
            stringBuilder.append("deleted file mode 100644\n")
        }

        stringBuilder.append("index $oldHash..$newHash 100644\n")

        if (oldHash == NULL_FILE_PATCH_MARKER) {
            stringBuilder.append("--- /dev/null\n")
        } else {
            stringBuilder.append("--- a/$filePath\n")
        }

        if (newHash == NULL_FILE_PATCH_MARKER) {
            stringBuilder.append("+++ /dev/null\n")
        } else {
            stringBuilder.append("+++ b/$filePath\n")
        }

        stringBuilder.append(diffPatch)
        return stringBuilder.toString()
    }

    fun getRepoPath(projectHash: String): File {
        return File(Paths.get(projectsDir.replaceFirst("^~".toRegex(), System.getProperty("user.home")), projectHash).toString())
    }

    fun getRepo(projectHash: String): Repository {
        val repo = FileRepositoryBuilder().setGitDir(
            File(getRepoPath(projectHash), ".git")
        ).build()
        return repo
    }

    fun getTree(projectHash: String, branchName: String): RevTree {
        val repo = getRepo(projectHash)
        return getTree(repo, branchName)
    }

    fun getTree(repo: Repository, branchName: String): RevTree {
        val lastCommitId = repo.resolve(branchName)
        val commit = repo.parseCommit(lastCommitId)
        return commit.tree
    }

    @WithSpan
    fun listFiles(
        @SpanAttribute("project") projectHash: String,
        @SpanAttribute("branch") branchName: String,
    ): List<String> {
        val repo = getRepo(projectHash)
        return listFiles(repo, branchName)
    }

    fun listFiles(
        repo: Repository,
        branchName: String,
    ): List<String> {
        val tree = getTree(repo, branchName)
        val walk = TreeWalk(repo)
        walk.addTree(tree)
        walk.isRecursive = true
        val items: MutableList<String> = mutableListOf()
        while (walk.next()) {
            items.add(walk.pathString)
        }
        return items
    }

    @WithSpan
    fun getFileContents(
        @SpanAttribute("project") projectHash: String,
        @SpanAttribute("branch") branchName: String,
        @SpanAttribute("file") filePath: String
    ): String? {
        val repo = getRepo(projectHash)
        return getFileContents(repo, branchName, filePath)
    }

    fun getFileContents(
        repo: Repository,
        branchName: String,
        filePath: String
    ): String? {
        val tree = getTree(repo, branchName)
        val treeWalk = TreeWalk.forPath(repo, filePath, tree) ?: return null
        val objectId = treeWalk.getObjectId(0)
        return String(repo.open(objectId).bytes)
    }

    // N.B. Stacking multiple patches which modify the same file returns an error.
    @WithSpan
    fun applyGitPatch(
        @SpanAttribute("project") projectHash: String,
        tree: RevTree,
        patchString: String
    ): RevTree? {
        val repo = getRepo(projectHash)
        return applyGitPatch(repo, tree, patchString)
    }

    fun applyGitPatch(
        repo: Repository,
        tree: RevTree,
        patchString: String
    ): RevTree? {
        val patch = Patch()
        patch.parse(patchString.byteInputStream())
        if (patch.errors.isNotEmpty()) {
            LOG.info(patch.errors)
            return null
        }
        val odi = repo.newObjectInserter()
        val res = PatchApplier(repo, tree, odi).applyPatch(patch)
        if (res.errors.isNotEmpty()) {
            LOG.info(res.errors)
            return null
        }
        odi.flush()

        return RevWalk(repo).lookupTree(res.treeId)
    }

    @WithSpan
    fun commit(
        @SpanAttribute("project") projectHash: String,
        @SpanAttribute("branch") branchName: String,
        treeId: ObjectId,
        commitMessage: String
    ) {
        val repo = getRepo(projectHash)
        return commit(repo, branchName, treeId, commitMessage)
    }

    fun commit(
        repo: Repository,
        branchName: String,
        treeId: ObjectId,
        commitMessage: String,
        author: PersonIdent = COMMIT_AUTHOR,
    ) {
        // Commit without changing HEAD. Based on:
        // https://github.com/eclipse-jgit/jgit/blob/c1eba8abe04493ceb9b30c4f747fb0255c4f28e1/org.eclipse.jgit/src/org/eclipse/jgit/api/CommitCommand.java#L169
        // See https://jwiegley.github.io/git-from-the-bottom-up/1-Repository/4-how-trees-are-made.html for an explainer about all of this.
        if (!repo.repositoryState.canCommit()) {
            throw IOException("Repository in uncommitable state")
        }

        val commitBuilder = CommitBuilder()
        commitBuilder.author = author
        commitBuilder.committer = author
        commitBuilder.message = commitMessage
        commitBuilder.setParentId(repo.resolve(branchName))
        commitBuilder.setTreeId(treeId)

        val odi = repo.newObjectInserter()
        val commitId = odi.insert(commitBuilder)
        odi.flush()

        val revCommit = repo.parseCommit(commitId)
        val refUpdate = repo.updateRef("refs/heads/${branchName}")
        refUpdate.setNewObjectId(revCommit)
        refUpdate.setRefLogMessage("commit: ${revCommit.shortMessage}", false)
        refUpdate.setExpectedOldObjectId(commitBuilder.parentIds[0])
        val result = refUpdate.forceUpdate()
        if (result == RefUpdate.Result.REJECTED || result == RefUpdate.Result.LOCK_FAILURE) {
            throw ConcurrentRefUpdateException(
                "Could not lock ref",
                refUpdate.ref,
                result
            )
        }
    }


    @WithSpan
    fun createRepo(
        @SpanAttribute("project") projectHash: String,
    ) {
        val git = Git.init().setDirectory(getRepoPath(projectHash))
            .setInitialBranch("main").call()
        git.commit().setAllowEmpty(true).setMessage("Initialized")
            .setAuthor(COMMIT_AUTHOR).setCommitter(
                COMMIT_AUTHOR
            ).call()
        git.repository.config.setBoolean("http", null, "receivepack", true)
        git.repository.config.save()
    }

    @WithSpan
    fun createBranch(
        @SpanAttribute("project") projectHash: String,
        branchName: String
    ) {
        if (branchName == "main") {
            LOG.info("createBranch Output: Main branch skipping.")
            return
        }

        val repo = getRepo(projectHash)
        val git = Git(repo)
        git.branchCreate().setName(branchName).setStartPoint("main").call()
    }

    @WithSpan
    fun deleteRepo(
        @SpanAttribute("project") projectHash: String,
    ) {
        val path = getRepoPath(projectHash)
        FileUtils.deleteDirectory(path)
    }

    @WithSpan
    fun getHead(
        @SpanAttribute("project") projectHash: String,
        @SpanAttribute("branch") branchName: String
    ): String {
        val repo = getRepo(projectHash)
        return ObjectId.toString(repo.resolve(branchName))
    }

    fun createProjectHash(): String {
        val bytes = Random.nextBytes(16)
        val md = MessageDigest.getInstance("SHA-256")
        val digest = md.digest(bytes)
        return digest.fold("") { str, it -> str + "%02x".format(it) }
    }
}
