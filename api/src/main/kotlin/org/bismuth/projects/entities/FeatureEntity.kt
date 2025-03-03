package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonBackReference
import com.fasterxml.jackson.annotation.JsonIgnore
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime
import java.util.*

@Entity
@Table(name = "features")
class FeatureEntity() : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<FeatureEntity> {

    }

    lateinit var name: String

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null

    @ManyToOne(fetch = FetchType.LAZY)
    @JoinColumn(name = "projectId")
    @JsonBackReference
    lateinit var project: ProjectEntity

    var functionUUID: UUID? = null
    var deployedCommit: String? = null

    @OneToMany(mappedBy = "feature", orphanRemoval = true)
    @JsonIgnore
    val config: List<FeatureConfigEntity> = mutableListOf()
}