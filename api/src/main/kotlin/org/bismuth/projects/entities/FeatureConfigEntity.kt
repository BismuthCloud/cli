package org.bismuth.projects.entities

import com.fasterxml.jackson.annotation.JsonBackReference
import io.quarkus.hibernate.orm.panache.kotlin.PanacheEntity
import jakarta.persistence.*
import org.bismuth.BismuthPanacheCompanionInterface
import org.hibernate.annotations.CreationTimestamp
import org.hibernate.annotations.UpdateTimestamp
import java.time.LocalDateTime

@Entity
@Table(name = "feature_config")
class FeatureConfigEntity : PanacheEntity() {
    companion object : BismuthPanacheCompanionInterface<FeatureConfigEntity> {
    }

    @JoinColumn(name = "featureId")
    @ManyToOne(fetch = FetchType.LAZY)
    @JsonBackReference
    lateinit var feature: FeatureEntity

    var key: String = ""
    var value: String = ""

    @UpdateTimestamp
    private val updatedAt: LocalDateTime? = null

    @CreationTimestamp
    @Column(nullable = false, updatable = false)
    private var createdAt: LocalDateTime? = null
}