package org.bismuth.auth.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import com.fasterxml.jackson.annotation.JsonView
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.JdbcType
import org.hibernate.annotations.UpdateTimestamp
import org.hibernate.dialect.PostgreSQLEnumJdbcType
import java.time.LocalDateTime

enum class SubscriptionType {
    INDIVIDUAL,
    PROFESSIONAL,
    TEAM,
    ENT,
}

@Entity
@Table(name = "subscriptions")
class SubscriptionEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<SubscriptionEntity> {
    }

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @UpdateTimestamp
    val updatedAt: LocalDateTime? = null

    @JsonIgnore
    var customerId: String? = null

    @JsonIgnore
    var subscriptionId: String? = null

    @Enumerated(EnumType.STRING)
    lateinit var type: SubscriptionType

    @JsonIgnore
    var expiresAt: LocalDateTime? = null

    var credits: Int = 0
}
