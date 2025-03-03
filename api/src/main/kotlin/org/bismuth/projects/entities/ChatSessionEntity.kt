package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonBackReference
import com.fasterxml.jackson.annotation.JsonProperty
import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.module.kotlin.readValue
import io.hypersistence.utils.hibernate.type.json.JsonBinaryType
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import java.time.LocalDateTime
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.Type
import org.hibernate.annotations.UpdateTimestamp

@Entity
@Table(name = "chat_sessions")
class ChatSessionEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<ChatSessionEntity> {
        private val objectMapper = ObjectMapper()
    }

    @JoinColumn(name = "featureId")
    @ManyToOne(fetch = FetchType.LAZY)
    @JsonBackReference
    lateinit var feature: FeatureEntity

    lateinit var origin: String

    var name: String? = null

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @Column(columnDefinition = "jsonb", nullable = false)
    @get:JsonProperty("context_storage")
    @set:JsonProperty("context_storage")
    @Type(JsonBinaryType::class)
    var contextStorage: String? = null
}
