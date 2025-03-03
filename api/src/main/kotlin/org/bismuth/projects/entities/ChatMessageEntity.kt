package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonBackReference
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.bismuth.auth.entities.UserEntity
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime


@Entity
@Table(name = "chat_messages")
class ChatMessageEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<ChatMessageEntity> {

    }

    var isAI: Boolean = false
    var containsCode: Boolean = false

    lateinit var content: String

    @JoinColumn(name = "userId")
    @ManyToOne(fetch = FetchType.LAZY, optional = true)
    var user: UserEntity? = null

    var messageLLMContext: String? = null

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    var createdAt: LocalDateTime? = null

    var feedbackUpvote: Boolean? = null
    var feedback: String? = null

    var accepted: Boolean? = null

    @ManyToOne(fetch = FetchType.LAZY)
    @JoinColumn(name = "sessionId")
    @JsonBackReference
    lateinit var session: ChatSessionEntity

    var requestId: String? = null
}