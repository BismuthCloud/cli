package org.bismuth.auth.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import com.fasterxml.jackson.annotation.JsonView
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime

@Entity
@Table(name = "users")
class UserEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<UserEntity> {
    }

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @Column(nullable = false)
    lateinit var email: String

    @Column(nullable = false, unique = true)
    lateinit var username: String

    lateinit var name: String

    @ManyToMany(cascade = [CascadeType.ALL])
    @JoinTable(
        name = "organization_users",
        joinColumns = [JoinColumn(name = "userId")],
        inverseJoinColumns = [JoinColumn(name = "orgId")]
    )
    @JsonView(UserOrgViews.UserView::class)
    val organizations: MutableList<OrganizationEntity> = mutableListOf()

    // This is a user that hasn't registered yet, but has been invited to an org.
    @Column(nullable = false)
    @JsonIgnore
    var pending: Boolean = false
}