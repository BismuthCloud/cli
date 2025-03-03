package org.bismuth.auth.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import com.fasterxml.jackson.annotation.JsonView
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.JdbcTypeCode
import org.hibernate.annotations.UpdateTimestamp
import org.hibernate.type.SqlTypes
import java.time.LocalDateTime

@Entity
@Table(name = "organizations")
class OrganizationEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<OrganizationEntity> {
    }

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    lateinit var name: String

    @ManyToMany(cascade = [CascadeType.ALL])
    @JoinTable(
        name = "organization_users",
        joinColumns = [JoinColumn(name = "orgId")],
        inverseJoinColumns = [JoinColumn(name = "userId")]
    )
    @JsonView(UserOrgViews.OrganizationView::class)
    val users: MutableList<UserEntity> = mutableListOf()

    @OneToOne
    @JoinColumn(name = "subscriptionId")
    lateinit var subscription: SubscriptionEntity

    @JdbcTypeCode(SqlTypes.JSON)
    var llmConfig: Map<String, String>? = null
}