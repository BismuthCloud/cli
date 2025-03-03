package org.bismuth.auth.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.bismuth.projects.entities.ChatMessageEntity
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime

@Entity
@Table(name = "api_keys")
class APIKeyEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<APIKeyEntity> {
    }

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    var createdAt: LocalDateTime? = null

    @JoinColumn(name = "userId")
    @ManyToOne(fetch = FetchType.EAGER)
    @JsonIgnore
    lateinit var user: UserEntity

    @JsonIgnore
    lateinit var token: String

    lateinit var description: String
}
