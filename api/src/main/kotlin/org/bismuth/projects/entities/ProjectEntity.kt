package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonIgnore
import com.fasterxml.jackson.annotation.JsonManagedReference
import io.hypersistence.utils.hibernate.type.json.JsonBinaryType
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.bismuth.auth.entities.OrganizationEntity
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.Type
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime

@Entity
@Table(name = "projects")
class ProjectEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<ProjectEntity> {

    }

    data class GHConfig(
        val bismuthInProgressLabel: String? = null,
        val bugScanBranch: String? = null,
        val bugScanPRLabel: Long? = null,
        val prReviewEnabled: Boolean? = null,
    )

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    lateinit var name: String
    lateinit var hash: String

    @ManyToOne(fetch = FetchType.LAZY)
    @JoinColumn(name = "organizationId")
    @JsonIgnore
    lateinit var organization: OrganizationEntity

    @OneToMany(mappedBy = "project", orphanRemoval = true)
    @JsonManagedReference
    val features: MutableList<FeatureEntity> = mutableListOf()

    @Column(name = "internalCloneToken")
    lateinit var cloneToken: String

    var hasPushed: Boolean = false
}