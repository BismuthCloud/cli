package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import com.fasterxml.jackson.annotation.JsonProperty
import io.hypersistence.utils.hibernate.type.json.JsonBinaryType
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import java.time.LocalDateTime
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.Type
import org.hibernate.annotations.UpdateTimestamp

@Entity
@Table(name = "generation_traces")
class GenerationTraceEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<GenerationTraceEntity> {}

    @UpdateTimestamp private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @JoinColumn(name = "chatMessageId")
    @ManyToOne(fetch = FetchType.LAZY)
    @JsonIgnore
    lateinit var chatMessage: ChatMessageEntity

    @Column(columnDefinition = "jsonb", nullable = false)
    @Type(JsonBinaryType::class)
    lateinit var state: Map<String, Any>
}
