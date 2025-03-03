package org.bismuth.projects.entities

import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import java.time.LocalDateTime
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp

@Entity
@Table(name = "generation_analysis")
class GenerationAnalysisEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<GenerationAnalysisEntity> {}

    @UpdateTimestamp private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @JoinColumn(name = "chatMessageId")
    @ManyToOne(fetch = FetchType.LAZY, optional = true)
    var chatMessage: ChatMessageEntity? = null

    lateinit var generation: String
    var mypy: String? = null
}
